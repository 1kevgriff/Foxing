//! Win32 shell: windows, menus, dialogs. Document and text logic live in the library.

use crate::ids::*;
use crate::menuview;
use crate::statusview;
use crate::textview;
use foxing::document::Document;
use foxing::ui::menu::{Menu, MenuItem, MenuKey};
use foxing::ui::Theme;
use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows_sys::Win32::UI::Controls::Dialogs::*;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, VK_DOWN, VK_ESCAPE, VK_F10, VK_LEFT, VK_MENU, VK_RETURN, VK_RIGHT,
    VK_SHIFT, VK_UP,
};
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const FIND_BUF_LEN: usize = 256;
/// Status bar parts: spacer, Ln/Col, line count, line ending, encoding.
const STATUS_PARTS: usize = 5;
/// Widths (96-DPI pixels) of the fixed parts, right to left after the spacer.
static STATUS_WIDTHS: [i32; STATUS_PARTS - 1] = [150, 140, 120, 80];

struct App {
    main: Cell<HWND>,
    edit: Cell<HWND>,
    font: Cell<HFONT>,
    doc: RefCell<Document>,
    wrap: Cell<bool>,
    find_dlg: Cell<HWND>,
    find_msg: Cell<u32>,
    find: Cell<*mut FINDREPLACEW>,
    status: Cell<HWND>,
    menubar: Cell<HWND>,
    /// ID_THEME_SYSTEM, ID_THEME_LIGHT, or ID_THEME_DARK.
    theme_choice: Cell<u16>,
}

thread_local! {
    static APP: App = const { App {
        main: Cell::new(null_mut()),
        edit: Cell::new(null_mut()),
        font: Cell::new(null_mut()),
        doc: RefCell::new(Document::new()),
        wrap: Cell::new(false),
        find_dlg: Cell::new(null_mut()),
        find_msg: Cell::new(0),
        find: Cell::new(null_mut()),
        status: Cell::new(null_mut()),
        menubar: Cell::new(null_mut()),
        theme_choice: Cell::new(ID_THEME_SYSTEM),
    } };
}

