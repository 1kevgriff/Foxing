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
        let mut cmd = Command::new(EXE);
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
    assert_eq!(app.text(), "hello\r\nworld ✓");
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
fn word_wrap_toggle() {
    let app = App::launch(None);
    app.type_text("some text that is here");
    app.set_sel(5, 9);
    let old = app.edit();
    let checked =
        || unsafe { GetMenuState(GetMenu(app.hwnd), ID_WRAP as u32, MF_BYCOMMAND) } & MF_CHECKED;
    assert_eq!(checked(), 0);

    app.cmd(ID_WRAP);
    assert_ne!(app.edit(), old, "edit control should be recreated");
    assert_eq!(app.text(), "some text that is here");
    assert_eq!(app.sel(), (5, 9));
    assert_eq!(app.title(), "*Untitled - Foxing");
    assert_ne!(checked(), 0);

    app.cmd(ID_WRAP);
    assert_eq!(checked(), 0);
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
        let child = Command::new(EXE).spawn().unwrap();
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
