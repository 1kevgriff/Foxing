//! Win32 host for [`MenuBar`]: the bar is a child window ("FoxingMenuBar"); the open
//! drop-down is a non-activating owned popup ("FoxingMenuPopup") that only paints.
//! While a menu is active the bar holds mouse capture, so all mouse input (including
//! over the popup) arrives here in bar coordinates.

use crate::gdi::{self, BackBuffer, Gdi};
use crate::uia;
use foxing::ui::a11y::{Action, Node};
use foxing::ui::menu::{Menu, MenuBar, MenuKey, MenuMsg};
use foxing::ui::{DrawList, Rect, Theme};
use std::cell::RefCell;
use std::mem::zeroed;
use std::ptr::{null, null_mut};
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::WM_MOUSELEAVE;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const BAR_CLASS: *const u16 = w!("FoxingMenuBar");
const POPUP_CLASS: *const u16 = w!("FoxingMenuPopup");

/// Test/automation queries (no pointers, so they work across processes).
/// `wParam` = command id; returns 1 if checked.
pub const MB_GETCHECK: u32 = WM_APP + 1;
/// Returns the open menu index, or -1.
pub const MB_GETOPEN: u32 = WM_APP + 2;
/// Returns 1 while in menu mode.
pub const MB_ISACTIVE: u32 = WM_APP + 3;

struct State {
    bar: MenuBar,
    gdi: Gdi,
    font: HFONT,
    theme: Theme,
    back: BackBuffer,
    popup: HWND,
    popup_back: BackBuffer,
    tracking_leave: bool,
}

impl Drop for State {
    fn drop(&mut self) {
        unsafe {
            if !self.popup.is_null() {
                DestroyWindow(self.popup);
            }
            DeleteObject(self.font as HGDIOBJ);
        }
    }
}

pub unsafe fn register() {
    let hinst = GetModuleHandleW(null());
    let arrow = LoadCursorW(null_mut(), IDC_ARROW);
    for (class, proc, style) in [
        (
            BAR_CLASS,
            bar_proc as unsafe extern "system" fn(_, _, _, _) -> _,
            0,
        ),
        (POPUP_CLASS, popup_proc, CS_DROPSHADOW),
    ] {
        let wc = WNDCLASSW {
            style,
            lpfnWndProc: Some(proc),
            hInstance: hinst,
            hCursor: arrow,
            lpszClassName: class,
            ..zeroed()
        };
        RegisterClassW(&wc);
    }
}

pub unsafe fn create(parent: HWND, id: u16, menus: Vec<Menu>) -> HWND {
    let menus = Box::new(menus);
    CreateWindowExW(
        0,
        BAR_CLASS,
        null(),
        WS_CHILD | WS_VISIBLE,
        0,
        0,
        0,
        0,
        parent,
        id as usize as HMENU,
        GetModuleHandleW(null()),
        Box::into_raw(menus) as *const _,
    )
}

unsafe fn state(hwnd: HWND) -> Option<&'static RefCell<State>> {
    (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<State>).as_ref()
}

unsafe fn make_gdi(dpi: u32) -> (Gdi, HFONT) {
    let font = gdi::ui_font(dpi);
    (Gdi::new(font, font, dpi), font)
}

unsafe fn client(hwnd: HWND) -> Rect {
    let mut rc: RECT = zeroed();
    GetClientRect(hwnd, &mut rc);
    Rect::new(0, 0, rc.right, rc.bottom)
}

