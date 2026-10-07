//! Win32 text view ("FoxingText"): paints rows from [`Editor`] and turns input into
//! editor calls. Also answers the EDIT messages automation relies on (`WM_GETTEXT`,
//! `EM_GETSEL`, `EM_SETSEL`, `EM_REPLACESEL`, ...) and sends `EN_CHANGE` to its parent.

use crate::gdi::{self, BackBuffer, Gdi};
use foxing::buffer::Buffer;
use foxing::document::NATIVE_EOL;
use foxing::editor::{Editor, Motion};
use foxing::ui::scroll::{ScrollAction, ScrollBar};
use foxing::ui::{Cmd, DrawList, Rect, Theme};
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
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
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
    gdi: Gdi,
    back: BackBuffer,
    theme: Theme,
    vbar: ScrollBar,
    hbar: ScrollBar,
    /// Which scrollbar holds the mouse: Some(true) vertical, Some(false) horizontal.
    bar_drag: Option<bool>,
    tracking_leave: bool,
    /// Set by a handler that changed nothing visible, so no refresh is needed.
    quiet: bool,
    /// What was last made visible; `None` forces a full repaint.
    last: Option<Frame>,
}

/// The visible state that decides how much to repaint.
#[derive(Debug, Clone, PartialEq)]
struct Frame {
    size: (i32, i32),
    top: usize,
    left: usize,
    rows: usize,
    wrap: bool,
    lines: usize,
    version: u64,
    edit_line: usize,
    sel: std::ops::Range<usize>,
    caret_line: usize,
    vbar: (u64, u64, u64),
    hbar: (u64, u64, u64),
}

impl Frame {
    fn of(v: &View) -> Frame {
        Frame {
            size: (v.width, v.height),
            top: v.ed.top_line(),
            left: v.ed.left(),
            rows: v.ed.view_rows(),
            wrap: v.ed.wrap(),
            lines: v.ed.buffer().line_count(),
            version: v.ed.version(),
            edit_line: v.ed.last_edit_line(),
            sel: v.ed.selection(),
            caret_line: v.ed.buffer().line_of(v.ed.caret()),
            vbar: v.vbar.state(),
            hbar: v.hbar.state(),
        }
    }
}

