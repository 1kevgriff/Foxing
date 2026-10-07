//! Win32 host for [`StatusBar`] ("FoxingStatus"). `WM_GETTEXT` returns the parts joined
//! by tabs, for automation and tests.

use crate::gdi::{self, BackBuffer, Gdi};
use foxing::ui::status::StatusBar;
use foxing::ui::{DrawList, Rect, Theme};
use std::cell::RefCell;
use std::mem::zeroed;
use std::ptr::{null, null_mut};
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: *const u16 = w!("FoxingStatus");

struct State {
    bar: StatusBar,
    gdi: Gdi,
    font: HFONT,
    theme: Theme,
    back: BackBuffer,
}

impl Drop for State {
    fn drop(&mut self) {
        unsafe { DeleteObject(self.font as HGDIOBJ) };
    }
}

pub unsafe fn register() {
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: GetModuleHandleW(null()),
        hCursor: LoadCursorW(null_mut(), IDC_ARROW),
        lpszClassName: CLASS,
        ..zeroed()
    };
    RegisterClassW(&wc);
}

/// `widths` are the fixed parts (96-DPI px) after the flexible first part.
pub unsafe fn create(parent: HWND, id: u16, widths: &'static [i32]) -> HWND {
    CreateWindowExW(
        0,
        CLASS,
        null(),
        WS_CHILD | WS_VISIBLE,
        0,
        0,
        0,
        0,
        parent,
        id as usize as HMENU,
        GetModuleHandleW(null()),
        &widths as *const &'static [i32] as *const _,
    )
}

unsafe fn state(hwnd: HWND) -> Option<&'static RefCell<State>> {
    (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<State>).as_ref()
}

unsafe fn make_gdi(dpi: u32) -> (Gdi, HFONT) {
    let font = gdi::ui_font(dpi);
    (Gdi::new(font, font, dpi), font)
}

/// Updates part texts; repaints only if something changed.
pub unsafe fn set_texts(hwnd: HWND, texts: &[String]) {
    let Some(s) = state(hwnd) else { return };
    let changed = {
        let mut s = s.borrow_mut();
        let mut changed = false;
        for (i, t) in texts.iter().enumerate().take(s.bar.parts()) {
            changed |= s.bar.set_text(i, t);
        }
        changed
    };
    if changed {
        InvalidateRect(hwnd, null(), 0);
    }
}

pub unsafe fn height(hwnd: HWND) -> i32 {
    state(hwnd).map_or(0, |s| {
        let s = s.borrow();
        s.bar.height(&s.gdi)
    })
}

pub unsafe fn set_theme(hwnd: HWND, theme: Theme) {
    if let Some(s) = state(hwnd) {
        s.borrow_mut().theme = theme;
    }
    InvalidateRect(hwnd, null(), 0);
}

pub unsafe fn set_dpi(hwnd: HWND, dpi: u32) {
    if let Some(s) = state(hwnd) {
        let mut s = s.borrow_mut();
        let (g, font) = make_gdi(dpi);
        DeleteObject(s.font as HGDIOBJ);
        s.gdi = g;
        s.font = font;
    }
    InvalidateRect(hwnd, null(), 0);
}

fn joined(bar: &StatusBar) -> Vec<u16> {
    (0..bar.parts())
        .map(|i| bar.text(i))
        .collect::<Vec<_>>()
        .join("\t")
        .encode_utf16()
        .collect()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let cs = &*(lp as *const CREATESTRUCTW);
            // Valid for the duration of CreateWindowExW, which sends WM_NCCREATE synchronously.
            let widths: &[i32] = *(cs.lpCreateParams as *const &[i32]);
            let (g, font) = make_gdi(GetDpiForWindow(cs.hwndParent));
            let s = Box::new(RefCell::new(State {
                bar: StatusBar::new(widths),
                gdi: g,
                font,
                theme: Theme::LIGHT,
                back: BackBuffer::new(),
            }));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(s) as isize);
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_NCDESTROY => {
            let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut RefCell<State>;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            if !p.is_null() {
                drop(Box::from_raw(p));
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let Some(s) = state(hwnd) else {
                return DefWindowProcW(hwnd, msg, wp, lp);
            };
            let mut s = s.borrow_mut();
            let State {
                bar,
                gdi: g,
                theme,
                back,
                ..
            } = &mut *s;
            back.paint(hwnd, |dc, w, h, _| {
                let mut dl = DrawList::default();
                bar.paint(Rect::new(0, 0, w, h), theme, g, &mut dl);
                gdi::render(dc, &dl, g);
            });
            0
        }
        WM_GETTEXTLENGTH => state(hwnd).map_or(0, |s| joined(&s.borrow().bar).len() as LRESULT),
        WM_GETTEXT => {
            let Some(s) = state(hwnd) else { return 0 };
            let text = joined(&s.borrow().bar);
            let cap = wp;
            let out = lp as *mut u16;
            if cap == 0 || out.is_null() {
                return 0;
            }
            let n = text.len().min(cap - 1);
            std::ptr::copy_nonoverlapping(text.as_ptr(), out, n);
            *out.add(n) = 0;
            n as LRESULT
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