pub unsafe fn height(hwnd: HWND) -> i32 {
    state(hwnd).map_or(0, |s| {
        let s = s.borrow();
        s.bar.height(&s.gdi)
    })
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

pub unsafe fn set_theme(hwnd: HWND, theme: Theme) {
    let popup = state(hwnd).map(|s| {
        let mut s = s.borrow_mut();
        s.theme = theme;
        s.popup
    });
    InvalidateRect(hwnd, null(), 0);
    if let Some(p) = popup.filter(|p| !p.is_null()) {
        InvalidateRect(p, null(), 0);
    }
}

pub unsafe fn set_checked(hwnd: HWND, id: u16, on: bool) {
    if let Some(s) = state(hwnd) {
        s.borrow_mut().bar.set_checked(id, on);
    }
}

pub unsafe fn is_active(hwnd: HWND) -> bool {
    state(hwnd).is_some_and(|s| s.borrow().bar.is_active())
}

/// Alt tap / F10.
pub unsafe fn toggle_keyboard(hwnd: HWND) {
    run(hwnd, |b, _, _| b.toggle_keyboard());
}

/// Alt+letter. Returns true if a menu opened.
pub unsafe fn mnemonic(hwnd: HWND, c: char) -> bool {
    run(hwnd, |b, _, _| b.mnemonic(c)) != MenuMsg::Nothing
}

pub unsafe fn key(hwnd: HWND, k: MenuKey) {
    run(hwnd, |b, _, _| b.key(k));
}

pub unsafe fn close(hwnd: HWND) {
    run(hwnd, |b, _, _| b.close());
}

static A11Y: uia::Source = uia::Source {
    tree: a11y_tree,
    act: a11y_act,
    class: "FoxingMenuBar",
    text: None,
};

unsafe fn a11y_tree(hwnd: HWND) -> Node {
    state(hwnd)
        .and_then(|s| {
            s.try_borrow()
                .ok()
                .map(|st| st.bar.a11y(client(hwnd), &st.gdi))
        })
        .unwrap_or_default()
}

unsafe fn a11y_act(hwnd: HWND, a: Action) {
    match a {
        Action::ToggleMenu(i) => {
            run(hwnd, |b, _, _| b.toggle_menu(i));
        }
        Action::Command(id) => {
            run(hwnd, |b, _, _| {
                b.close();
                MenuMsg::Command(id)
            });
        }
        Action::ActivateRow(_) => {}
    }
}

/// Applies an input to the bar, then syncs the window state to the result.
unsafe fn run(hwnd: HWND, f: impl FnOnce(&mut MenuBar, Rect, &Gdi) -> MenuMsg) -> MenuMsg {
    let Some(s) = state(hwnd) else {
        return MenuMsg::Nothing;
    };
    let (msg, active) = {
        let mut st = s.borrow_mut();
        let State { bar, gdi, .. } = &mut *st;
        let msg = f(bar, client(hwnd), gdi);
        (msg, bar.is_active())
    };
    if msg == MenuMsg::Nothing {
        return msg;
    }
    sync_popup(hwnd, s);
    InvalidateRect(hwnd, null(), 0);
    if active {
        if GetCapture() != hwnd {
            SetCapture(hwnd);
        }
    } else if GetCapture() == hwnd {
        ReleaseCapture();
    }
    if let MenuMsg::Command(id) = msg {
        // Posted, not sent: the command may show dialogs or rebuild state.
        PostMessageW(GetParent(hwnd), WM_COMMAND, id as WPARAM, 0);
    }
    uia::focus_changed(hwnd, &A11Y);
    msg
}

/// Shows the popup under the open title, or hides it.
unsafe fn sync_popup(hwnd: HWND, s: &RefCell<State>) {
    // The popup is created on first use to keep startup lean.
    if s.borrow().popup.is_null() && s.borrow().bar.open_menu().is_some() {
        let popup = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            POPUP_CLASS,
            null(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            GetParent(hwnd),
            null_mut(),
            GetModuleHandleW(null()),
            hwnd as *const _,
        );
        s.borrow_mut().popup = popup;
    }
    let (popup, rect) = {
        let st = s.borrow();
        (
            st.popup,
            st.bar.dropdown(client(hwnd), &st.gdi).map(|(r, _)| r),
        )
    };
    match rect {
        Some(r) => {
            let mut pt = POINT { x: r.x, y: r.y };
            ClientToScreen(hwnd, &mut pt);
            SetWindowPos(
                popup,
                null_mut(),
                pt.x,
                pt.y,
                r.w,
                r.h,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW,
            );
            InvalidateRect(popup, null(), 0);
        }
        None if !popup.is_null() => {
            ShowWindow(popup, SW_HIDE);
        }
        None => {}
    }
}

fn point(lp: LPARAM) -> (i32, i32) {
    (
        (lp & 0xFFFF) as i16 as i32,
        ((lp >> 16) & 0xFFFF) as i16 as i32,
    )
}

unsafe extern "system" fn bar_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let cs = &*(lp as *const CREATESTRUCTW);
            let menus = *Box::from_raw(cs.lpCreateParams as *mut Vec<Menu>);
            let (g, font) = make_gdi(GetDpiForWindow(cs.hwndParent).max(96));
            let s = Box::new(RefCell::new(State {
                bar: MenuBar::new(menus),
                gdi: g,
                font,
                theme: Theme::LIGHT,
                back: BackBuffer::new(),
                popup: null_mut(),
                popup_back: BackBuffer::new(),
                tracking_leave: false,
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
            let mut st = s.borrow_mut();
            let State {
                bar,
                gdi: g,
                theme,
                back,
                ..
            } = &mut *st;
            back.paint(hwnd, |dc, w, h, _| {
                let mut dl = DrawList::default();
                bar.paint_bar(Rect::new(0, 0, w, h), theme, g, &mut dl);
                gdi::render(dc, &dl, g);
            });
            0
        }
        WM_MOUSEMOVE => {
            let (x, y) = point(lp);
            if let Some(s) = state(hwnd) {
                let mut st = s.borrow_mut();
                if !st.tracking_leave && !st.bar.is_active() {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    st.tracking_leave = TrackMouseEvent(&mut tme) != 0;
                }
            }
            run(hwnd, |b, r, g| b.mouse_move(r, g, x, y));
            0
        }
        WM_MOUSELEAVE => {
            if let Some(s) = state(hwnd) {
                s.borrow_mut().tracking_leave = false;
            }
            run(hwnd, |b, _, _| b.mouse_leave());
            0
        }
        WM_LBUTTONDOWN => {
            let (x, y) = point(lp);
            run(hwnd, |b, r, g| b.mouse_down(r, g, x, y));
            0
        }
        WM_LBUTTONUP => {
            let (x, y) = point(lp);
            run(hwnd, |b, r, g| b.mouse_up(r, g, x, y));
            0
        }
        WM_CAPTURECHANGED => {
            // Another window took the mouse (e.g. Alt+Tab): leave menu mode.
            if lp as HWND != hwnd && is_active(hwnd) {
                close(hwnd);
            }
            0
        }
        WM_GETOBJECT => uia::get_object(hwnd, wp, lp, &A11Y)
            .unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp)),
        MB_GETCHECK => state(hwnd).map_or(0, |s| s.borrow().bar.is_checked(wp as u16) as LRESULT),
        MB_GETOPEN => state(hwnd).map_or(-1, |s| {
            s.borrow().bar.open_menu().map_or(-1, |i| i as LRESULT)
        }),
        MB_ISACTIVE => is_active(hwnd) as LRESULT,
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn popup_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let cs = &*(lp as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let bar_hwnd = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as HWND;
            let Some(s) = state(bar_hwnd) else {
                return DefWindowProcW(hwnd, msg, wp, lp);
            };
            let bounds = client(bar_hwnd);
            let mut st = s.borrow_mut();
            let State {
                bar,
                gdi: g,
                theme,
                popup_back,
                ..
            } = &mut *st;
            let origin = bar.dropdown(bounds, g).map(|(r, _)| (r.x, r.y));
            popup_back.paint(hwnd, |dc, _, _, _| {
                let mut dl = DrawList::default();
                bar.paint_dropdown(bounds, theme, g, &mut dl);
                if let Some((x, y)) = origin {
                    dl.offset(-x, -y);
                }
                gdi::render(dc, &dl, g);
            });
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