fn app<R>(f: impl FnOnce(&App) -> R) -> R {
    APP.with(f)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn loword(v: usize) -> u16 {
    v as u16
}

fn hiword(v: usize) -> u16 {
    (v >> 16) as u16
}

unsafe fn set_sel(edit: HWND, s: usize, e: usize) {
    SendMessageW(edit, EM_SETSEL, s, e as LPARAM);
    SendMessageW(edit, EM_SCROLLCARET, 0, 0);
}

unsafe fn make_font(dpi: u32) -> HFONT {
    CreateFontW(
        -((11 * dpi as i32 + 36) / 72),
        0,
        0,
        0,
        FW_NORMAL as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        CLEARTYPE_QUALITY as u32,
        (FIXED_PITCH | FF_MODERN) as u32,
        w!("Consolas"),
    )
}

unsafe fn create_edit(parent: HWND) -> HWND {
    let edit = textview::create(parent, IDC_EDIT);
    SendMessageW(edit, WM_SETFONT, app(|a| a.font.get()) as WPARAM, 1);
    let mut rc: RECT = zeroed();
    GetClientRect(parent, &mut rc);
    MoveWindow(edit, 0, 0, rc.right, rc.bottom, 1);
    edit
}

fn doc_name() -> String {
    app(|a| a.doc.borrow().name())
}

unsafe fn update_title() {
    let title = wide(&app(|a| a.doc.borrow().title()));
    SetWindowTextW(app(|a| a.main.get()), title.as_ptr());
}

unsafe fn set_doc(doc: Document) {
    app(|a| *a.doc.borrow_mut() = doc);
    update_title();
    update_status();
}

/// 12345678 -> "12,345,678".
fn group(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

unsafe fn update_status() {
    let (status, edit) = app(|a| (a.status.get(), a.edit.get()));
    if !status_shown(status) {
        return;
    }
    let parts = textview::peek(edit, |ed| {
        let (line, col) = ed.caret_line_col();
        let lines = ed.buffer().line_count();
        [
            String::new(),
            format!("Ln {}, Col {}", group(line), group(col)),
            format!(
                "{} {}",
                group(lines),
                if lines == 1 { "line" } else { "lines" }
            ),
            if ed.eol() == "\r\n" {
                "Windows (CRLF)"
            } else {
                "Unix (LF)"
            }
            .to_owned(),
            "UTF-8".to_owned(),
        ]
    });
    statusview::set_texts(status, &parts);
}

/// The status bar's own visibility (not its parent's, which is hidden during startup).
unsafe fn status_shown(status: HWND) -> bool {
    !status.is_null() && GetWindowLongW(status, GWL_STYLE) as u32 & WS_VISIBLE != 0
}

/// Menu bar on top, status bar (if shown) at the bottom, text view in between.
unsafe fn layout_children(hwnd: HWND) {
    let (edit, status, bar) = app(|a| (a.edit.get(), a.status.get(), a.menubar.get()));
    let mut rc: RECT = zeroed();
    GetClientRect(hwnd, &mut rc);
    let top = if bar.is_null() {
        0
    } else {
        menuview::height(bar)
    };
    if !bar.is_null() {
        MoveWindow(bar, 0, 0, rc.right, top, 1);
    }
    let mut bottom = rc.bottom;
    if status_shown(status) {
        let h = statusview::height(status);
        bottom -= h;
        MoveWindow(status, 0, bottom.max(0), rc.right, h, 1);
    }
    MoveWindow(edit, 0, top, rc.right, (bottom - top).max(0), 1);
}

/// Windows "app mode" setting: true when apps should be dark.
unsafe fn system_prefers_dark() -> bool {
    let mut value: u32 = 1;
    let mut size = size_of::<u32>() as u32;
    let r = RegGetValueW(
        HKEY_CURRENT_USER,
        w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
        w!("AppsUseLightTheme"),
        RRF_RT_REG_DWORD,
        null_mut(),
        &mut value as *mut u32 as *mut _,
        &mut size,
    );
    r == ERROR_SUCCESS && value == 0
}

/// Resolves the theme choice and applies it to every component and the title bar.
unsafe fn apply_theme() {
    let (main, edit, status, bar, choice) = app(|a| {
        (
            a.main.get(),
            a.edit.get(),
            a.status.get(),
            a.menubar.get(),
            a.theme_choice.get(),
        )
    });
    let dark = match choice {
        ID_THEME_DARK => true,
        ID_THEME_LIGHT => false,
        _ => system_prefers_dark(),
    };
    let theme = if dark { Theme::DARK } else { Theme::LIGHT };
    textview::set_theme(edit, theme);
    statusview::set_theme(status, theme);
    menuview::set_theme(bar, theme);
    let on: i32 = dark as i32; // Win32 BOOL
    DwmSetWindowAttribute(
        main,
        DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
        &on as *const i32 as *const _,
        size_of::<i32>() as u32,
    );
    for id in [ID_THEME_SYSTEM, ID_THEME_LIGHT, ID_THEME_DARK] {
        menuview::set_checked(bar, id, id == choice);
    }
}

unsafe fn toggle_status_bar() {
    let (main, status) = app(|a| (a.main.get(), a.status.get()));
    let show = !status_shown(status);
    ShowWindow(status, if show { SW_SHOW } else { SW_HIDE });
    menuview::set_checked(app(|a| a.menubar.get()), ID_STATUS_BAR, show);
    layout_children(main);
    if show {
        update_status();
    }
}

unsafe fn error_box(msg: &str) {
    let m = wide(msg);
    MessageBoxW(
        app(|a| a.main.get()),
        m.as_ptr(),
        w!("Foxing"),
        MB_OK | MB_ICONERROR,
    );
}

unsafe fn load_file(path: PathBuf) {
    let wait = SetCursor(LoadCursorW(null_mut(), IDC_WAIT));
    let opened = Document::open(path.clone());
    SetCursor(wait);
    match opened {
        Ok((doc, body)) => {
            textview::set_buffer(app(|a| a.edit.get()), body);
            set_doc(doc);
        }
        Err(e) => error_box(&format!("Cannot open {}:\n{e}", path.display())),
    }
}

unsafe fn save_to(path: PathBuf) -> bool {
    let edit = app(|a| a.edit.get());
    let wait = SetCursor(LoadCursorW(null_mut(), IDC_WAIT));
    let saved = textview::with(edit, |ed| {
        app(|a| a.doc.borrow_mut().save_as(path.clone(), ed.buffer()))
    });
    SetCursor(wait);
    match saved {
        Ok(()) => {
            update_title();
            true
        }
        Err(e) => {
            error_box(&format!("Cannot save {}:\n{e}", path.display()));
            false
        }
    }
}

unsafe fn file_dialog(save: bool) -> Option<PathBuf> {
    let mut buf = vec![0u16; 32768];
    if save {
        let name: Vec<u16> = doc_name().encode_utf16().collect();
        buf[..name.len()].copy_from_slice(&name);
    }
    let filter: Vec<u16> = "Text Documents (*.txt)\0*.txt\0All Files (*.*)\0*.*\0\0"
        .encode_utf16()
        .collect();
    let mut ofn: OPENFILENAMEW = zeroed();
    ofn.lStructSize = size_of::<OPENFILENAMEW>() as u32;
    ofn.hwndOwner = app(|a| a.main.get());
    ofn.lpstrFilter = filter.as_ptr();
    ofn.lpstrFile = buf.as_mut_ptr();
    ofn.nMaxFile = buf.len() as u32;
    ofn.lpstrDefExt = w!("txt");
    ofn.Flags = OFN_EXPLORER | OFN_PATHMUSTEXIST;
    let ok = if save {
        ofn.Flags |= OFN_OVERWRITEPROMPT;
        GetSaveFileNameW(&mut ofn)
    } else {
        ofn.Flags |= OFN_FILEMUSTEXIST;
        GetOpenFileNameW(&mut ofn)
    };
    if ok == 0 {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(PathBuf::from(OsString::from_wide(&buf[..len])))
}

unsafe fn save_as() -> bool {
    match file_dialog(true) {
        Some(p) => save_to(p),
        None => false,
    }
}

unsafe fn save() -> bool {
    match app(|a| a.doc.borrow().path().map(Path::to_path_buf)) {
        Some(p) => save_to(p),
        None => save_as(),
    }
}

/// Returns true if it's OK to discard the current document.
unsafe fn confirm_discard() -> bool {
    if !app(|a| a.doc.borrow().is_dirty()) {
        return true;
    }
    let msg = wide(&format!("Do you want to save changes to {}?", doc_name()));
    match MessageBoxW(
        app(|a| a.main.get()),
        msg.as_ptr(),
        w!("Foxing"),
        MB_YESNOCANCEL | MB_ICONWARNING,
    ) {
        IDYES => save(),
        IDNO => true,
        _ => false,
    }
}

unsafe fn toggle_wrap() {
    let edit = app(|a| a.edit.get());
    let wrap = !app(|a| a.wrap.get());
    app(|a| a.wrap.set(wrap));
    textview::set_wrap(edit, wrap);
    menuview::set_checked(app(|a| a.menubar.get()), ID_WRAP, wrap);
    SetFocus(edit);
}

unsafe fn find_state() -> *mut FINDREPLACEW {
    let fr = app(|a| a.find.get());
    if !fr.is_null() {
        return fr;
    }
    let buf: &'static mut [u16] = Box::leak(vec![0u16; FIND_BUF_LEN].into_boxed_slice());
    let mut st: FINDREPLACEW = zeroed();
    st.lStructSize = size_of::<FINDREPLACEW>() as u32;
    st.hwndOwner = app(|a| a.main.get());
    st.Flags = FR_DOWN;
    st.lpstrFindWhat = buf.as_mut_ptr();
    st.wFindWhatLen = FIND_BUF_LEN as u16;
    let fr = Box::into_raw(Box::new(st));
    app(|a| a.find.set(fr));
    fr
}

unsafe fn show_find() {
    let dlg = app(|a| a.find_dlg.get());
    if !dlg.is_null() {
        SetFocus(dlg);
        return;
    }
    let dlg = FindTextW(find_state());
    app(|a| a.find_dlg.set(dlg));
}

unsafe fn needle(fr: *const FINDREPLACEW) -> Vec<u16> {
    let buf = std::slice::from_raw_parts((*fr).lpstrFindWhat, FIND_BUF_LEN);
    let len = buf.iter().position(|&c| c == 0).unwrap_or(FIND_BUF_LEN);
    buf[..len].to_vec()
}

unsafe fn find_next() {
    let fr = app(|a| a.find.get());
    if fr.is_null() || needle(fr).is_empty() {
        show_find();
        return;
    }
    let needle = String::from_utf16_lossy(&needle(fr));
    let down = (*fr).Flags & FR_DOWN != 0;
    let case = (*fr).Flags & FR_MATCHCASE != 0;
    let wait = SetCursor(LoadCursorW(null_mut(), IDC_WAIT));
    let found = textview::with(app(|a| a.edit.get()), |ed| ed.find(&needle, down, case));
    SetCursor(wait);
    if !found {
        MessageBeep(MB_OK);
    }
}

fn menus() -> Vec<Menu> {
    let it = MenuItem::new;
    let sep = MenuItem::separator;
    let mut status_bar = it("&Status Bar", "", ID_STATUS_BAR);
    status_bar.checked = true;
    vec![
        Menu::new(
            "&File",
            vec![
                it("&New", "Ctrl+N", ID_NEW),
                it("&Open...", "Ctrl+O", ID_OPEN),
                it("&Save", "Ctrl+S", ID_SAVE),
                it("Save &As...", "Ctrl+Shift+S", ID_SAVE_AS),
                sep(),
                it("E&xit", "", ID_EXIT),
            ],
        ),
        Menu::new(
            "&Edit",
            vec![
                it("&Undo", "Ctrl+Z", ID_UNDO),
                sep(),
                it("Cu&t", "Ctrl+X", ID_CUT),
                it("&Copy", "Ctrl+C", ID_COPY),
                it("&Paste", "Ctrl+V", ID_PASTE),
                sep(),
                it("&Find...", "Ctrl+F", ID_FIND),
                it("Find &Next", "F3", ID_FIND_NEXT),
                sep(),
                it("Select &All", "Ctrl+A", ID_SELECT_ALL),
            ],
        ),
        Menu::new("F&ormat", vec![it("&Word Wrap", "", ID_WRAP)]),
        Menu::new(
            "&View",
            vec![
                status_bar,
                sep(),
                it("S&ystem Theme", "", ID_THEME_SYSTEM),
                it("&Light Theme", "", ID_THEME_LIGHT),
                it("&Dark Theme", "", ID_THEME_DARK),
            ],
        ),
    ]
}

unsafe fn on_command(id: u16, code: u16) {
    let (main, edit) = app(|a| (a.main.get(), a.edit.get()));
    match id {
        IDC_EDIT => {
            if code == EN_CHANGE as u16 && app(|a| a.doc.borrow_mut().mark_dirty()) {
                update_title();
            }
            if code == EN_CHANGE as u16 || code == VN_CARET {
                update_status();
            }
        }
        ID_NEW => {
            if confirm_discard() {
                SetWindowTextW(edit, w!(""));
                set_doc(Document::new());
            }
        }
        ID_OPEN => {
            if confirm_discard() {
                if let Some(p) = file_dialog(false) {
                    load_file(p);
                }
            }
        }
        ID_SAVE => {
            save();
        }
        ID_SAVE_AS => {
            save_as();
        }
        ID_EXIT => {
            SendMessageW(main, WM_CLOSE, 0, 0);
        }
        ID_UNDO => {
            SendMessageW(edit, WM_UNDO, 0, 0);
        }
        ID_CUT => {
            SendMessageW(edit, WM_CUT, 0, 0);
        }
        ID_COPY => {
            SendMessageW(edit, WM_COPY, 0, 0);
        }
        ID_PASTE => {
            SendMessageW(edit, WM_PASTE, 0, 0);
        }
        ID_SELECT_ALL => set_sel(edit, 0, usize::MAX),
        ID_FIND => show_find(),
        ID_FIND_NEXT => find_next(),
        ID_WRAP => toggle_wrap(),
        ID_STATUS_BAR => toggle_status_bar(),
        ID_THEME_SYSTEM | ID_THEME_LIGHT | ID_THEME_DARK => {
            app(|a| a.theme_choice.set(id));
            apply_theme();
        }
        _ => {}
    }
}

unsafe fn on_drop(hdrop: HDROP) {
    let len = DragQueryFileW(hdrop, 0, null_mut(), 0);
    let mut buf = vec![0u16; len as usize + 1];
    DragQueryFileW(hdrop, 0, buf.as_mut_ptr(), buf.len() as u32);
    DragFinish(hdrop);
    buf.truncate(len as usize);
    if confirm_discard() {
        load_file(PathBuf::from(OsString::from_wide(&buf)));
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg != 0 && msg == app(|a| a.find_msg.get()) {
        let fr = lp as *const FINDREPLACEW;
        if (*fr).Flags & FR_DIALOGTERM != 0 {
            app(|a| a.find_dlg.set(null_mut()));
        } else if (*fr).Flags & FR_FINDNEXT != 0 {
            find_next();
        }
        return 0;
    }
    match msg {
        WM_CREATE => {
            app(|a| {
                a.main.set(hwnd);
                a.font.set(make_font(GetDpiForWindow(hwnd)));
            });
            let edit = create_edit(hwnd);
            let status = statusview::create(hwnd, IDC_STATUS, &STATUS_WIDTHS);
            let bar = menuview::create(hwnd, IDC_MENUBAR, menus());
            app(|a| {
                a.edit.set(edit);
                a.status.set(status);
                a.menubar.set(bar);
            });
            layout_children(hwnd);
            update_status();
            // Before the window is shown, so a dark start never flashes light.
            apply_theme();
            0
        }
        WM_SIZE => {
            layout_children(hwnd);
            0
        }
        WM_SETFOCUS => {
            SetFocus(app(|a| a.edit.get()));
            0
        }
        WM_COMMAND => {
            on_command(loword(wp), hiword(wp));
            0
        }
        WM_DROPFILES => {
            on_drop(wp as HDROP);
            0
        }
        WM_DPICHANGED => {
            let rc = &*(lp as *const RECT);
            let font = make_font(hiword(wp) as u32);
            let old = app(|a| a.font.replace(font));
            SendMessageW(app(|a| a.edit.get()), WM_SETFONT, font as WPARAM, 1);
            statusview::set_dpi(app(|a| a.status.get()), hiword(wp) as u32);
            menuview::set_dpi(app(|a| a.menubar.get()), hiword(wp) as u32);
            DeleteObject(old);
            SetWindowPos(
                hwnd,
                null_mut(),
                rc.left,
                rc.top,
                rc.right - rc.left,
                rc.bottom - rc.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            0
        }
        // Leaving the window (or moving it) dismisses an open menu.
        WM_ACTIVATE | WM_MOVE => {
            if msg == WM_MOVE || loword(wp) as u32 == WA_INACTIVE {
                menuview::close(app(|a| a.menubar.get()));
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_SETTINGCHANGE => {
            // Sent when the user flips Windows between light and dark app mode.
            let topic = lp as *const u16;
            let is_color = !topic.is_null() && {
                let want: Vec<u16> = "ImmersiveColorSet".encode_utf16().collect();
                (0..want.len()).all(|i| *topic.add(i) == want[i]) && *topic.add(want.len()) == 0
            };
            if is_color && app(|a| a.theme_choice.get()) == ID_THEME_SYSTEM {
                apply_theme();
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_CLOSE => {
            if confirm_discard() {
                DestroyWindow(hwnd);
            }
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

pub fn run() {
    unsafe {
        textview::register();
        statusview::register();
        menuview::register();
        let hinst = GetModuleHandleW(null());
        let class = w!("foxing");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            lpszClassName: class,
            ..zeroed()
        };
        RegisterClassW(&wc);
        app(|a| a.find_msg.set(RegisterWindowMessageW(FINDMSGSTRINGW)));

        let hwnd = CreateWindowExW(
            WS_EX_ACCEPTFILES,
            class,
            w!("Untitled - Foxing"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            null_mut(),
            null_mut(),
            hinst,
            null(),
        );
        if let Some(arg) = std::env::args_os().nth(1) {
            load_file(PathBuf::from(arg));
        }
        ShowWindow(hwnd, SW_SHOWDEFAULT);

        let ctrl = FVIRTKEY | FCONTROL;
        let accels = [
            ACCEL {
                fVirt: ctrl,
                key: b'N' as u16,
                cmd: ID_NEW,
            },
            ACCEL {
                fVirt: ctrl,
                key: b'O' as u16,
                cmd: ID_OPEN,
            },
            ACCEL {
                fVirt: ctrl,
                key: b'S' as u16,
                cmd: ID_SAVE,
            },
            ACCEL {
                fVirt: ctrl | FSHIFT,
                key: b'S' as u16,
                cmd: ID_SAVE_AS,
            },
            ACCEL {
                fVirt: ctrl,
                key: b'F' as u16,
                cmd: ID_FIND,
            },
            ACCEL {
                fVirt: ctrl,
                key: b'A' as u16,
                cmd: ID_SELECT_ALL,
            },
            ACCEL {
                fVirt: FVIRTKEY,
                key: 0x72,
                cmd: ID_FIND_NEXT,
            }, // VK_F3
        ];
        let haccel = CreateAcceleratorTableW(accels.as_ptr(), accels.len() as i32);

        let bar = app(|a| a.menubar.get());
        // Alt pressed and released with nothing in between toggles menu mode.
        let mut alt_tap = false;
        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            let (m, vk) = (msg.message, msg.wParam as u16);
            let is_key = matches!(
                m,
                WM_KEYDOWN | WM_KEYUP | WM_CHAR | WM_SYSKEYDOWN | WM_SYSKEYUP | WM_SYSCHAR
            );
            if m == WM_SYSKEYDOWN && vk == VK_MENU {
                // Ignore auto-repeat while Alt is held.
                if msg.lParam & (1 << 30) == 0 {
                    alt_tap = true;
                }
                continue;
            }
            if m == WM_SYSKEYUP && vk == VK_MENU {
                if std::mem::take(&mut alt_tap) {
                    menuview::toggle_keyboard(bar);
                }
                continue;
            }
            if m == WM_KEYDOWN || m == WM_SYSKEYDOWN {
                alt_tap = false;
            }
            if m == WM_SYSKEYDOWN && vk == VK_F10 && GetKeyState(VK_SHIFT as i32) >= 0 {
                menuview::toggle_keyboard(bar);
                continue;
            }
            if is_key && menuview::is_active(bar) {
                let key = match (m, vk) {
                    (WM_KEYDOWN | WM_SYSKEYDOWN, VK_LEFT) => Some(MenuKey::Left),
                    (WM_KEYDOWN | WM_SYSKEYDOWN, VK_RIGHT) => Some(MenuKey::Right),
                    (WM_KEYDOWN | WM_SYSKEYDOWN, VK_UP) => Some(MenuKey::Up),
                    (WM_KEYDOWN | WM_SYSKEYDOWN, VK_DOWN) => Some(MenuKey::Down),
                    (WM_KEYDOWN | WM_SYSKEYDOWN, VK_RETURN) => Some(MenuKey::Enter),
                    (WM_KEYDOWN | WM_SYSKEYDOWN, VK_ESCAPE) => Some(MenuKey::Escape),
                    (WM_CHAR | WM_SYSCHAR, _) => char::from_u32(msg.wParam as u32)
                        .filter(|c| c.is_alphanumeric())
                        .map(MenuKey::Char),
                    (WM_KEYDOWN | WM_SYSKEYDOWN, _) => {
                        // Turn letter keys into WM_CHAR for mnemonics; don't dispatch.
                        TranslateMessage(&msg);
                        None
                    }
                    _ => None,
                };
                if let Some(k) = key {
                    menuview::key(bar, k);
                }
                continue;
            }
            if m == WM_SYSCHAR {
                let c = char::from_u32(msg.wParam as u32).filter(|c| c.is_alphanumeric());
                if c.is_some_and(|c| menuview::mnemonic(bar, c)) {
                    continue;
                }
            }
            let dlg = app(|a| a.find_dlg.get());
            if !dlg.is_null() && IsDialogMessageW(dlg, &msg) != 0 {
                continue;
            }
            if TranslateAcceleratorW(hwnd, haccel, &msg) != 0 {
                continue;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
