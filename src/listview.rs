//! Win32 host for the folder sidebar ([`List`]): "FoxingFolder". Activating a row
//! posts `WM_COMMAND(id, LN_ACTIVATE)` to the parent, which asks for [`selected`].

use crate::gdi::{self, BackBuffer, Gdi};
use crate::uia;
use foxing::ui::a11y::{Action, Node};
use foxing::ui::list::{List, ListMsg};
use foxing::ui::{DrawList, Measure, Rect, Theme};
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

const CLASS: *const u16 = w!("FoxingFolder");
/// Sidebar width (96-DPI px).
const WIDTH: i32 = 220;

/// Notification code (WM_COMMAND high word): a row was activated.
pub const LN_ACTIVATE: u16 = 0x8101;
/// Automation: number of rows.
pub const FL_GETCOUNT: u32 = WM_APP + 20;
/// Automation: activate row `wParam`, as a click would.
pub const FL_ACTIVATE: u32 = WM_APP + 21;
/// Automation: selected row, or -1.
pub const FL_GETSEL: u32 = WM_APP + 22;

struct State {
    list: List,
    gdi: Gdi,
    font: HFONT,
    theme: Theme,
    back: BackBuffer,
    tracking_leave: bool,
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

/// Created hidden; shown when a folder opens.
pub unsafe fn create(parent: HWND, id: u16) -> HWND {
    CreateWindowExW(
        0,
        CLASS,
        null(),
        WS_CHILD,
        0,
        0,
        0,
        0,
        parent,
        id as usize as HMENU,
        GetModuleHandleW(null()),
        null(),
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

pub unsafe fn width(hwnd: HWND) -> i32 {
    state(hwnd).map_or(WIDTH, |s| s.borrow().gdi.px(WIDTH))
}

pub unsafe fn set_items(hwnd: HWND, header: &str, items: Vec<String>) {
    if let Some(s) = state(hwnd) {
        let mut s = s.borrow_mut();
        s.list.header = header.to_owned();
        s.list.set_items(items);
    }
    InvalidateRect(hwnd, null(), 0);
}

/// Marks the current row (or none) and scrolls it into view.
pub unsafe fn set_selected(hwnd: HWND, sel: Option<usize>) {
    if let Some(s) = state(hwnd) {
        let mut st = s.borrow_mut();
        let State { list, gdi, .. } = &mut *st;
        list.set_selected(sel);
        if let Some(i) = sel {
            list.reveal(client(hwnd), gdi, i);
        }
    }
    InvalidateRect(hwnd, null(), 0);
}

pub unsafe fn selected(hwnd: HWND) -> Option<usize> {
    state(hwnd).and_then(|s| s.borrow().list.selected())
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

static A11Y: uia::Source = uia::Source {
    tree: a11y_tree,
    act: a11y_act,
    class: "FoxingFolder",
    text: None,
};

unsafe fn a11y_tree(hwnd: HWND) -> Node {
    state(hwnd)
        .and_then(|s| {
            s.try_borrow()
                .ok()
                .map(|st| st.list.a11y(client(hwnd), &st.gdi))
        })
        .unwrap_or_default()
}

unsafe fn a11y_act(hwnd: HWND, a: Action) {
    if let Action::ActivateRow(i) = a {
        activate(hwnd, i);
    }
}

/// Selects row `i` and tells the parent, as a click would.
unsafe fn activate(hwnd: HWND, i: usize) {
    if let Some(s) = state(hwnd) {
        s.borrow_mut().list.set_selected(Some(i));
    }
    InvalidateRect(hwnd, null(), 0);
    notify_activate(hwnd);
}

unsafe fn notify_activate(hwnd: HWND) {
    let id = GetDlgCtrlID(hwnd) as usize;
    // Posted: the parent may show a save prompt.
    PostMessageW(
        GetParent(hwnd),
        WM_COMMAND,
        id | ((LN_ACTIVATE as usize) << 16),
        hwnd as LPARAM,
    );
}

fn point(lp: LPARAM) -> (i32, i32) {
    (
        (lp & 0xFFFF) as i16 as i32,
        ((lp >> 16) & 0xFFFF) as i16 as i32,
    )
}

/// Runs `f` on the list; repaints / notifies per the result.
unsafe fn run(hwnd: HWND, f: impl FnOnce(&mut List, Rect, &Gdi) -> ListMsg) {
    let Some(s) = state(hwnd) else { return };
    let msg = {
        let mut st = s.borrow_mut();
        let State { list, gdi, .. } = &mut *st;
        f(list, client(hwnd), gdi)
    };
    match msg {
        ListMsg::Nothing => {}
        ListMsg::Repaint => {
            InvalidateRect(hwnd, null(), 0);
        }
        ListMsg::Activate(_) => {
            InvalidateRect(hwnd, null(), 0);
            notify_activate(hwnd);
            uia::focus_changed(hwnd, &A11Y);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let cs = &*(lp as *const CREATESTRUCTW);
            let (g, font) = make_gdi(GetDpiForWindow(cs.hwndParent).max(96));
            let s = Box::new(RefCell::new(State {
                list: List::new(""),
                gdi: g,
                font,
                theme: Theme::LIGHT,
                back: BackBuffer::new(),
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
                list,
                gdi: g,
                theme,
                back,
                ..
            } = &mut *st;
            back.paint(hwnd, |dc, w, h, _| {
                let mut dl = DrawList::default();
                list.paint(Rect::new(0, 0, w, h), theme, g, &mut dl);
                gdi::render(dc, &dl, g);
            });
            0
        }
        WM_MOUSEMOVE => {
            let (x, y) = point(lp);
            if let Some(s) = state(hwnd) {
                let mut st = s.borrow_mut();
                if !st.tracking_leave {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    st.tracking_leave = TrackMouseEvent(&mut tme) != 0;
                }
            }
            run(hwnd, |l, r, g| l.mouse_move(r, g, x, y));
            0
        }
        WM_MOUSELEAVE => {
            if let Some(s) = state(hwnd) {
                s.borrow_mut().tracking_leave = false;
            }
            run(hwnd, |l, _, _| l.mouse_leave());
            0
        }
        WM_LBUTTONDOWN => {
            let (x, y) = point(lp);
            run(hwnd, |l, r, g| l.click(r, g, x, y));
            0
        }
        WM_MOUSEWHEEL => {
            let notches = ((wp >> 16) as i16) as i64 / 120;
            run(hwnd, |l, r, g| l.scroll(r, g, -notches * 3));
            0
        }
        WM_GETTEXTLENGTH | WM_GETTEXT => {
            let Some(s) = state(hwnd) else { return 0 };
            let text: Vec<u16> = s.borrow().list.items().join("\n").encode_utf16().collect();
            if msg == WM_GETTEXTLENGTH {
                return text.len() as LRESULT;
            }
            let out = lp as *mut u16;
            if wp == 0 || out.is_null() {
                return 0;
            }
            let n = text.len().min(wp - 1);
            std::ptr::copy_nonoverlapping(text.as_ptr(), out, n);
            *out.add(n) = 0;
            n as LRESULT
        }
        FL_GETCOUNT => state(hwnd).map_or(0, |s| s.borrow().list.items().len() as LRESULT),
        FL_GETSEL => selected(hwnd).map_or(-1, |i| i as LRESULT),
        FL_ACTIVATE => {
            activate(hwnd, wp);
            0
        }
        WM_GETOBJECT => uia::get_object(hwnd, wp, lp, &A11Y)
            .unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp)),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
