//! Black-box tests: launch the real exe and drive it with window messages.
//! Run single-threaded: `cargo test --release -- --test-threads=1`.
#![cfg(windows)]

#[allow(dead_code)]
#[path = "../src/ids.rs"]
mod ids;

use ids::*;
use std::os::windows::io::AsRawHandle;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::ptr::null_mut;
use std::time::{Duration, Instant};
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::Threading::{GetProcessTimes, WaitForInputIdle};
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const EXE: &str = env!("CARGO_BIN_EXE_foxing");
const EDT1: i32 = 0x480; // Find dialog's text box
const MB_GETCHECK: u32 = WM_APP + 1;
const MB_GETOPEN: u32 = WM_APP + 2;
const MB_ISACTIVE: u32 = WM_APP + 3;
const VM_GETBG: u32 = WM_APP + 10;
const FL_GETCOUNT: u32 = WM_APP + 20;
const FL_ACTIVATE: u32 = WM_APP + 21;
const FL_GETSEL: u32 = WM_APP + 22;
const VK_MENU: usize = 0x12;
const VK_RIGHT: usize = 0x27;
const VK_DOWN: usize = 0x28;
const VK_RETURN: usize = 0x0D;
const VK_ESCAPE: usize = 0x1B;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("foxing-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn wait_for<T>(ms: u64, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + Duration::from_millis(ms);
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn text_of(h: HWND) -> String {
    unsafe {
        let len = SendMessageW(h, WM_GETTEXTLENGTH, 0, 0) as usize;
        let mut buf = vec![0u16; len + 1];
        let got = SendMessageW(h, WM_GETTEXT, buf.len(), buf.as_mut_ptr() as LPARAM) as usize;
        String::from_utf16_lossy(&buf[..got])
    }
}

struct Search {
    pid: u32,
    class: String,
    title: Option<String>,
    found: HWND,
}

unsafe extern "system" fn enum_cb(h: HWND, lp: LPARAM) -> BOOL {
    let s = &mut *(lp as *mut Search);
    let mut pid = 0;
    GetWindowThreadProcessId(h, &mut pid);
    if pid != s.pid || IsWindowVisible(h) == 0 {
        return 1;
    }
    let mut buf = [0u16; 256];
    let n = GetClassNameW(h, buf.as_mut_ptr(), 256) as usize;
    if String::from_utf16_lossy(&buf[..n]) != s.class {
        return 1;
    }
    if let Some(t) = &s.title {
        if &text_of(h) != t {
            return 1;
        }
    }
    s.found = h;
    0
}

fn find_window(pid: u32, class: &str, title: Option<&str>) -> Option<HWND> {
    let mut s = Search {
        pid,
        class: class.into(),
        title: title.map(Into::into),
        found: null_mut(),
    };
    unsafe { EnumWindows(Some(enum_cb), &mut s as *mut Search as LPARAM) };
    (!s.found.is_null()).then_some(s.found)
}

struct App {
    child: Child,
    hwnd: HWND,
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl App {
    fn launch(file: Option<&PathBuf>) -> App {
        // Every launch gets its own settings file so tests never see each other's (or
        // the user's) remembered choices.
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self::launch_with(file, &tmp(&format!("settings-{n}.ini")))
    }

    fn launch_with(file: Option<&PathBuf>, settings: &PathBuf) -> App {
        let mut cmd = Command::new(EXE);
        cmd.env("FOXING_SETTINGS", settings);
        if let Some(f) = file {
            cmd.arg(f);
        }
        let child = cmd.spawn().expect("spawn foxing");
        unsafe { WaitForInputIdle(child.as_raw_handle() as HANDLE, 5000) };
        let pid = child.id();
        let hwnd = wait_for(5000, || find_window(pid, "foxing", None)).expect("main window");
        App { child, hwnd }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn edit(&self) -> HWND {
        let h = unsafe { GetDlgItem(self.hwnd, IDC_EDIT as i32) };
        assert!(!h.is_null(), "edit control missing");
        h
    }

    fn title(&self) -> String {
        text_of(self.hwnd)
    }

    fn text(&self) -> String {
        text_of(self.edit())
    }

    /// Replaces all text the way typing would: marks modified and fires EN_CHANGE.
    fn type_text(&self, s: &str) {
        let w = wide(s);
        unsafe {
            SendMessageW(self.edit(), EM_SETSEL, 0, -1);
            SendMessageW(self.edit(), EM_REPLACESEL, 1, w.as_ptr() as LPARAM);
        }
    }

    fn cmd(&self, id: u16) {
        unsafe { SendMessageW(self.hwnd, WM_COMMAND, id as WPARAM, 0) };
    }

    fn sel(&self) -> (usize, usize) {
        let r = unsafe { SendMessageW(self.edit(), EM_GETSEL, 0, 0) } as usize;
        (r & 0xFFFF, (r >> 16) & 0xFFFF)
    }

    fn set_sel(&self, s: usize, e: usize) {
        unsafe { SendMessageW(self.edit(), EM_SETSEL, s, e as LPARAM) };
    }

    fn dialog(&self, title: &str) -> HWND {
        let pid = self.pid();
        wait_for(5000, || find_window(pid, "#32770", Some(title)))
            .unwrap_or_else(|| panic!("dialog {title:?} not found"))
    }

    fn status(&self) -> HWND {
        unsafe { GetDlgItem(self.hwnd, IDC_STATUS as i32) }
    }

    /// Text of a status bar part (the custom bar answers WM_GETTEXT with tab-joined parts).
    fn status_text(&self, part: usize) -> String {
        text_of(self.status())
            .split('\t')
            .nth(part)
            .unwrap_or_default()
            .to_owned()
    }

    fn menubar(&self) -> HWND {
        unsafe { GetDlgItem(self.hwnd, IDC_MENUBAR as i32) }
    }

    fn menu_checked(&self, id: u16) -> bool {
        unsafe { SendMessageW(self.menubar(), MB_GETCHECK, id as WPARAM, 0) != 0 }
    }

    fn menu_open(&self) -> isize {
        unsafe { SendMessageW(self.menubar(), MB_GETOPEN, 0, 0) }
    }

    fn menu_active(&self) -> bool {
        unsafe { SendMessageW(self.menubar(), MB_ISACTIVE, 0, 0) != 0 }
    }

    /// Posts keyboard input through Foxing's message loop, as typing would.
    fn post(&self, msg: u32, wp: usize, lp: isize) {
        unsafe { PostMessageW(self.edit(), msg, wp, lp) };
    }

    fn folder_view(&self) -> HWND {
        unsafe { GetDlgItem(self.hwnd, IDC_FOLDER as i32) }
    }

    fn folder_count(&self) -> usize {
        unsafe { SendMessageW(self.folder_view(), FL_GETCOUNT, 0, 0) as usize }
    }

    fn folder_sel(&self) -> isize {
        unsafe { SendMessageW(self.folder_view(), FL_GETSEL, 0, 0) }
    }

    fn folder_activate(&self, i: usize) {
        unsafe { SendMessageW(self.folder_view(), FL_ACTIVATE, i, 0) };
    }

    fn wait_title(&self, want: &str) {
        wait_for(5000, || (self.title() == want).then_some(()))
            .unwrap_or_else(|| panic!("title {want:?}, got {:?}", self.title()));
    }

    fn alive(&mut self) -> bool {
        self.child.try_wait().unwrap().is_none()
    }
}

#[test]
fn launch_blank() {
    let app = App::launch(None);
    assert_eq!(app.title(), "Untitled - Foxing");
    assert_eq!(app.text(), "");
}

#[test]
fn open_cli_arg() {
    let f = tmp("cli.txt");
    std::fs::write(&f, "hello\nworld ✓").unwrap();
    let app = App::launch(Some(&f));
    // Line endings are preserved as stored.
    assert_eq!(app.text(), "hello\nworld ✓");
    assert_eq!(app.title(), "cli.txt - Foxing");
}

#[test]
fn open_missing_file_starts_empty() {
    let f = tmp("does-not-exist.txt");
    let _ = std::fs::remove_file(&f);
    let app = App::launch(Some(&f));
    assert_eq!(app.text(), "");
    assert_eq!(app.title(), "does-not-exist.txt - Foxing");
}

#[test]
fn open_utf16() {
    let f = tmp("utf16.txt");
    let mut b = vec![0xFF, 0xFE];
    b.extend("wide ✓ text".encode_utf16().flat_map(|c| c.to_le_bytes()));
    std::fs::write(&f, b).unwrap();
    let app = App::launch(Some(&f));
    assert_eq!(app.text(), "wide ✓ text");
}

#[test]
fn large_file() {
    let f = tmp("large.txt");
    let line = "0123456789abcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxyz\r\n";
    let body = line.repeat(5 * 1024 * 1024 / line.len());
    std::fs::write(&f, &body).unwrap();
    let app = App::launch(Some(&f));
    let len = unsafe { SendMessageW(app.edit(), WM_GETTEXTLENGTH, 0, 0) } as usize;
    assert_eq!(len, body.len());
}

#[test]
fn dirty_title() {
    let app = App::launch(None);
    app.type_text("abc");
    assert_eq!(app.title(), "*Untitled - Foxing");
}

#[test]
fn save_existing() {
    let f = tmp("save.txt");
    std::fs::write(&f, "old").unwrap();
    let app = App::launch(Some(&f));
    app.type_text("new ✓\r\nline");
    assert_eq!(app.title(), "*save.txt - Foxing");
    app.cmd(ID_SAVE);
    assert_eq!(std::fs::read(&f).unwrap(), "new ✓\r\nline".as_bytes());
    assert_eq!(app.title(), "save.txt - Foxing");
}

#[test]
fn typing_undo_and_lf_preserved_on_save() {
    let f = tmp("lf.txt");
    std::fs::write(&f, "one\ntwo\n").unwrap();
    let app = App::launch(Some(&f));
    app.set_sel(0, 0);
    for c in "hi".chars() {
        unsafe { SendMessageW(app.edit(), WM_CHAR, c as WPARAM, 0) };
    }
    unsafe { SendMessageW(app.edit(), WM_CHAR, 0x0D, 0) }; // Enter
    assert_eq!(app.text(), "hi\none\ntwo\n");
    assert_eq!(app.title(), "*lf.txt - Foxing");
    unsafe { SendMessageW(app.edit(), WM_UNDO, 0, 0) };
    assert_eq!(app.text(), "hione\ntwo\n");
    unsafe { SendMessageW(app.edit(), WM_CHAR, 0x0D, 0) };
    app.cmd(ID_SAVE);
    // The file's LF endings survive; nothing is converted to CRLF.
    assert_eq!(std::fs::read(&f).unwrap(), b"hi\none\ntwo\n");
}

#[test]
fn crlf_file_keeps_crlf() {
    let f = tmp("crlf.txt");
    std::fs::write(&f, "a\r\nb").unwrap();
    let app = App::launch(Some(&f));
    // Pin the caret: a stray real click on the new window must not move it.
    app.set_sel(0, 0);
    unsafe { SendMessageW(app.edit(), WM_CHAR, 0x0D, 0) };
    app.cmd(ID_SAVE);
    assert_eq!(std::fs::read(&f).unwrap(), b"\r\na\r\nb");
}

#[test]
fn status_bar_shows_position_lines_and_eol() {
    let f = tmp("status-lf.txt");
    std::fs::write(&f, "ab\ncd").unwrap();
    let app = App::launch(Some(&f));
    assert_eq!(app.status_text(1), "Ln 1, Col 1");
    assert_eq!(app.status_text(2), "2 lines");
    assert_eq!(app.status_text(3), "Unix (LF)");
    assert_eq!(app.status_text(4), "UTF-8");
    for c in "xy".chars() {
        unsafe { SendMessageW(app.edit(), WM_CHAR, c as WPARAM, 0) };
    }
    assert_eq!(app.status_text(1), "Ln 1, Col 3");
    unsafe { SendMessageW(app.edit(), WM_CHAR, 0x0D, 0) };
    assert_eq!(app.status_text(1), "Ln 2, Col 1");
    assert_eq!(app.status_text(2), "3 lines");

    let f = tmp("status-crlf.txt");
    std::fs::write(&f, "a\r\nb").unwrap();
    let app = App::launch(Some(&f));
    assert_eq!(app.status_text(3), "Windows (CRLF)");
}

#[test]
fn status_bar_toggle() {
    let app = App::launch(None);
    let checked = || app.menu_checked(ID_STATUS_BAR);
    assert_ne!(unsafe { IsWindowVisible(app.status()) }, 0);
    assert!(checked());
    app.cmd(ID_STATUS_BAR);
    assert_eq!(unsafe { IsWindowVisible(app.status()) }, 0);
    assert!(!checked());
    app.cmd(ID_STATUS_BAR);
    assert_ne!(unsafe { IsWindowVisible(app.status()) }, 0);
    assert_eq!(app.status_text(1), "Ln 1, Col 1");
}

fn first_visible(app: &App) -> usize {
    unsafe { SendMessageW(app.edit(), EM_GETFIRSTVISIBLELINE, 0, 0) as usize }
}

fn client_size(h: HWND) -> (i32, i32) {
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    unsafe { GetClientRect(h, &mut rc) };
    (rc.right, rc.bottom)
}

fn click(h: HWND, x: i32, y: i32) {
    let lp = ((y as u32 as LPARAM) << 16) | (x as u32 & 0xFFFF) as LPARAM;
    unsafe {
        SendMessageW(h, WM_LBUTTONDOWN, 1, lp);
        SendMessageW(h, WM_LBUTTONUP, 0, lp);
    }
}

#[test]
fn custom_scrollbar_pages_drags_and_wheels() {
    let f = tmp("scroll.txt");
    let body: String = (0..500).map(|i| format!("line {i}\n")).collect();
    std::fs::write(&f, body).unwrap();
    let app = App::launch(Some(&f));
    let edit = app.edit();
    let (w, h) = client_size(edit);
    assert_eq!(first_visible(&app), 0);

    // Click the vertical track near the bottom: pages down.
    click(edit, w - 4, h - 40);
    let paged = first_visible(&app);
    assert!(paged > 10, "page down moved to {paged}");

    // Drag the thumb to the very bottom: last page.
    let lp_at = |y: i32| ((y as u32 as LPARAM) << 16) | ((w - 4) as u32 & 0xFFFF) as LPARAM;
    unsafe {
        // Grab whatever is under the pointer at the top after scrolling back up.
        SendMessageW(edit, WM_VSCROLL, SB_TOP as WPARAM, 0);
        SendMessageW(edit, WM_LBUTTONDOWN, 1, lp_at(5));
        SendMessageW(edit, WM_MOUSEMOVE, 1, lp_at(h * 10));
        SendMessageW(edit, WM_LBUTTONUP, 0, lp_at(h * 10));
    }
    let bottom = first_visible(&app);
    assert!(bottom > 400, "dragged to {bottom}");

    // Wheel up three notches scrolls back toward the top.
    unsafe { SendMessageW(edit, WM_MOUSEWHEEL, (120usize * 3) << 16, 0) };
    assert!(first_visible(&app) < bottom);

    // The caret never moved: scrolling isn't editing.
    assert_eq!(app.status_text(1), "Ln 1, Col 1");
}

#[test]
fn menu_alt_tap_and_arrows_run_a_command() {
    let app = App::launch(None);
    assert!(!app.menu_active());
    // Alt down + up: menu mode, nothing open.
    app.post(WM_SYSKEYDOWN, VK_MENU, 0x2000_0001);
    app.post(WM_SYSKEYUP, VK_MENU, 0xC000_0001u32 as isize);
    wait_for(2000, || app.menu_active().then_some(())).expect("menu mode");
    assert_eq!(app.menu_open(), -1);
    // Right, Right: Format. Down opens it on Word Wrap. Enter runs it.
    app.post(WM_KEYDOWN, VK_RIGHT, 0);
    app.post(WM_KEYDOWN, VK_RIGHT, 0);
    app.post(WM_KEYDOWN, VK_DOWN, 0);
    wait_for(2000, || (app.menu_open() == 2).then_some(())).expect("Format open");
    app.post(WM_KEYDOWN, VK_RETURN, 0);
    wait_for(2000, || app.menu_checked(ID_WRAP).then_some(())).expect("Word Wrap on");
    assert!(!app.menu_active());
}

#[test]
fn menu_mnemonics_and_escape() {
    let app = App::launch(None);
    // Alt+V opens View; S runs Status Bar (hides it).
    app.post(WM_SYSCHAR, 'v' as usize, 0x2000_0001);
    wait_for(2000, || (app.menu_open() == 3).then_some(())).expect("View open");
    app.post(WM_CHAR, 's' as usize, 0);
    wait_for(2000, || (!app.menu_checked(ID_STATUS_BAR)).then_some(())).expect("status toggled");
    assert_eq!(unsafe { IsWindowVisible(app.status()) }, 0);
    // Alt+F then Esc twice leaves menu mode without running anything.
    app.post(WM_SYSCHAR, 'f' as usize, 0x2000_0001);
    wait_for(2000, || (app.menu_open() == 0).then_some(())).expect("File open");
    app.post(WM_KEYDOWN, VK_ESCAPE, 0);
    app.post(WM_KEYDOWN, VK_ESCAPE, 0);
    wait_for(2000, || (!app.menu_active()).then_some(())).expect("menu closed");
    assert_eq!(app.title(), "Untitled - Foxing");
}

#[test]
fn menu_mouse_click_opens_and_closes() {
    let app = App::launch(None);
    click(app.menubar(), 8, 8);
    assert_eq!(app.menu_open(), 0, "File open");
    // Clicking the open title again closes it.
    click(app.menubar(), 8, 8);
    assert_eq!(app.menu_open(), -1);
    assert!(!app.menu_active());
}

#[test]
fn theme_menu_switches_light_and_dark() {
    let app = App::launch(None);
    let bg = || unsafe { SendMessageW(app.edit(), VM_GETBG, 0, 0) as u32 };
    // Starts on "System Theme" (light or dark depending on this machine).
    assert!(app.menu_checked(ID_THEME_SYSTEM));
    assert!(bg() == 0xFFFFFF || bg() == 0x1E1E1E);
    app.cmd(ID_THEME_DARK);
    assert_eq!(bg(), 0x1E1E1E);
    assert!(app.menu_checked(ID_THEME_DARK) && !app.menu_checked(ID_THEME_SYSTEM));
    app.cmd(ID_THEME_LIGHT);
    assert_eq!(bg(), 0xFFFFFF);
    assert!(app.menu_checked(ID_THEME_LIGHT) && !app.menu_checked(ID_THEME_DARK));
}

#[test]
fn settings_are_remembered_between_launches() {
    let ini = tmp("remember.ini");
    let _ = std::fs::remove_file(&ini);
    let mut app = App::launch_with(None, &ini);
    app.cmd(ID_THEME_DARK);
    app.cmd(ID_WRAP);
    app.cmd(ID_STATUS_BAR);
    unsafe { PostMessageW(app.hwnd, WM_CLOSE, 0, 0) };
    wait_for(5000, || (!app.alive()).then_some(())).expect("clean exit");
    let text = std::fs::read_to_string(&ini).unwrap();
    assert!(
        text.contains("theme=dark") && text.contains("wrap=true"),
        "{text}"
    );

    let app = App::launch_with(None, &ini);
    assert!(app.menu_checked(ID_THEME_DARK));
    assert!(app.menu_checked(ID_WRAP));
    assert!(!app.menu_checked(ID_STATUS_BAR));
    assert_eq!(unsafe { IsWindowVisible(app.status()) }, 0);
    assert_eq!(
        unsafe { SendMessageW(app.edit(), VM_GETBG, 0, 0) } as u32,
        0x1E1E1E
    );
}

/// A fresh folder: two text files, plus things the sidebar must skip.
fn sample_folder(name: &str) -> PathBuf {
    let d = tmp(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("sub")).unwrap();
    std::fs::write(d.join("a.txt"), "alpha").unwrap();
    std::fs::write(d.join("b.md"), "beta").unwrap();
    std::fs::write(d.join("image.png"), [0u8, 1, 2]).unwrap();
    std::fs::write(d.join("sub").join("inner.txt"), "x").unwrap();
    d
}

#[test]
fn folder_from_command_line_lists_text_files() {
    let d = sample_folder("folder-cli");
    let app = App::launch(Some(&d));
    assert_eq!(app.folder_count(), 2);
    assert_eq!(text_of(app.folder_view()), "a.txt\nb.md");
    assert_ne!(unsafe { IsWindowVisible(app.folder_view()) }, 0);
    assert_eq!(app.title(), "Untitled - Foxing");
    assert_eq!(app.folder_sel(), -1);
}

#[test]
fn folder_switching_files_prompts_for_unsaved_changes() {
    let d = sample_folder("folder-switch");
    let app = App::launch(Some(&d));
    app.folder_activate(0);
    app.wait_title("a.txt - Foxing");
    assert_eq!(app.text(), "alpha");
    assert_eq!(app.folder_sel(), 0);
    app.set_sel(0, 0);
    unsafe { SendMessageW(app.edit(), WM_CHAR, 'x' as WPARAM, 0) };
    // Switching away from unsaved changes asks; "Don't Save" switches.
    app.folder_activate(1);
    let dlg = app.dialog("Foxing");
    unsafe { PostMessageW(dlg, WM_COMMAND, IDNO as WPARAM, 0) };
    app.wait_title("b.md - Foxing");
    assert_eq!(app.text(), "beta");
    assert_eq!(app.folder_sel(), 1);
    assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "alpha");
}

#[test]
fn folder_new_file_creates_and_opens_it() {
    let d = sample_folder("folder-new");
    let app = App::launch(Some(&d));
    app.cmd(ID_NEW_IN_FOLDER);
    app.wait_title("Untitled.txt - Foxing");
    assert!(d.join("Untitled.txt").exists());
    assert_eq!(app.folder_count(), 3);
    assert_eq!(app.folder_sel(), 2, "a.txt, b.md, Untitled.txt");
}

#[test]
fn folder_is_remembered_and_can_be_closed() {
    let d = sample_folder("folder-remember");
    let ini = tmp("folder-remember.ini");
    let _ = std::fs::remove_file(&ini);
    let mut app = App::launch_with(Some(&d), &ini);
    unsafe { PostMessageW(app.hwnd, WM_CLOSE, 0, 0) };
    wait_for(5000, || (!app.alive()).then_some(())).expect("clean exit");

    let app = App::launch_with(None, &ini);
    assert_eq!(app.folder_count(), 2, "last folder reopened");
    app.cmd(ID_CLOSE_FOLDER);
    assert_eq!(unsafe { IsWindowVisible(app.folder_view()) }, 0);
    assert!(!std::fs::read_to_string(&ini)
        .unwrap()
        .contains("last_folder"));
}

/// Runs a Windows PowerShell snippet with the .NET UI Automation client loaded, as a
/// screen reader would see Foxing. `H` in the script is replaced by `hwnd`.
fn uia(hwnd: HWND, script: &str) -> String {
    let script = format!(
        "Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes; \
         $A = [System.Windows.Automation.AutomationElement]; \
         $T = [System.Windows.Automation.TreeScope]; \
         $C = [System.Windows.Automation.Condition]::TrueCondition; \
         $e = $A::FromHandle([IntPtr]{}); {}",
        hwnd as isize, script
    );
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .expect("run powershell");
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert!(
        out.status.success(),
        "uia script failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    text
}

const NAMES: &str = "($e.FindAll($T::Children, $C) | % { $_.Current.Name + ':' + $_.Current.ControlType.ProgrammaticName }) -join ';'";

#[test]
fn uia_menu_bar_lists_menus_and_runs_items() {
    let app = App::launch(None);
    assert_eq!(
        uia(app.menubar(), NAMES),
        "File:ControlType.MenuItem;Edit:ControlType.MenuItem;Format:ControlType.MenuItem;View:ControlType.MenuItem"
    );
    // Expand Format, then toggle Word Wrap, as a screen reader user would.
    uia(
        app.menubar(),
        "$f = $e.FindAll($T::Children, $C)[2]; \
         $f.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand(); \
         $c = New-Object System.Windows.Automation.PropertyCondition($A::NameProperty, 'Word Wrap'); \
         $w = $f.FindFirst($T::Children, $c); \
         $w.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Toggle()",
    );
    wait_for(5000, || app.menu_checked(ID_WRAP).then_some(())).expect("Word Wrap toggled via UIA");
}

#[test]
fn uia_folder_list_selects_files() {
    let d = sample_folder("folder-uia");
    let app = App::launch(Some(&d));
    assert_eq!(
        uia(app.folder_view(), NAMES),
        "a.txt:ControlType.ListItem;b.md:ControlType.ListItem"
    );
    uia(
        app.folder_view(),
        "$e.FindAll($T::Children, $C)[1].GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()",
    );
    app.wait_title("b.md - Foxing");
}

#[test]
fn uia_status_bar_and_document() {
    let f = tmp("uia-doc.txt");
    std::fs::write(&f, "hello\nworld").unwrap();
    let app = App::launch(Some(&f));
    let status = uia(app.status(), NAMES);
    assert!(status.contains("Ln 1, Col 1:ControlType.Text"), "{status}");
    assert!(status.contains("Unix (LF):ControlType.Text"), "{status}");
    let doc = uia(
        app.edit(),
        "$e.Current.ControlType.ProgrammaticName + '|' + $e.Current.Name + '|' + \
         $e.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value",
    );
    assert_eq!(
        doc.replace("\r\n", "\n"),
        "ControlType.Document|Text editor|hello\nworld"
    );
}

#[test]
fn word_wrap_toggle() {
    let app = App::launch(None);
    app.type_text("some text that is here");
    app.set_sel(5, 9);
    let checked = || app.menu_checked(ID_WRAP);
    assert!(!checked());

    app.cmd(ID_WRAP);
    assert_eq!(app.text(), "some text that is here");
    assert_eq!(app.sel(), (5, 9));
    assert_eq!(app.title(), "*Untitled - Foxing");
    assert!(checked());

    app.cmd(ID_WRAP);
    assert!(!checked());
    assert_eq!(app.sel(), (5, 9));
}

#[test]
fn close_prompt_cancel() {
    let mut app = App::launch(None);
    app.type_text("unsaved");
    unsafe { PostMessageW(app.hwnd, WM_CLOSE, 0, 0) };
    let dlg = app.dialog("Foxing");
    unsafe { PostMessageW(dlg, WM_COMMAND, IDCANCEL as WPARAM, 0) };
    wait_for(2000, || unsafe { (IsWindow(dlg) == 0).then_some(()) }).expect("prompt closed");
    assert!(app.alive());
    assert_eq!(app.text(), "unsaved");
}

#[test]
fn close_prompt_dont_save() {
    let f = tmp("dontsave.txt");
    std::fs::write(&f, "original").unwrap();
    let mut app = App::launch(Some(&f));
    app.type_text("changed");
    unsafe { PostMessageW(app.hwnd, WM_CLOSE, 0, 0) };
    let dlg = app.dialog("Foxing");
    unsafe { PostMessageW(dlg, WM_COMMAND, IDNO as WPARAM, 0) };
    wait_for(5000, || (!app.alive()).then_some(())).expect("process exits");
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "original");
}

#[test]
fn close_clean_exits_without_prompt() {
    let mut app = App::launch(None);
    unsafe { PostMessageW(app.hwnd, WM_CLOSE, 0, 0) };
    wait_for(5000, || (!app.alive()).then_some(())).expect("process exits");
}

#[test]
fn find_next() {
    let app = App::launch(None);
    app.type_text("alpha beta Alpha gamma alpha");
    app.set_sel(0, 0);
    unsafe { PostMessageW(app.hwnd, WM_COMMAND, ID_FIND as WPARAM, 0) };
    let dlg = app.dialog("Find");
    unsafe {
        let edit = GetDlgItem(dlg, EDT1);
        let w = wide("ALPHA");
        SendMessageW(edit, WM_SETTEXT, 0, w.as_ptr() as LPARAM);
        let btn = GetDlgItem(dlg, IDOK);
        PostMessageW(dlg, WM_COMMAND, IDOK as WPARAM, btn as LPARAM);
    }
    wait_for(2000, || (app.sel() == (0, 5)).then_some(())).expect("first match selected");

    app.cmd(ID_FIND_NEXT);
    assert_eq!(app.sel(), (11, 16));
    app.cmd(ID_FIND_NEXT);
    assert_eq!(app.sel(), (23, 28));
    app.cmd(ID_FIND_NEXT); // no more matches: selection stays
    assert_eq!(app.sel(), (23, 28));
}

fn cpu_time(child: &Child) -> Duration {
    let mut t = [FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    }; 4];
    let [c, e, k, u] = &mut t;
    unsafe { GetProcessTimes(child.as_raw_handle() as HANDLE, c, e, k, u) };
    let ticks = |f: &FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
    Duration::from_nanos((ticks(&t[2]) + ticks(&t[3])) * 100)
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

/// Gate on Foxing's own CPU time to first visible window: wall-clock is dominated by
/// machine-level process-creation overhead (AV/EDR), reported alongside a baseline.
#[test]
fn launch_time() {
    let (mut cpu, mut wall) = (vec![], vec![]);
    for _ in 0..5 {
        let start = Instant::now();
        let child = Command::new(EXE)
            .env("FOXING_SETTINGS", tmp("settings-launch-time.ini"))
            .spawn()
            .unwrap();
        let pid = child.id();
        let hwnd = wait_for(10000, || find_window(pid, "foxing", None)).expect("main window");
        wall.push(start.elapsed());
        cpu.push(cpu_time(&child));
        drop(App { child, hwnd });
    }
    let baseline = median(
        (0..5)
            .map(|_| {
                let start = Instant::now();
                Command::new("cmd.exe")
                    .args(["/c", "exit"])
                    .status()
                    .unwrap();
                start.elapsed()
            })
            .collect(),
    );
    let (cpu, wall) = (median(cpu), median(wall));
    println!("foxing: cpu {cpu:?}, wall {wall:?}; baseline cmd.exe wall {baseline:?}");
    // Timer granularity is 15.6 ms and a bare `cmd /c exit` costs 0-47 ms here.
    assert!(cpu < Duration::from_millis(100), "startup CPU {cpu:?}");
}
