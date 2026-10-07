//! Win32 text view ("FoxingText"): paints rows from [`Editor`] and turns input into
//! editor calls. Also answers the EDIT messages automation relies on (`WM_GETTEXT`,
//! `EM_GETSEL`, `EM_SETSEL`, `EM_REPLACESEL`, ...) and sends `EN_CHANGE` to its parent.

use foxing::buffer::Buffer;
use foxing::document::NATIVE_EOL;
use foxing::editor::{Editor, Motion};
use std::cell::{Cell, RefCell};
use std::mem::zeroed;
use std::ptr::{null, null_mut};
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::DataExchange::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::*;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::Input::Ime::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: *const u16 = w!("FoxingText");
const CF_UNICODETEXT: u32 = 13;
const DRAG_TIMER: usize = 1;
const MK_SHIFT: usize = 0x0004;
/// Left text margin in pixels.
const PAD: i32 = 4;

struct View {
    ed: Editor,
    font: HFONT,
    cell_w: i32,
    line_h: i32,
    width: i32,
    height: i32,
    focused: bool,
    dragging: bool,
    /// High surrogate waiting for its pair from WM_CHAR.
    high: Option<u16>,
    /// Back buffer for flicker-free painting.
    back: HBITMAP,
    back_size: (i32, i32),
}

/// Per-window state. Size, focus, and line height live outside the `RefCell` because
/// Win32 delivers WM_SIZE / WM_SETFOCUS re-entrantly (from SetScrollInfo, SetFocus)
/// while the view is borrowed; those are recorded here and applied by `finish`.
struct Shared {
    view: RefCell<View>,
    size: Cell<(i32, i32)>,
    focused: Cell<bool>,
    line_h: Cell<i32>,
    stale_layout: Cell<bool>,
}

pub unsafe fn register() {
    let wc = WNDCLASSW {
        style: CS_DBLCLKS,
        lpfnWndProc: Some(wndproc),
        hInstance: GetModuleHandleW(null()),
        hCursor: LoadCursorW(null_mut(), IDC_IBEAM),
        lpszClassName: CLASS,
        ..zeroed()
    };
    RegisterClassW(&wc);
}