/// Lines to repaint going from `old` to `new`, or `None` for everything. Partial
/// repaints cover the common typing case: no scroll, no resize, no wrap, no line
/// count change, no selection, and any edit on the caret's old or new line.
fn dirty_lines(old: &Frame, new: &Frame) -> Option<Vec<usize>> {
    let same_view = old.size == new.size
        && old.top == new.top
        && old.left == new.left
        && old.rows == new.rows
        && !old.wrap
        && !new.wrap
        && old.lines == new.lines
        && old.sel.is_empty()
        && new.sel.is_empty();
    if !same_view {
        return None;
    }
    let lines = vec![old.caret_line, new.caret_line];
    if old.version != new.version && !lines.contains(&new.edit_line) {
        return None;
    }
    Some(lines)
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
    /// An IME composition is in progress.
    composing: Cell<bool>,
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
        WS_CHILD | WS_VISIBLE,
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

/// Lays out, syncs the scrollbars to the editor, positions the caret, and repaints.
/// Safe to call re-entrantly: it does nothing while the view is borrowed (the outer
/// call finishes).
unsafe fn finish(hwnd: HWND, sh: &Shared) {
    let (caret, dirty) = match sh.view.try_borrow_mut() {
        Ok(mut v) => {
            sync(&mut v, sh);
            sh.stale_layout.set(false);
            layout(&mut v);
            let frame = Frame::of(&v);
            let dirty = v.last.as_ref().and_then(|old| {
                let lines = dirty_lines(old, &frame)?;
                let tr = text_rect(&v);
                let mut rects: Vec<Rect> = lines
                    .iter()
                    .filter(|&&l| l >= frame.top && l < frame.top + frame.rows)
                    .map(|&l| Rect::new(0, (l - frame.top) as i32 * v.line_h, tr.w, v.line_h))
                    .collect();
                if old.vbar != frame.vbar && v.vbar.visible() {
                    rects.push(vbar_rect(&v));
                }
                if old.hbar != frame.hbar && v.hbar.visible() {
                    rects.push(hbar_rect(&v));
                }
                Some(rects)
            });
            v.last = Some(frame);
            (caret_px(&v), dirty)
        }
        Err(_) => return,
    };
    if sh.focused.get() {
        let (x, y) = caret.unwrap_or((-1000, -1000));
        SetCaretPos(x, y);
        // The IME window only matters while composing; positioning it costs ~0.3 ms,
        // too much to pay on every keystroke.
        if sh.composing.get() {
            place_ime(hwnd, x, y);
        }
    }
    match dirty {
        Some(rects) => {
            for r in rects {
                let rc = RECT {
                    left: r.x,
                    top: r.y,
                    right: r.right(),
                    bottom: r.bottom(),
                };
                InvalidateRect(hwnd, &rc, 0);
            }
        }
        None => {
            InvalidateRect(hwnd, null(), 0);
        }
    }
}

/// Puts the IME composition window at the caret.
unsafe fn place_ime(hwnd: HWND, x: i32, y: i32) {
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
        v.last = None;
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

unsafe fn measure(hwnd: HWND, v: &mut View) {
    let dc = GetDC(null_mut());
    let old = SelectObject(dc, v.font as HGDIOBJ);
    let mut tm: TEXTMETRICW = zeroed();
    GetTextMetricsW(dc, &mut tm);
    SelectObject(dc, old);
    ReleaseDC(null_mut(), dc);
    v.cell_w = tm.tmAveCharWidth.max(1);
    v.line_h = (tm.tmHeight + tm.tmExternalLeading).max(1);
    v.gdi = Gdi::new(v.font, v.font, GetDpiForWindow(hwnd).max(96));
}

fn bar_px(v: &View) -> i32 {
    ScrollBar::thickness(&v.gdi)
}

/// The text area: the client area minus visible scrollbars.
fn text_rect(v: &View) -> Rect {
    let b = bar_px(v);
    let w = v.width - if v.vbar.visible() { b } else { 0 };
    let h = v.height - if v.hbar.visible() { b } else { 0 };
    Rect::new(0, 0, w.max(0), h.max(0))
}

fn vbar_rect(v: &View) -> Rect {
    let b = bar_px(v);
    let h = v.height - if v.hbar.visible() { b } else { 0 };
    Rect::new(v.width - b, 0, b, h.max(0))
}

fn hbar_rect(v: &View) -> Rect {
    let b = bar_px(v);
    let w = v.width - if v.vbar.visible() { b } else { 0 };
    Rect::new(0, v.height - b, w.max(0), b)
}

/// Fits the editor's cell grid to the text area and syncs the scrollbars. Bar
/// visibility changes the text area, so this settles in at most a few passes.
fn layout(v: &mut View) {
    for _ in 0..3 {
        let before = (v.vbar.visible(), v.hbar.visible());
        let tr = text_rect(v);
        let rows = (tr.h / v.line_h).max(1) as usize;
        let cols = ((tr.w - PAD) / v.cell_w).max(1) as usize;
        v.ed.set_view(rows, cols);
        let lines = v.ed.buffer().line_count() as u64;
        v.vbar.set(lines, rows as u64, v.ed.top_line() as u64);
        if v.ed.wrap() {
            v.hbar.set(0, 1, 0);
        } else {
            let width = v.ed.visible_width().max(v.ed.left() + 1) as u64;
            v.hbar.set(
                width.max(v.ed.left() as u64 + cols as u64 / 2),
                cols as u64,
                v.ed.left() as u64,
            );
        }
        if (v.vbar.visible(), v.hbar.visible()) == before {
            break;
        }
    }
}

fn caret_px(v: &View) -> Option<(i32, i32)> {
    v.ed.caret_cell()
        .map(|(row, col)| (PAD + col as i32 * v.cell_w, row as i32 * v.line_h))
}

fn paint(hwnd: HWND, v: &mut View) {
    // Take the buffer out so the closure can read the rest of the view.
    let mut back = std::mem::replace(&mut v.back, BackBuffer::new());
    unsafe {
        back.paint(hwnd, |dc, _, _, clip| {
            let dl = draw(v, clip);
            gdi::render(dc, &dl, &v.gdi);
        })
    };
    v.back = back;
}

/// Draw list for the parts of the view that intersect `clip`.
fn draw(v: &View, clip: Rect) -> DrawList {
    let mut dl = DrawList::default();
    let tr = text_rect(v);
    let t = v.theme;
    dl.fill(
        Rect::new(
            clip.x,
            clip.y,
            clip.w.min(tr.w - clip.x),
            clip.h.min(tr.h - clip.y),
        ),
        t.text_bg,
    );
    let sel = v.ed.selection();
    for (i, row) in v.ed.visible_rows().iter().enumerate() {
        let y = i as i32 * v.line_h;
        if !clip.intersects(&Rect::new(0, y, tr.w, v.line_h)) {
            continue;
        }
        let glyphs = v.ed.glyphs(row);
        let mut g = 0;
        while g < glyphs.len() {
            let selected = sel.contains(&glyphs[g].at);
            let start = g;
            let mut chars = Vec::new();
            let mut advances = Vec::new();
            while g < glyphs.len() && sel.contains(&glyphs[g].at) == selected {
                let gl = glyphs[g];
                chars.push(match gl.ch {
                    '\t' => ' ',
                    c if (c as u32) < 0x20 || c == '\u{7f}' => '\u{FFFD}',
                    c => c,
                });
                advances.push(gl.cells as i32 * v.cell_w);
                g += 1;
            }
            dl.cmds.push(Cmd::Glyphs {
                x: PAD + glyphs[start].col as i32 * v.cell_w,
                y,
                color: if selected { t.sel_fg } else { t.text_fg },
                bg: selected.then_some((t.sel_bg, v.line_h)),
                chars,
                advances,
            });
        }
        // Show a selected line break as one highlighted cell after the text.
        let line_end = row.range.end;
        if row.brk && sel.start <= line_end && sel.end > line_end {
            let col = glyphs.last().map_or(0, |gl| gl.col + gl.cells) as i32;
            dl.fill(
                Rect::new(PAD + col * v.cell_w, y, v.cell_w, v.line_h),
                t.sel_bg,
            );
        }
    }
    if v.vbar.visible() && clip.intersects(&vbar_rect(v)) {
        v.vbar.paint(vbar_rect(v), &t, &v.gdi, &mut dl);
    }
    if v.hbar.visible() && clip.intersects(&hbar_rect(v)) {
        v.hbar.paint(hbar_rect(v), &t, &v.gdi, &mut dl);
    }
    if v.vbar.visible() && v.hbar.visible() {
        let b = bar_px(v);
        dl.fill(Rect::new(v.width - b, v.height - b, b, b), t.scroll_track);
    }
    dl
}

/// Buffer offset under a client point, clamped into the viewport.
fn hit(v: &View, x: i32, y: i32) -> usize {
    let row = (y.max(0) / v.line_h) as usize;
    let col = (((x - PAD).max(0) + v.cell_w / 2) / v.cell_w) as usize;
    v.ed.offset_at(row.min(v.ed.view_rows().saturating_sub(1)), col)
}

/// Which scrollbar is under the point in `lp`: `Some(true)` vertical, `Some(false)`
/// horizontal.
fn bar_hit(v: &View, lp: LPARAM) -> Option<bool> {
    let (x, y) = lparam_point(lp);
    if v.vbar.visible() && vbar_rect(v).contains(x, y) {
        Some(true)
    } else if v.hbar.visible() && hbar_rect(v).contains(x, y) {
        Some(false)
    } else {
        None
    }
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
            composing: Cell::new(false),
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
                gdi: Gdi::new(
                    GetStockObject(SYSTEM_FIXED_FONT) as HFONT,
                    GetStockObject(SYSTEM_FIXED_FONT) as HFONT,
                    GetDpiForWindow(GetParent(hwnd)).max(96),
                ),
                back: BackBuffer::new(),
                theme: Theme::LIGHT,
                vbar: ScrollBar::new(true),
                hbar: ScrollBar::new(false),
                bar_drag: None,
                tracking_leave: false,
                quiet: false,
                last: None,
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
        drop(Box::from_raw(p));
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
        WM_IME_STARTCOMPOSITION => {
            sh.composing.set(true);
            finish(hwnd, sh);
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        WM_IME_ENDCOMPOSITION => {
            sh.composing.set(false);
            return DefWindowProcW(hwnd, msg, wp, lp);
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
            let read_only = matches!(
                msg,
                WM_PAINT | WM_GETTEXT | WM_GETTEXTLENGTH | EM_GETSEL | EM_CANUNDO | WM_GETFONT
            );
            let quiet = std::mem::take(&mut v.quiet) || read_only;
            (r.map(|r| (r, quiet)), after.0 != before.0, after != before)
        }
        Err(_) => (None, false, false),
    };
    match result {
        Some((r, quiet)) => {
            if !quiet {
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
            measure(hwnd, v);
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
        WM_LBUTTONDOWN if bar_hit(v, lp).is_some() => {
            let (x, y) = lparam_point(lp);
            let vertical = bar_hit(v, lp) == Some(true);
            let (rect, page) = if vertical {
                (vbar_rect(v), v.ed.view_rows().max(2) as i64 - 1)
            } else {
                (hbar_rect(v), (text_rect(v).w / v.cell_w).max(1) as i64)
            };
            let bar = if vertical { &mut v.vbar } else { &mut v.hbar };
            let step = match bar.mouse_down(rect, &v.gdi, x, y) {
                Some(ScrollAction::PageBack) => -page,
                Some(ScrollAction::PageForward) => page,
                Some(ScrollAction::Grab) => {
                    v.bar_drag = Some(vertical);
                    SetCapture(hwnd);
                    0
                }
                None => 0,
            };
            if vertical {
                v.ed.scroll_rows(step);
            } else {
                v.ed.scroll_cols(step);
            }
            0
        }
        WM_MOUSEMOVE if v.bar_drag.is_some() => {
            let (x, y) = lparam_point(lp);
            if v.bar_drag == Some(true) {
                let r = vbar_rect(v);
                if let Some(pos) = v.vbar.mouse_move(r, &v.gdi, x, y) {
                    v.ed.scroll_to_line(pos as usize);
                }
            } else {
                let r = hbar_rect(v);
                if let Some(pos) = v.hbar.mouse_move(r, &v.gdi, x, y) {
                    v.ed.set_left(pos as usize);
                }
            }
            0
        }
        WM_LBUTTONUP | WM_CAPTURECHANGED if v.bar_drag.is_some() => {
            v.bar_drag = None;
            v.vbar.mouse_up();
            v.hbar.mouse_up();
            if msg == WM_LBUTTONUP {
                ReleaseCapture();
            }
            0
        }
        WM_MOUSEMOVE if !v.dragging => {
            let (x, y) = lparam_point(lp);
            let vh = v.vbar.visible() && v.vbar.thumb(vbar_rect(v), &v.gdi).contains(x, y);
            let hh = v.hbar.visible() && v.hbar.thumb(hbar_rect(v), &v.gdi).contains(x, y);
            let changed = v.vbar.set_hot(vh) | v.hbar.set_hot(hh);
            if (vh || hh) && !v.tracking_leave {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                v.tracking_leave = TrackMouseEvent(&mut tme) != 0;
            }
            v.quiet = !changed;
            0
        }
        WM_MOUSELEAVE => {
            v.tracking_leave = false;
            v.vbar.set_hot(false);
            v.hbar.set_hot(false);
            0
        }
        WM_SETCURSOR if (lp & 0xFFFF) as u32 == HTCLIENT => {
            let mut pt: POINT = zeroed();
            GetCursorPos(&mut pt);
            ScreenToClient(hwnd, &mut pt);
            let over_bar = (v.vbar.visible() && vbar_rect(v).contains(pt.x, pt.y))
                || (v.hbar.visible() && hbar_rect(v).contains(pt.x, pt.y));
            SetCursor(LoadCursorW(
                null_mut(),
                if over_bar { IDC_ARROW } else { IDC_IBEAM },
            ));
            return Some(1);
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
        WM_LBUTTONDBLCLK if bar_hit(v, lp).is_none() => {
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
            let tr = text_rect(v);
            if pt.y < 0 {
                v.ed.scroll_rows(-1);
            } else if pt.y >= tr.h {
                v.ed.scroll_rows(1);
            }
            if pt.x < 0 {
                v.ed.scroll_cols(-4);
            } else if pt.x >= tr.w {
                v.ed.scroll_cols(4);
            }
            let at = hit(v, pt.x, pt.y.min(tr.h - 1));
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
            v.last = None;
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
        EM_GETFIRSTVISIBLELINE => {
            v.quiet = true;
            v.ed.top_line() as LRESULT
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

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> Frame {
        Frame {
            size: (800, 600),
            top: 10,
            left: 0,
            rows: 30,
            wrap: false,
            lines: 100,
            version: 1,
            edit_line: 12,
            sel: 5..5,
            caret_line: 12,
            vbar: (100, 30, 10),
            hbar: (80, 70, 0),
        }
    }

    #[test]
    fn typing_on_the_caret_line_repaints_that_line() {
        let old = frame();
        let new = Frame {
            version: 2,
            sel: 6..6,
            ..frame()
        };
        assert_eq!(dirty_lines(&old, &new), Some(vec![12, 12]));
    }

    #[test]
    fn caret_moving_between_lines_repaints_both() {
        let new = Frame {
            caret_line: 13,
            sel: 9..9,
            ..frame()
        };
        assert_eq!(dirty_lines(&frame(), &new), Some(vec![12, 13]));
    }

    #[test]
    fn anything_else_repaints_everything() {
        let base = frame();
        for new in [
            Frame { top: 11, ..frame() },
            Frame { left: 3, ..frame() },
            Frame {
                size: (801, 600),
                ..frame()
            },
            Frame {
                lines: 101,
                version: 2,
                ..frame()
            },
            Frame {
                wrap: true,
                ..frame()
            },
            Frame {
                sel: 5..9,
                ..frame()
            },
            // An edit away from the caret (e.g. replace-all) is not local.
            Frame {
                version: 2,
                edit_line: 40,
                ..frame()
            },
        ] {
            assert_eq!(dirty_lines(&base, &new), None, "{new:?}");
        }
    }
}
