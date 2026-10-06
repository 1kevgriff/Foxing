//! Win32 shell: windows, menus, dialogs. Document and text logic live in the library.

use crate::ids::*;
use foxing::document::Document;
use foxing::text;
use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::Dialogs::*;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const FIND_BUF_LEN: usize = 256;
/// EM_SETLIMITTEXT(0) maximum for a multiline EDIT control.
const EDIT_MAX_CHARS: usize = 0x7FFF_FFFE;

struct App {
    main: Cell<HWND>,
    edit: Cell<HWND>,
    font: Cell<HFONT>,
    doc: RefCell<Document>,
    wrap: Cell<bool>,
    find_dlg: Cell<HWND>,
    find_msg: Cell<u32>,
    find: Cell<*mut FINDREPLACEW>,
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

unsafe fn get_text(h: HWND) -> Vec<u16> {
    let len = GetWindowTextLengthW(h);
    let mut buf = vec![0u16; len as usize + 1];
    let got = GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32);
    buf.truncate(got as usize);
    buf
}

unsafe fn get_sel(edit: HWND) -> (usize, usize) {
    let (mut s, mut e) = (0u32, 0u32);
    SendMessageW(
        edit,
        EM_GETSEL,
        &mut s as *mut u32 as WPARAM,
        &mut e as *mut u32 as LPARAM,
    );
    (s as usize, e as usize)
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

unsafe fn create_edit(parent: HWND, wrap: bool) -> HWND {
    let mut style =
        WS_CHILD | WS_VISIBLE | WS_VSCROLL | (ES_MULTILINE | ES_AUTOVSCROLL | ES_NOHIDESEL) as u32;
    if !wrap {
        style |= WS_HSCROLL | ES_AUTOHSCROLL as u32;
    }
    let edit = CreateWindowExW(
        0,
        w!("EDIT"),
        null(),
        style,
        0,
        0,
        0,
        0,
        parent,
        IDC_EDIT as usize as HMENU,
        GetModuleHandleW(null()),
        null(),
    );
    SendMessageW(edit, EM_SETLIMITTEXT, 0, 0);
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
    match Document::open(path.clone()) {
        Ok((doc, body)) => {
            // The EDIT control requires CRLF.
            let edit = app(|a| a.edit.get());
            let s = wide(&text::with_eol(&body, "\r\n"));
            drop(body);
            let units = s.len() - 1;
            if units > EDIT_MAX_CHARS {
                error_box(&format!(
                    "{} is too large to open ({units} characters; the limit is {EDIT_MAX_CHARS}).",
                    path.display()
                ));
                return;
            }
            SetWindowTextW(edit, s.as_ptr());
            // The control can fail silently (out of memory); never adopt a path whose
            // contents didn't load, or Save would overwrite the file with partial text.
            if GetWindowTextLengthW(edit) as usize != units {
                SetWindowTextW(edit, w!(""));
                set_doc(Document::new());
                error_box(&format!("{} could not be loaded completely.", path.display()));
                return;
            }
            SendMessageW(edit, EM_EMPTYUNDOBUFFER, 0, 0);
            set_sel(edit, 0, 0);
            set_doc(doc);
        }
        Err(e) => error_box(&format!("Cannot open {}:\n{e}", path.display())),
    }
}

unsafe fn save_to(path: PathBuf) -> bool {
    let units = get_text(app(|a| a.edit.get()));
    let body = text::to_lf(&String::from_utf16_lossy(&units));
    match app(|a| a.doc.borrow_mut().save_as(path.clone(), &body)) {
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
    let (main, old) = app(|a| (a.main.get(), a.edit.get()));
    let mut text = get_text(old);
    text.push(0);
    let (s, e) = get_sel(old);
    let wrap = !app(|a| a.wrap.get());
    DestroyWindow(old);
    let edit = create_edit(main, wrap);
    SetWindowTextW(edit, text.as_ptr());
    set_sel(edit, s, e);
    app(|a| {
        a.wrap.set(wrap);
        a.edit.set(edit);
    });
    let check = if wrap { MF_CHECKED } else { MF_UNCHECKED };
    CheckMenuItem(GetMenu(main), ID_WRAP as u32, MF_BYCOMMAND | check);
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
    let needle = needle(fr);
    let edit = app(|a| a.edit.get());
    let hay = get_text(edit);
    let (s, e) = get_sel(edit);
    let down = (*fr).Flags & FR_DOWN != 0;
    let case = (*fr).Flags & FR_MATCHCASE != 0;
    match text::find(&hay, &needle, if down { e } else { s }, case, down) {
        Some(i) => set_sel(edit, i, i + needle.len()),
        None => {
            MessageBeep(MB_OK);
        }
    }
}

unsafe fn build_menu() -> HMENU {
    let item = |m: HMENU, id: u16, label: *const u16| {
        AppendMenuW(m, MF_STRING, id as usize, label);
    };
    let sep = |m: HMENU| {
        AppendMenuW(m, MF_SEPARATOR, 0, null());
    };

    let file = CreatePopupMenu();
    item(file, ID_NEW, w!("&New\tCtrl+N"));
    item(file, ID_OPEN, w!("&Open...\tCtrl+O"));
    item(file, ID_SAVE, w!("&Save\tCtrl+S"));
    item(file, ID_SAVE_AS, w!("Save &As...\tCtrl+Shift+S"));
    sep(file);
    item(file, ID_EXIT, w!("E&xit"));

    let edit = CreatePopupMenu();
    item(edit, ID_UNDO, w!("&Undo\tCtrl+Z"));
    sep(edit);
    item(edit, ID_CUT, w!("Cu&t\tCtrl+X"));
    item(edit, ID_COPY, w!("&Copy\tCtrl+C"));
    item(edit, ID_PASTE, w!("&Paste\tCtrl+V"));
    sep(edit);
    item(edit, ID_FIND, w!("&Find...\tCtrl+F"));
    item(edit, ID_FIND_NEXT, w!("Find &Next\tF3"));
    sep(edit);
    item(edit, ID_SELECT_ALL, w!("Select &All\tCtrl+A"));

    let format = CreatePopupMenu();
    item(format, ID_WRAP, w!("&Word Wrap"));

    let bar = CreateMenu();
    AppendMenuW(bar, MF_POPUP, file as usize, w!("&File"));
    AppendMenuW(bar, MF_POPUP, edit as usize, w!("&Edit"));
    AppendMenuW(bar, MF_POPUP, format as usize, w!("F&ormat"));
    bar
}

unsafe fn on_command(id: u16, code: u16) {
    let (main, edit) = app(|a| (a.main.get(), a.edit.get()));
    match id {
        IDC_EDIT => {
            if code == EN_CHANGE as u16 && app(|a| a.doc.borrow_mut().mark_dirty()) {
                update_title();
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
            let edit = create_edit(hwnd, false);
            app(|a| a.edit.set(edit));
            0
        }
        WM_SIZE => {
            let edit = app(|a| a.edit.get());
            MoveWindow(
                edit,
                0,
                0,
                loword(lp as usize) as i32,
                hiword(lp as usize) as i32,
                1,
            );
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
            build_menu(),
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

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
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