pub unsafe fn create(parent: HWND, id: u16) -> HWND {
    CreateWindowExW(
        0,
        CLASS,
        null(),
        WS_CHILD | WS_VISIBLE | WS_VSCROLL | WS_HSCROLL,
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

unsafe fn shared(hwnd: HWND) -> Option<&'static Shared> {
    (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Shared).as_ref()
}

fn sync(v: &mut View, sh: &Shared) {
    (v.width, v.height) = sh.size.get();
    v.focused = sh.focused.get();
}

/// What `finish` applies once the view is no longer borrowed.
struct Plan {
    vert: SCROLLINFO,
    horz: SCROLLINFO,
    caret: Option<(i32, i32)>,
}

/// Re-lays out if needed, then updates scrollbars, caret, and paint. Safe to call
/// re-entrantly: it does nothing while the view is borrowed (the outer call finishes).
unsafe fn finish(hwnd: HWND, sh: &Shared) {
    let plan = match sh.view.try_borrow_mut() {
        Ok(mut v) => {
            sync(&mut v, sh);
            if sh.stale_layout.replace(false) {
                layout(&mut v);
            }
            plan(&v)
        }
        Err(_) => return,
    };
    SetScrollInfo(hwnd, SB_VERT, &plan.vert, 1);
    SetScrollInfo(hwnd, SB_HORZ, &plan.horz, 1);
    if sh.focused.get() {
        let (x, y) = plan.caret.unwrap_or((-1000, -1000));
        SetCaretPos(x, y);
        let himc = ImmGetContext(hwnd);
        if !himc.is_null() {
            let cf = COMPOSITIONFORM {
                dwStyle: CFS_POINT,
                ptCurrentPos: POINT { x, y },
                rcArea: zeroed(),
            };
            ImmSetCompositionWindow(himc, &cf);
            ImmReleaseContext(hwnd, himc);
        }
    }
    InvalidateRect(hwnd, null(), 0);
}

/// Runs `f` on the view's editor, then refreshes scrollbars, caret, and paint.
/// Sends `EN_CHANGE` if the text changed. Must not be called from inside the view's
/// own message handling.
pub unsafe fn with<R>(hwnd: HWND, f: impl FnOnce(&mut Editor) -> R) -> R {
    let sh = shared(hwnd).expect("text view state");
    let (r, changed, moved) = {
        let mut v = sh.view.borrow_mut();
        sync(&mut v, sh);
        let before = (v.ed.version(), v.ed.anchor(), v.ed.caret());
        let r = f(&mut v.ed);
        let after = (v.ed.version(), v.ed.anchor(), v.ed.caret());
        (r, after.0 != before.0, after != before)
    };
    finish(hwnd, sh);
    if changed {
        notify_change(hwnd);
    }
    if moved {
        notify(hwnd, crate::ids::VN_CARET);
    }
    r
}

/// Replaces the document. Keeps the wrap setting; clears undo; no `EN_CHANGE`.
pub unsafe fn set_buffer(hwnd: HWND, buf: Buffer) {
    let sh = shared(hwnd).expect("text view state");
    {
        let mut v = sh.view.borrow_mut();
        let wrap = v.ed.wrap();
        v.ed = Editor::new(buf, NATIVE_EOL);
        v.ed.set_wrap(wrap);
    }
    sh.stale_layout.set(true);
    finish(hwnd, sh);
}

pub unsafe fn set_wrap(hwnd: HWND, wrap: bool) {
    let sh = shared(hwnd).expect("text view state");
    sh.view.borrow_mut().ed.set_wrap(wrap);
    sh.stale_layout.set(true);
    finish(hwnd, sh);
    sh.view.borrow_mut().ed.ensure_visible();
    finish(hwnd, sh);
}

unsafe fn notify(hwnd: HWND, code: u16) {
    let id = GetDlgCtrlID(hwnd) as usize;
    SendMessageW(
        GetParent(hwnd),
        WM_COMMAND,
        id | ((code as usize) << 16),
        hwnd as LPARAM,
    );
}

unsafe fn notify_change(hwnd: HWND) {
    notify(hwnd, EN_CHANGE as u16);
}

/// Reads the editor without refreshing or notifying (for status display).
pub unsafe fn peek<R>(hwnd: HWND, f: impl FnOnce(&Editor) -> R) -> R {
    let sh = shared(hwnd).expect("text view state");
    let v = sh.view.borrow();
    f(&v.ed)
}

unsafe fn measure(v: &mut View) {
    let dc = GetDC(null_mut());
    let old = SelectObject(dc, v.font as HGDIOBJ);
    let mut tm: TEXTMETRICW = zeroed();
    GetTextMetricsW(dc, &mut tm);
    SelectObject(dc, old);
    ReleaseDC(null_mut(), dc);
    v.cell_w = tm.tmAveCharWidth.max(1);
    v.line_h = (tm.tmHeight + tm.tmExternalLeading).max(1);
}

/// Pushes the pixel size into the editor's cell grid.
unsafe fn layout(v: &mut View) {
    let rows = (v.height / v.line_h).max(1) as usize;
    let cols = ((v.width - PAD) / v.cell_w).max(1) as usize;
    v.ed.set_view(rows, cols);
}

/// Scrollbar positions are i32; scale line numbers for documents beyond that.
fn v_scale(lines: usize) -> usize {
    lines / (i32::MAX as usize / 2) + 1
}

fn plan(v: &View) -> Plan {
    let lines = v.ed.buffer().line_count();
    let sc = v_scale(lines);
    let mut vert: SCROLLINFO = unsafe { zeroed() };
    vert.cbSize = size_of::<SCROLLINFO>() as u32;
    vert.fMask = SIF_RANGE | SIF_PAGE | SIF_POS;
    vert.nMax = ((lines - 1) / sc) as i32;
    vert.nPage = (v.ed.view_rows() / sc).max(1) as u32;
    vert.nPos = (v.ed.top_line() / sc) as i32;

    let mut horz = vert;
    let cols = ((v.width - PAD) / v.cell_w).max(1) as usize;
    if v.ed.wrap() {
        horz.nMax = 0;
        horz.nPage = 1;
        horz.nPos = 0;
    } else {
        let width = v.ed.visible_width().max(v.ed.left() + cols);
        horz.nMax = (width - 1).min(i32::MAX as usize) as i32;
        horz.nPage = cols as u32;
        horz.nPos = v.ed.left().min(i32::MAX as usize) as i32;
    }
    let caret =
        v.ed.caret_cell()
            .map(|(row, col)| (PAD + col as i32 * v.cell_w, row as i32 * v.line_h));
    Plan { vert, horz, caret }
}

unsafe fn paint(hwnd: HWND, v: &mut View) {
    let mut ps: PAINTSTRUCT = zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    let (w, h) = (v.width.max(1), v.height.max(1));
    if v.back.is_null() || v.back_size != (w, h) {
        if !v.back.is_null() {
            DeleteObject(v.back as HGDIOBJ);
        }
        v.back = CreateCompatibleBitmap(hdc, w, h);
        v.back_size = (w, h);
    }
    let mem = CreateCompatibleDC(hdc);
    let old_bmp = SelectObject(mem, v.back as HGDIOBJ);
    let old_font = SelectObject(mem, v.font as HGDIOBJ);
    let full = RECT {
        left: 0,
        top: 0,
        right: w,
        bottom: h,
    };
    FillRect(mem, &full, GetSysColorBrush(COLOR_WINDOW));

    let sel = v.ed.selection();
    let len = v.ed.buffer().len();
    let (text_fg, sel_fg, sel_bg) = (
        GetSysColor(COLOR_WINDOWTEXT),
        GetSysColor(COLOR_HIGHLIGHTTEXT),
        GetSysColor(COLOR_HIGHLIGHT),
    );
    let mut units: Vec<u16> = Vec::new();
    let mut dx: Vec<i32> = Vec::new();
    for (i, row) in v.ed.visible_rows().iter().enumerate() {
        let y = i as i32 * v.line_h;
        let glyphs = v.ed.glyphs(row);
        let mut g = 0;
        while g < glyphs.len() {
            let selected = sel.contains(&glyphs[g].at);
            let start = g;
            units.clear();
            dx.clear();
            while g < glyphs.len() && sel.contains(&glyphs[g].at) == selected {
                let gl = glyphs[g];
                let ch = match gl.ch {
                    '\t' => ' ',
                    c if (c as u32) < 0x20 || c == '\u{7f}' => '\u{FFFD}',
                    c => c,
                };
                let mut b = [0u16; 2];
                let enc = ch.encode_utf16(&mut b);
                units.extend_from_slice(enc);
                dx.push(gl.cells as i32 * v.cell_w);
                if enc.len() == 2 {
                    dx.push(0);
                }
                g += 1;
            }
            let x = PAD + glyphs[start].col as i32 * v.cell_w;
            let right = PAD + (glyphs[g - 1].col + glyphs[g - 1].cells) as i32 * v.cell_w;
            let rc = RECT {
                left: x,
                top: y,
                right,
                bottom: y + v.line_h,
            };
            if selected {
                SetTextColor(mem, sel_fg);
                SetBkColor(mem, sel_bg);
                ExtTextOutW(
                    mem,
                    x,
                    y,
                    ETO_OPAQUE,
                    &rc,
                    units.as_ptr(),
                    units.len() as u32,
                    dx.as_ptr(),
                );
            } else {
                SetTextColor(mem, text_fg);
                SetBkMode(mem, TRANSPARENT as i32);
                ExtTextOutW(
                    mem,
                    x,
                    y,
                    0,
                    null(),
                    units.as_ptr(),
                    units.len() as u32,
                    dx.as_ptr(),
                );
                SetBkMode(mem, OPAQUE as i32);
            }
        }
        // Show a selected line break as one highlighted cell after the text.
        let line_end = row.range.end;
        let has_break = line_end < len && v.ed.buffer().line_range(row.line).end == line_end;
        if has_break && sel.start <= line_end && sel.end > line_end {
            let col = glyphs.last().map_or(0, |gl| gl.col + gl.cells) as i32;
            let first_row =
                glyphs.is_empty() && row.range.start > v.ed.buffer().line_start(row.line);
            if !first_row {
                let rc = RECT {
                    left: PAD + col * v.cell_w,
                    top: y,
                    right: PAD + (col + 1) * v.cell_w,
                    bottom: y + v.line_h,
                };
                let brush = CreateSolidBrush(sel_bg);
                FillRect(mem, &rc, brush);
                DeleteObject(brush as HGDIOBJ);
            }
        }
    }

    BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
    SelectObject(mem, old_font);
    SelectObject(mem, old_bmp);
    DeleteDC(mem);
    EndPaint(hwnd, &ps);
}

/// Buffer offset under a client point, clamped into the viewport.
fn hit(v: &View, x: i32, y: i32) -> usize {
    let row = (y.max(0) / v.line_h) as usize;
    let col = (((x - PAD).max(0) + v.cell_w / 2) / v.cell_w) as usize;
    v.ed.offset_at(row.min(v.ed.view_rows().saturating_sub(1)), col)
}

fn lparam_point(lp: LPARAM) -> (i32, i32) {
    (
        (lp & 0xFFFF) as i16 as i32,
        ((lp >> 16) & 0xFFFF) as i16 as i32,
    )
}

unsafe fn key_down(hwnd: HWND, v: &mut View, vk: u16) -> bool {
    let ctrl = GetKeyState(VK_CONTROL as i32) < 0;
    let shift = GetKeyState(VK_SHIFT as i32) < 0;
    let motion = match vk {
        VK_LEFT if ctrl => Some(Motion::WordLeft),
        VK_LEFT => Some(Motion::Left),
        VK_RIGHT if ctrl => Some(Motion::WordRight),
        VK_RIGHT => Some(Motion::Right),
        VK_UP if !ctrl => Some(Motion::Up),
        VK_DOWN if !ctrl => Some(Motion::Down),
        VK_PRIOR => Some(Motion::PageUp),
        VK_NEXT => Some(Motion::PageDown),
        VK_HOME if ctrl => Some(Motion::DocStart),
        VK_HOME => Some(Motion::Home),
        VK_END if ctrl => Some(Motion::DocEnd),
        VK_END => Some(Motion::End),
        _ => None,
    };
    if let Some(m) = motion {
        v.ed.move_caret(m, shift);
        return true;
    }
    match vk {
        VK_UP => v.ed.scroll_rows(-1),
        VK_DOWN => v.ed.scroll_rows(1),
        VK_BACK => v.ed.backspace(ctrl),
        VK_DELETE if shift => copy(hwnd, v, true),
        VK_DELETE => v.ed.delete_forward(ctrl),
        VK_INSERT if ctrl => copy(hwnd, v, false),
        VK_INSERT if shift => paste(hwnd, v),
        0x5A if ctrl && shift => {
            v.ed.redo();
        }
        0x5A if ctrl => {
            v.ed.undo();
        }
        0x59 if ctrl => {
            v.ed.redo();
        }
        0x58 if ctrl => copy(hwnd, v, true),
        0x43 if ctrl => copy(hwnd, v, false),
        0x56 if ctrl => paste(hwnd, v),
        _ => return false,
    }
    true
}

unsafe fn copy(hwnd: HWND, v: &mut View, cut: bool) {
    if v.ed.selection().is_empty() {
        return;
    }
    let text = if cut {
        v.ed.cut()
    } else {
        v.ed.selected_text()
    };
    // Clipboard text uses CRLF by convention.
    let text = foxing::text::with_eol(&foxing::text::to_lf(&text), "\r\n");
    let units: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    if OpenClipboard(hwnd) == 0 {
        return;
    }
    EmptyClipboard();
    let mem = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2);
    if !mem.is_null() {
        let p = GlobalLock(mem) as *mut u16;
        if !p.is_null() {
            std::ptr::copy_nonoverlapping(units.as_ptr(), p, units.len());
            GlobalUnlock(mem);
            SetClipboardData(CF_UNICODETEXT, mem as HANDLE);
        }
    }
    CloseClipboard();
}

unsafe fn paste(hwnd: HWND, v: &mut View) {
    if OpenClipboard(hwnd) == 0 {
        return;
    }
    let h = GetClipboardData(CF_UNICODETEXT);
    if !h.is_null() {
        let p = GlobalLock(h as HGLOBAL) as *const u16;
        if !p.is_null() {
            let mut n = 0;
            while *p.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            GlobalUnlock(h as HGLOBAL);
            v.ed.insert(&s);
        }
    }
    CloseClipboard();
}

/// EDIT-compatible text access in UTF-16 units.
unsafe fn get_text(v: &View, cap: usize, out: *mut u16) -> usize {
    if cap == 0 || out.is_null() {
        return 0;
    }
    let mut n = 0;
    let mut b = [0u16; 2];
    'outer: for c in v.ed.buffer().chars_from(0) {
        for &u in c.encode_utf16(&mut b).iter() {
            if n + 1 >= cap {
                break 'outer;
            }
            *out.add(n) = u;
            n += 1;
        }
    }
    *out.add(n) = 0;
    n
}

unsafe fn wide_arg(lp: LPARAM) -> String {
    let p = lp as *const u16;
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    while *p.add(n) != 0 {
        n += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let view = Box::new(Shared {
            size: Cell::new((0, 0)),
            focused: Cell::new(false),
            line_h: Cell::new(16),
            stale_layout: Cell::new(false),
            view: RefCell::new(View {
                ed: Editor::new(Buffer::new(), NATIVE_EOL),
                font: GetStockObject(SYSTEM_FIXED_FONT) as HFONT,
                cell_w: 8,
                line_h: 16,
                width: 0,
                height: 0,
                focused: false,
                dragging: false,
                high: None,
                back: null_mut(),
                back_size: (0, 0),
            }),
        });
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(view) as isize);
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let Some(sh) = shared(hwnd) else {
        return DefWindowProcW(hwnd, msg, wp, lp);
    };
    if msg == WM_NCDESTROY {
        let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Shared;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        let sh = Box::from_raw(p);
        let back = sh.view.borrow().back;
        if !back.is_null() {
            DeleteObject(back as HGDIOBJ);
        }
        return DefWindowProcW(hwnd, msg, wp, lp);
    }

    // Messages that can arrive re-entrantly only touch the plain cells.
    match msg {
        WM_ERASEBKGND => return 1,
        WM_SIZE => {
            sh.size
                .set(((lp & 0xFFFF) as i32, ((lp >> 16) & 0xFFFF) as i32));
            sh.stale_layout.set(true);
            finish(hwnd, sh);
            return 0;
        }
        WM_SETFOCUS => {
            sh.focused.set(true);
            CreateCaret(hwnd, null_mut(), 2, sh.line_h.get());
            ShowCaret(hwnd);
            finish(hwnd, sh);
            return 0;
        }
        WM_KILLFOCUS => {
            sh.focused.set(false);
            DestroyCaret();
            return 0;
        }
        _ => {}
    }

    // Everything else may edit; notify the parent after the borrow ends.
    let (result, changed, moved) = match sh.view.try_borrow_mut() {
        Ok(mut v) => {
            sync(&mut v, sh);
            let before = (v.ed.version(), v.ed.anchor(), v.ed.caret());
            let r = handle(hwnd, &mut v, msg, wp, lp);
            sh.line_h.set(v.line_h);
            let after = (v.ed.version(), v.ed.anchor(), v.ed.caret());
            (r, after.0 != before.0, after != before)
        }
        Err(_) => (None, false, false),
    };
    match result {
        Some(r) => {
            if msg != WM_PAINT {
                finish(hwnd, sh);
            }
            if changed && msg != WM_SETTEXT {
                notify_change(hwnd);
            }
            if moved {
                notify(hwnd, crate::ids::VN_CARET);
            }
            r
        }
        None => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Handles `msg`; `None` means "use DefWindowProc".
unsafe fn handle(hwnd: HWND, v: &mut View, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    let r = match msg {
        WM_PAINT => {
            paint(hwnd, v);
            return Some(0);
        }
        WM_SETFONT => {
            v.font = wp as HFONT;
            measure(v);
            layout(v);
            if v.focused {
                DestroyCaret();
                CreateCaret(hwnd, null_mut(), 2, v.line_h);
                ShowCaret(hwnd);
            }
            0
        }
        WM_GETFONT => return Some(v.font as LRESULT),
        WM_GETDLGCODE => {
            return Some((DLGC_WANTALLKEYS | DLGC_WANTCHARS | DLGC_WANTARROWS) as LRESULT)
        }
        WM_KEYDOWN => {
            if !key_down(hwnd, v, wp as u16) {
                return None;
            }
            0
        }
        WM_CHAR => {
            let u = wp as u16;
            match u {
                0xD800..=0xDBFF => {
                    v.high = Some(u);
                    return Some(0);
                }
                0xDC00..=0xDFFF => {
                    if let Some(h) = v.high.take() {
                        v.ed.insert(&String::from_utf16_lossy(&[h, u]));
                    }
                }
                0x0D => v.ed.newline(),
                0x09 => v.ed.insert("\t"),
                0..=0x1F | 0x7F => return Some(0),
                _ => {
                    if let Some(ch) = char::from_u32(u as u32) {
                        v.ed.insert(ch.encode_utf8(&mut [0; 4]));
                    }
                }
            }
            0
        }
        WM_LBUTTONDOWN => {
            SetFocus(hwnd);
            let (x, y) = lparam_point(lp);
            let at = hit(v, x, y);
            let anchor = if wp & MK_SHIFT != 0 {
                v.ed.anchor()
            } else {
                at
            };
            v.ed.set_selection(anchor, at);
            v.dragging = true;
            SetCapture(hwnd);
            SetTimer(hwnd, DRAG_TIMER, 50, None);
            0
        }
        WM_LBUTTONDBLCLK => {
            let (x, y) = lparam_point(lp);
            let at = hit(v, x, y);
            v.ed.select_word_at(at);
            0
        }
        WM_MOUSEMOVE if v.dragging => {
            let (x, y) = lparam_point(lp);
            let at = hit(v, x, y);
            let anchor = v.ed.anchor();
            v.ed.set_selection(anchor, at);
            0
        }
        WM_LBUTTONUP | WM_CAPTURECHANGED if v.dragging => {
            v.dragging = false;
            KillTimer(hwnd, DRAG_TIMER);
            if msg == WM_LBUTTONUP {
                ReleaseCapture();
            }
            0
        }
        WM_TIMER if wp == DRAG_TIMER && v.dragging => {
            let mut pt: POINT = zeroed();
            GetCursorPos(&mut pt);
            ScreenToClient(hwnd, &mut pt);
            if pt.y < 0 {
                v.ed.scroll_rows(-1);
            } else if pt.y >= v.height {
                v.ed.scroll_rows(1);
            }
            if pt.x < 0 {
                v.ed.scroll_cols(-4);
            } else if pt.x >= v.width {
                v.ed.scroll_cols(4);
            }
            let at = hit(v, pt.x, pt.y.min(v.height - 1));
            let anchor = v.ed.anchor();
            v.ed.set_selection(anchor, at);
            0
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp >> 16) as i16) as i64;
            let mut lines: u32 = 3;
            SystemParametersInfoW(SPI_GETWHEELSCROLLLINES, 0, &mut lines as *mut u32 as _, 0);
            v.ed.scroll_rows(-delta * lines.max(1) as i64 / 120);
            0
        }
        WM_MOUSEHWHEEL => {
            let delta = ((wp >> 16) as i16) as i64;
            v.ed.scroll_cols(delta * 4 / 120);
            0
        }
        WM_VSCROLL => {
            let rows = v.ed.view_rows() as i64;
            match (wp & 0xFFFF) as i32 {
                SB_LINEUP => v.ed.scroll_rows(-1),
                SB_LINEDOWN => v.ed.scroll_rows(1),
                SB_PAGEUP => v.ed.scroll_rows(-(rows - 1).max(1)),
                SB_PAGEDOWN => v.ed.scroll_rows((rows - 1).max(1)),
                SB_TOP => v.ed.scroll_to_line(0),
                SB_BOTTOM => v.ed.scroll_to_line(usize::MAX),
                SB_THUMBTRACK | SB_THUMBPOSITION => {
                    let mut si: SCROLLINFO = zeroed();
                    si.cbSize = size_of::<SCROLLINFO>() as u32;
                    si.fMask = SIF_TRACKPOS;
                    GetScrollInfo(hwnd, SB_VERT, &mut si);
                    let s = v_scale(v.ed.buffer().line_count());
                    v.ed.scroll_to_line(si.nTrackPos.max(0) as usize * s);
                }
                _ => {}
            }
            0
        }
        WM_HSCROLL => {
            let cols = (v.width / v.cell_w).max(1) as i64;
            match (wp & 0xFFFF) as i32 {
                SB_LINELEFT => v.ed.scroll_cols(-1),
                SB_LINERIGHT => v.ed.scroll_cols(1),
                SB_PAGELEFT => v.ed.scroll_cols(-cols),
                SB_PAGERIGHT => v.ed.scroll_cols(cols),
                SB_LEFT => v.ed.set_left(0),
                SB_THUMBTRACK | SB_THUMBPOSITION => {
                    let mut si: SCROLLINFO = zeroed();
                    si.cbSize = size_of::<SCROLLINFO>() as u32;
                    si.fMask = SIF_TRACKPOS;
                    GetScrollInfo(hwnd, SB_HORZ, &mut si);
                    v.ed.set_left(si.nTrackPos.max(0) as usize);
                }
                _ => {}
            }
            0
        }
        // ---- EDIT-compatible messages ----
        WM_GETTEXTLENGTH => v.ed.utf16_len() as LRESULT,
        WM_GETTEXT => return Some(get_text(v, wp, lp as *mut u16) as LRESULT),
        WM_SETTEXT => {
            let wrap = v.ed.wrap();
            v.ed = Editor::new(Buffer::from_text(&wide_arg(lp)), NATIVE_EOL);
            v.ed.set_wrap(wrap);
            layout(v);
            1
        }
        EM_GETSEL => {
            let sel = v.ed.selection();
            let (s, e) = (v.ed.utf16_offset(sel.start), v.ed.utf16_offset(sel.end));
            if wp != 0 {
                *(wp as *mut u32) = s as u32;
            }
            if lp != 0 {
                *(lp as *mut u32) = e as u32;
            }
            ((s & 0xFFFF) | ((e & 0xFFFF) << 16)) as LRESULT
        }
        EM_SETSEL => {
            let (s, e) = (wp as isize, lp);
            if s == -1 {
                let caret = v.ed.caret();
                v.ed.set_selection(caret, caret);
            } else if s == 0 && e == -1 {
                v.ed.select_all();
            } else {
                let a = v.ed.offset_of_utf16(s.max(0) as usize);
                let b = if e < 0 {
                    v.ed.buffer().len()
                } else {
                    v.ed.offset_of_utf16(e as usize)
                };
                v.ed.set_selection(a, b);
            }
            0
        }
        EM_REPLACESEL => {
            v.ed.insert(&wide_arg(lp));
            0
        }
        EM_SCROLLCARET => {
            v.ed.ensure_visible();
            0
        }
        EM_EMPTYUNDOBUFFER => {
            v.ed.clear_undo();
            0
        }
        EM_CANUNDO => v.ed.can_undo() as LRESULT,
        EM_UNDO | WM_UNDO => v.ed.undo() as LRESULT,
        EM_SETLIMITTEXT => 0,
        WM_CUT => {
            copy(hwnd, v, true);
            0
        }
        WM_COPY => {
            copy(hwnd, v, false);
            0
        }
        WM_PASTE => {
            paste(hwnd, v);
            0
        }
        WM_CLEAR => {
            if !v.ed.selection().is_empty() {
                v.ed.delete_forward(false);
            }
            0
        }
        _ => return None,
    };
    Some(r)
}
