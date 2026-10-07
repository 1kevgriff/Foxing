//! Editing model on top of [`Buffer`]: selection, movement, edits, undo, word-wrap
//! layout and scrolling. Platform-neutral; shells only paint rows and forward input.
//!
//! Layout is a fixed cell grid: most chars take one cell, wide (CJK, emoji) take two,
//! combining marks take none, and tabs advance to the next multiple of [`TAB`].

use crate::buffer::Buffer;
use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

pub const TAB: usize = 8;

/// Display cells for a non-tab char.
pub fn char_cells(c: char) -> usize {
    let u = c as u32;
    let zero = matches!(u,
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x200B..=0x200F
        | 0x20D0..=0x20FF | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F);
    let wide = matches!(u,
        0x1100..=0x115F | 0x2E80..=0x303E | 0x3041..=0x33FF | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF | 0xA000..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x1F300..=0x1F64F
        | 0x1F900..=0x1F9FF | 0x20000..=0x3FFFD);
    if zero {
        0
    } else if wide {
        2
    } else {
        1
    }
}

fn advance(col: usize, c: char) -> usize {
    if c == '\t' {
        (col / TAB + 1) * TAB
    } else {
        col + char_cells(c)
    }
}

/// Word chars for word movement and double-click selection.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// One glyph to draw in a row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glyph {
    pub ch: char,
    /// Byte offset in the buffer.
    pub at: usize,
    /// Starting cell, relative to the row's first visible column.
    pub col: usize,
    pub cells: usize,
}

/// A visual row: part (or all) of one line.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub line: usize,
    /// Byte range of the row's text (excludes the line break).
    pub range: Range<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Motion {
    Left,
    Right,
    WordLeft,
    WordRight,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    DocStart,
    DocEnd,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Typing,
    Other,
}

#[derive(Debug, Clone)]
struct Edit {
    at: usize,
    removed: String,
    inserted: String,
    sel_before: (usize, usize),
    kind: Kind,
}

pub struct Editor {
    buf: Buffer,
    anchor: usize,
    head: usize,
    eol: &'static str,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    /// Column kept across vertical moves.
    goal: Option<usize>,
    wrap: bool,
    /// Viewport size in cells.
    rows: usize,
    cols: usize,
    /// First visible row: (line, row within line).
    top: (usize, usize),
    /// First visible column (no-wrap only).
    left: usize,
    /// Wrap layouts for recently used lines: (line, row start offsets).
    layouts: RefCell<Vec<(usize, Rc<[usize]>)>>,
    /// Bumped on every text change.
    version: u64,
}

impl Editor {
    pub fn new(buf: Buffer, default_eol: &'static str) -> Self {
        let eol = buf.detect_eol().unwrap_or(default_eol);
        Editor {
            buf,
            anchor: 0,
            head: 0,
            eol,
            undo: Vec::new(),
            redo: Vec::new(),
            goal: None,
            wrap: false,
            rows: 1,
            cols: 1,
            top: (0, 0),
            left: 0,
            layouts: RefCell::new(Vec::new()),
            version: 0,
        }
    }

    pub fn buffer(&self) -> &Buffer {
        &self.buf
    }

    pub fn eol(&self) -> &'static str {
        self.eol
    }

    pub fn selection(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    pub fn anchor(&self) -> usize {
        self.anchor
    }

    pub fn caret(&self) -> usize {
        self.head
    }

    /// 1-based line and column of the caret; the column counts chars (a tab is one).
    pub fn caret_line_col(&self) -> (usize, usize) {
        let line = self.buf.line_of(self.head);
        let start = self.buf.line_start(line);
        let mut p = start;
        let col = self
            .buf
            .chars_from(start)
            .take_while(|c| {
                let before = p < self.head;
                p += c.len_utf8();
                before
            })
            .count();
        (line + 1, col + 1)
    }

    pub fn selected_text(&self) -> String {
        self.buf.slice(self.selection())
    }

    pub fn set_selection(&mut self, anchor: usize, head: usize) {
        let len = self.buf.len();
        self.anchor = self.snap(anchor.min(len));
        self.head = self.snap(head.min(len));
        self.goal = None;
        self.ensure_visible();
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.head = self.buf.len();
        self.goal = None;
    }

    /// Moves back to a char boundary.
    fn snap(&self, mut pos: usize) -> usize {
        while !self.buf.is_char_boundary(pos) {
            pos -= 1;
        }
        pos
    }

    // ---- editing ----

    /// Replaces the selection with `text` (line breaks converted to the file's EOL).
    pub fn insert(&mut self, text: &str) {
        let text = if text.contains('\r') || (self.eol != "\n" && text.contains('\n')) {
            crate::text::with_eol(&crate::text::to_lf(text), self.eol)
        } else {
            text.to_owned()
        };
        let typing = !text.is_empty() && !text.contains('\n') && text.chars().count() == 1;
        self.replace(
            self.selection(),
            &text,
            if typing { Kind::Typing } else { Kind::Other },
        );
    }

    pub fn newline(&mut self) {
        let eol = self.eol;
        self.replace(self.selection(), eol, Kind::Other);
    }

    pub fn backspace(&mut self, word: bool) {
        if self.anchor == self.head {
            let to = self.motion_target(if word { Motion::WordLeft } else { Motion::Left });
            self.anchor = to;
        }
        self.replace(self.selection(), "", Kind::Other);
    }

    pub fn delete_forward(&mut self, word: bool) {
        if self.anchor == self.head {
            let to = self.motion_target(if word {
                Motion::WordRight
            } else {
                Motion::Right
            });
            self.head = to;
        }
        self.replace(self.selection(), "", Kind::Other);
    }

    /// Removes and returns the selection.
    pub fn cut(&mut self) -> String {
        let s = self.selected_text();
        self.replace(self.selection(), "", Kind::Other);
        s
    }

    fn replace(&mut self, range: Range<usize>, text: &str, kind: Kind) {
        if range.is_empty() && text.is_empty() {
            return;
        }
        let removed = self.buf.slice(range.clone());
        let sel_before = (self.anchor, self.head);
        self.apply(range.start, &removed, text);
        let coalesce = kind == Kind::Typing
            && matches!(self.undo.last(), Some(e) if e.kind == Kind::Typing
                && removed.is_empty() && e.at + e.inserted.len() == range.start);
        if coalesce {
            self.undo
                .last_mut()
                .expect("checked")
                .inserted
                .push_str(text);
        } else {
            self.undo.push(Edit {
                at: range.start,
                removed,
                inserted: text.to_owned(),
                sel_before,
                kind,
            });
        }
        self.redo.clear();
        self.anchor = range.start + text.len();
        self.head = self.anchor;
        self.goal = None;
        self.ensure_visible();
    }

    fn apply(&mut self, at: usize, removed: &str, inserted: &str) {
        self.buf.delete(at..at + removed.len());
        self.buf.insert(at, inserted);
        self.layouts.borrow_mut().clear();
        self.version += 1;
    }

    /// Changes whenever the text changes.
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        let Some(e) = self.undo.pop() else {
            return false;
        };
        self.apply(e.at, &e.inserted, &e.removed);
        (self.anchor, self.head) = e.sel_before;
        self.goal = None;
        self.redo.push(e);
        self.ensure_visible();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(mut e) = self.redo.pop() else {
            return false;
        };
        self.apply(e.at, &e.removed, &e.inserted);
        self.anchor = e.at + e.inserted.len();
        self.head = self.anchor;
        self.goal = None;
        e.kind = Kind::Other;
        self.undo.push(e);
        self.ensure_visible();
        true
    }

    pub fn clear_undo(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    /// Selects the next match. Returns false (selection unchanged) if none.
    pub fn find(&mut self, needle: &str, forward: bool, match_case: bool) -> bool {
        let sel = self.selection();
        let from = if forward { sel.end } else { sel.start };
        match self.buf.find(needle, from, forward, match_case) {
            Some(m) => {
                self.anchor = m.start;
                self.head = m.end;
                self.goal = None;
                self.ensure_visible();
                true
            }
            None => false,
        }
    }

    // ---- movement ----

    pub fn move_caret(&mut self, m: Motion, extend: bool) {
        let sel = self.selection();
        let collapse = !extend && !sel.is_empty();
        let to = match m {
            Motion::Left if collapse => sel.start,
            Motion::Right if collapse => sel.end,
            _ => self.motion_target(m),
        };
        if !matches!(
            m,
            Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown
        ) {
            self.goal = None;
        }
        self.head = to;
        if !extend {
            self.anchor = to;
        }
        self.ensure_visible();
    }

    fn motion_target(&mut self, m: Motion) -> usize {
        let pos = self.head;
        let len = self.buf.len();
        match m {
            Motion::Left => self.prev_char(pos),
            Motion::Right => self.next_char(pos),
            Motion::WordLeft => {
                let mut p = pos;
                // Skip non-word chars, then the word.
                while p > 0 && !self.char_before(p).is_some_and(is_word) {
                    p = self.prev_char(p);
                }
                while p > 0 && self.char_before(p).is_some_and(is_word) {
                    p = self.prev_char(p);
                }
                p
            }
            Motion::WordRight => {
                let mut p = pos;
                while p < len && self.char_at(p).is_some_and(is_word) {
                    p = self.next_char(p);
                }
                while p < len && !self.char_at(p).is_some_and(is_word) {
                    p = self.next_char(p);
                }
                p
            }
            Motion::Home => {
                let (line, row) = self.row_of(pos);
                self.rows_of(line)[row]
            }
            Motion::End => {
                let (line, row) = self.row_of(pos);
                self.row_range(line, row).end
            }
            Motion::DocStart => 0,
            Motion::DocEnd => len,
            Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown => {
                let at = self.row_of(pos);
                let goal = match self.goal {
                    Some(g) => g,
                    None => {
                        let g = col_in(&self.buf, self.row_range(at.0, at.1).start, pos);
                        self.goal = Some(g);
                        g
                    }
                };
                let page = self.rows.max(2) as i64 - 1;
                let delta = match m {
                    Motion::Up => -1,
                    Motion::Down => 1,
                    Motion::PageUp => -page,
                    _ => page,
                };
                let to = self.offset_rows(at, delta);
                if to == at {
                    // Already on the first/last row: go to the document edge.
                    return if delta > 0 { len } else { 0 };
                }
                offset_at_col(&self.buf, self.row_range(to.0, to.1), goal)
            }
        }
    }

    fn char_at(&self, pos: usize) -> Option<char> {
        self.buf.chars_from(pos).next()
    }

    fn char_before(&self, pos: usize) -> Option<char> {
        (pos > 0)
            .then(|| self.char_at(self.prev_char(pos)))
            .flatten()
    }

    /// Previous char boundary; treats `\r\n` as one step.
    fn prev_char(&self, pos: usize) -> usize {
        if pos == 0 {
            return 0;
        }
        let mut p = pos - 1;
        while !self.buf.is_char_boundary(p) {
            p -= 1;
        }
        if p > 0 && self.buf.byte_at(p) == b'\n' && self.buf.byte_at(p - 1) == b'\r' {
            p -= 1;
        }
        p
    }

    /// Next char boundary; treats `\r\n` as one step.
    fn next_char(&self, pos: usize) -> usize {
        let len = self.buf.len();
        if pos >= len {
            return len;
        }
        if self.buf.byte_at(pos) == b'\r' && pos + 1 < len && self.buf.byte_at(pos + 1) == b'\n' {
            return pos + 2;
        }
        pos + self.char_at(pos).map_or(1, char::len_utf8)
    }

    /// Selects the word (or run of non-word chars) around `pos`.
    pub fn select_word_at(&mut self, pos: usize) {
        let line = self.buf.line_of(pos);
        let r = self.buf.line_range(line);
        let word = self.char_at(pos).is_some_and(is_word);
        let mut s = pos.min(r.end);
        while s > r.start && self.char_before(s).is_some_and(is_word) == word {
            s = self.prev_char(s);
        }
        let mut e = pos.min(r.end);
        while e < r.end && self.char_at(e).is_some_and(is_word) == word {
            e = self.next_char(e);
        }
        self.anchor = s;
        self.head = e;
        self.goal = None;
    }

    // ---- layout ----

    pub fn wrap(&self) -> bool {
        self.wrap
    }

    pub fn set_wrap(&mut self, wrap: bool) {
        self.wrap = wrap;
        self.left = 0;
        self.layouts.borrow_mut().clear();
        self.top = (self.top.0, 0);
        self.ensure_visible();
    }

    /// Viewport size in cells.
    pub fn set_view(&mut self, rows: usize, cols: usize) {
        let cols = cols.max(1);
        if self.wrap && cols != self.cols {
            self.layouts.borrow_mut().clear();
            self.top = (self.top.0, 0);
        }
        self.rows = rows.max(1);
        self.cols = cols;
    }

    /// Row start offsets (absolute) for `line`; one row unless wrapping.
    fn rows_of(&self, line: usize) -> Rc<[usize]> {
        if !self.wrap {
            return Rc::from([self.buf.line_start(line)]);
        }
        if let Some((_, r)) = self.layouts.borrow().iter().find(|(l, _)| *l == line) {
            return r.clone();
        }
        let rows: Rc<[usize]> = wrap_line(&self.buf, self.buf.line_range(line), self.cols).into();
        let mut cache = self.layouts.borrow_mut();
        if cache.len() >= 512 {
            cache.remove(0);
        }
        cache.push((line, rows.clone()));
        rows
    }

    fn row_range(&self, line: usize, row: usize) -> Range<usize> {
        let rows = self.rows_of(line);
        let end = rows
            .get(row + 1)
            .copied()
            .unwrap_or_else(|| self.buf.line_range(line).end);
        rows[row]..end
    }

    /// (line, row) holding `pos`. A position at a wrap boundary belongs to the next row.
    fn row_of(&self, pos: usize) -> (usize, usize) {
        let line = self.buf.line_of(pos);
        if !self.wrap {
            return (line, 0);
        }
        let rows = self.rows_of(line);
        (line, rows.partition_point(|&s| s <= pos) - 1)
    }

    /// Moves `delta` rows from `at`, clamped to the document.
    fn offset_rows(&self, at: (usize, usize), delta: i64) -> (usize, usize) {
        let (mut line, mut row) = at;
        let mut left = delta.unsigned_abs();
        if delta > 0 {
            while left > 0 {
                let n = self.rows_of(line).len();
                if row + 1 < n {
                    let step = ((n - 1 - row) as u64).min(left);
                    row += step as usize;
                    left -= step;
                } else if line + 1 < self.buf.line_count() {
                    line += 1;
                    row = 0;
                    left -= 1;
                    if !self.wrap {
                        // One row per line: jump straight there.
                        let step = left.min((self.buf.line_count() - 1 - line) as u64);
                        line += step as usize;
                        left -= step;
                    }
                } else {
                    break;
                }
            }
        } else {
            while left > 0 {
                if row > 0 {
                    let step = (row as u64).min(left);
                    row -= step as usize;
                    left -= step;
                } else if line > 0 {
                    if !self.wrap {
                        let step = left.min(line as u64);
                        line -= step as usize;
                        left -= step;
                        continue;
                    }
                    line -= 1;
                    row = self.rows_of(line).len() - 1;
                    left -= 1;
                } else {
                    break;
                }
            }
        }
        (line, row)
    }

    // ---- viewport ----

    pub fn view_rows(&self) -> usize {
        self.rows
    }

    pub fn left(&self) -> usize {
        self.left
    }

    pub fn top_line(&self) -> usize {
        self.top.0
    }

    pub fn scroll_rows(&mut self, delta: i64) {
        self.top = self.offset_rows(self.top, delta);
    }

    pub fn scroll_to_line(&mut self, line: usize) {
        self.top = (line.min(self.buf.line_count() - 1), 0);
    }

    pub fn scroll_cols(&mut self, delta: i64) {
        if !self.wrap {
            self.left = self.left.saturating_add_signed(delta as isize);
        }
    }

    pub fn set_left(&mut self, left: usize) {
        if !self.wrap {
            self.left = left;
        }
    }

    /// Scrolls so the caret is inside the viewport.
    pub fn ensure_visible(&mut self) {
        let caret = self.row_of(self.head);
        if caret < self.top {
            self.top = caret;
        } else {
            // Is the caret within `rows` rows of top?
            let last = self.offset_rows(self.top, self.rows as i64 - 1);
            if caret > last {
                self.top = self.offset_rows(caret, -(self.rows as i64 - 1));
            }
        }
        if !self.wrap {
            let (line, row) = caret;
            let col = col_in(&self.buf, self.row_range(line, row).start, self.head);
            let margin = (self.cols / 4).min(8);
            if col < self.left {
                self.left = col.saturating_sub(margin);
            } else if col >= self.left + self.cols {
                self.left = col + 1 + margin - self.cols.min(col + 1 + margin);
            }
        }
    }

    /// Rows currently in the viewport, top to bottom.
    pub fn visible_rows(&self) -> Vec<Row> {
        let mut out = Vec::with_capacity(self.rows);
        let (mut line, mut row) = self.top;
        let lines = self.buf.line_count();
        while out.len() < self.rows && line < lines {
            let n = self.rows_of(line).len();
            while row < n && out.len() < self.rows {
                out.push(Row {
                    line,
                    range: self.row_range(line, row),
                });
                row += 1;
            }
            line += 1;
            row = 0;
        }
        out
    }

    /// Glyphs of `row` within the visible columns.
    pub fn glyphs(&self, row: &Row) -> Vec<Glyph> {
        let mut out = Vec::new();
        let mut col = 0;
        let right = self.left + self.cols;
        let mut pos = row.range.start;
        for c in self.buf.chars_from(row.range.start) {
            if pos >= row.range.end || col >= right {
                break;
            }
            let next = advance(col, c);
            if next > self.left {
                out.push(Glyph {
                    ch: c,
                    at: pos,
                    col: col.saturating_sub(self.left),
                    cells: next - col.max(self.left),
                });
            }
            col = next;
            pos += c.len_utf8();
        }
        out
    }

    /// Caret position in the viewport as (row index, column), if visible.
    pub fn caret_cell(&self) -> Option<(usize, usize)> {
        let (line, row) = self.row_of(self.head);
        let rows = self.visible_rows();
        let idx = rows
            .iter()
            .position(|r| r.line == line && r.range == self.row_range(line, row))?;
        let col = col_in(&self.buf, rows[idx].range.start, self.head);
        (col >= self.left && col <= self.left + self.cols).then(|| (idx, col - self.left))
    }

    /// Buffer offset at a viewport cell (for mouse hits).
    pub fn offset_at(&self, row_idx: usize, col: usize) -> usize {
        let rows = self.visible_rows();
        match rows.get(row_idx) {
            Some(r) => offset_at_col(&self.buf, r.range.clone(), col + self.left),
            None => self.buf.len(),
        }
    }

    /// Longest visible row in cells, for the horizontal scrollbar.
    pub fn visible_width(&self) -> usize {
        self.visible_rows()
            .iter()
            .map(|r| col_in(&self.buf, r.range.start, r.range.end))
            .max()
            .unwrap_or(0)
    }

    // ---- UTF-16 offsets (for Win32 message compatibility; O(n)) ----

    pub fn utf16_offset(&self, pos: usize) -> usize {
        let mut u = 0;
        let mut p = 0;
        for c in self.buf.chars_from(0) {
            if p >= pos {
                break;
            }
            p += c.len_utf8();
            u += c.len_utf16();
        }
        u
    }

    pub fn offset_of_utf16(&self, units: usize) -> usize {
        let mut u = 0;
        let mut p = 0;
        for c in self.buf.chars_from(0) {
            if u >= units {
                break;
            }
            u += c.len_utf16();
            p += c.len_utf8();
        }
        p
    }

    pub fn utf16_len(&self) -> usize {
        self.buf.chars_from(0).map(char::len_utf16).sum()
    }
}

/// Cells from `start` to `pos` on one row.
fn col_in(buf: &Buffer, start: usize, pos: usize) -> usize {
    let mut col = 0;
    let mut p = start;
    for c in buf.chars_from(start) {
        if p >= pos {
            break;
        }
        col = advance(col, c);
        p += c.len_utf8();
    }
    col
}

/// Offset in `range` closest to cell `target`.
fn offset_at_col(buf: &Buffer, range: Range<usize>, target: usize) -> usize {
    let mut col = 0;
    let mut p = range.start;
    for c in buf.chars_from(range.start) {
        if p >= range.end {
            break;
        }
        let next = advance(col, c);
        if next > target {
            // Snap to whichever edge of the char is nearer.
            return if target - col < next - target {
                p
            } else {
                p + c.len_utf8()
            };
        }
        col = next;
        p += c.len_utf8();
    }
    range.end
}

/// Row start offsets for a line wrapped at `width` cells, breaking after spaces when
/// possible.
fn wrap_line(buf: &Buffer, range: Range<usize>, width: usize) -> Vec<usize> {
    let mut rows = vec![range.start];
    let mut col = 0;
    let mut p = range.start;
    let mut row_start = range.start;
    let mut last_break: Option<(usize, usize)> = None; // (offset after space, col there)
    for c in buf.chars_from(range.start) {
        if p >= range.end {
            break;
        }
        let next = advance(col, c);
        if next > width && p > row_start {
            match last_break.filter(|&(b, _)| b > row_start) {
                Some((b, bcol)) => {
                    rows.push(b);
                    row_start = b;
                    col -= bcol;
                }
                None => {
                    rows.push(p);
                    row_start = p;
                    col = 0;
                }
            }
            last_break = None;
        }
        col = advance(col, c);
        p += c.len_utf8();
        if c == ' ' || c == '\t' {
            last_break = Some((p, col));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(s: &str) -> Editor {
        let mut e = Editor::new(Buffer::from_text(s), "\n");
        e.set_view(10, 20);
        e
    }

    fn text(e: &Editor) -> String {
        e.buffer().slice(0..e.buffer().len())
    }

    #[test]
    fn cells() {
        assert_eq!(char_cells('a'), 1);
        assert_eq!(char_cells('日'), 2);
        assert_eq!(char_cells('🎉'), 2);
        assert_eq!(char_cells('\u{301}'), 0);
        assert_eq!(advance(3, '\t'), 8);
        assert_eq!(advance(8, '\t'), 16);
    }

    #[test]
    fn typing_and_undo_coalesce() {
        let mut e = ed("");
        for c in "hello".chars() {
            e.insert(&c.to_string());
        }
        e.newline();
        e.insert("x");
        assert_eq!(text(&e), "hello\nx");
        assert!(e.undo());
        assert_eq!(text(&e), "hello\n");
        assert!(e.undo());
        assert_eq!(text(&e), "hello");
        assert!(e.undo());
        assert_eq!(text(&e), "");
        assert!(!e.undo());
        assert!(e.redo());
        assert_eq!(text(&e), "hello");
        assert_eq!(e.caret(), 5);
    }

    #[test]
    fn insert_uses_file_eol() {
        let mut e = Editor::new(Buffer::from_text("a\r\nb"), "\n");
        e.set_selection(4, 4);
        e.insert("x\ny\r\nz");
        assert_eq!(text(&e), "a\r\nbx\r\ny\r\nz");
        e.newline();
        assert!(text(&e).ends_with("z\r\n"));
    }

    #[test]
    fn crlf_is_one_step() {
        let mut e = Editor::new(Buffer::from_text("a\r\nb"), "\n");
        e.set_selection(1, 1);
        e.move_caret(Motion::Right, false);
        assert_eq!(e.caret(), 3);
        e.move_caret(Motion::Left, false);
        assert_eq!(e.caret(), 1);
        e.set_selection(3, 3);
        e.backspace(false);
        assert_eq!(text(&e), "ab");
    }

    #[test]
    fn word_motion_and_delete() {
        let mut e = ed("foo bar_baz, qux");
        e.move_caret(Motion::WordRight, false);
        assert_eq!(e.caret(), 4);
        e.move_caret(Motion::WordRight, false);
        assert_eq!(e.caret(), 13);
        e.move_caret(Motion::WordLeft, false);
        assert_eq!(e.caret(), 4);
        e.move_caret(Motion::DocEnd, false);
        e.backspace(true);
        assert_eq!(text(&e), "foo bar_baz, ");
        e.select_word_at(5);
        assert_eq!(e.selected_text(), "bar_baz");
    }

    #[test]
    fn vertical_motion_keeps_goal_column() {
        let mut e = ed("long line here\nab\nanother long one");
        e.set_selection(10, 10);
        e.move_caret(Motion::Down, false);
        assert_eq!(e.caret(), 17); // end of "ab"
        e.move_caret(Motion::Down, false);
        assert_eq!(e.caret(), 18 + 10);
        e.move_caret(Motion::Up, true);
        assert_eq!(e.selection(), 17..28);
    }

    #[test]
    fn wrap_breaks_after_spaces() {
        let b = Buffer::from_text("aaa bbb ccc dddddddddddd");
        let rows = wrap_line(&b, 0..b.len(), 8);
        let parts: Vec<String> = rows
            .iter()
            .zip(rows.iter().skip(1).chain(std::iter::once(&b.len())))
            .map(|(&s, &e)| b.slice(s..e))
            .collect();
        assert_eq!(parts, ["aaa bbb ", "ccc ", "dddddddd", "dddd"]);
    }

    #[test]
    fn wrapped_rows_and_scrolling() {
        let mut e = ed("0123456789abcdefghij\nshort\nx");
        e.set_view(2, 10);
        e.set_wrap(true);
        let rows = e.visible_rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].range, 10..20);
        e.scroll_rows(2);
        assert_eq!(e.visible_rows()[0].line, 1);
        e.set_selection(0, 0);
        assert_eq!(e.top_line(), 0);
        e.move_caret(Motion::Down, false);
        assert_eq!(e.caret(), 10);
        e.move_caret(Motion::End, false);
        assert_eq!(e.caret(), 20);
    }

    #[test]
    fn glyphs_and_hit_testing() {
        let mut e = ed("a\tb日c");
        e.set_view(5, 40);
        let row = e.visible_rows()[0].clone();
        let g = e.glyphs(&row);
        let cols: Vec<(char, usize, usize)> = g.iter().map(|g| (g.ch, g.col, g.cells)).collect();
        assert_eq!(
            cols,
            [
                ('a', 0, 1),
                ('\t', 1, 7),
                ('b', 8, 1),
                ('日', 9, 2),
                ('c', 11, 1)
            ]
        );
        assert_eq!(e.offset_at(0, 9), 3); // left half of 日 snaps before it
        assert_eq!(e.offset_at(0, 10), 6);
        e.set_selection(6, 6);
        assert_eq!(e.caret_cell(), Some((0, 11)));
    }

    #[test]
    fn horizontal_scroll_follows_caret() {
        let mut e = ed(&"x".repeat(100));
        e.set_view(5, 20);
        e.move_caret(Motion::End, false);
        assert!(e.left() > 0);
        let (_, col) = e.caret_cell().unwrap();
        assert!(col <= 20);
        e.move_caret(Motion::Home, false);
        assert_eq!(e.left(), 0);
    }

    #[test]
    fn page_moves_and_ensure_visible() {
        let s: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let mut e = ed(&s);
        e.set_view(10, 40);
        e.move_caret(Motion::PageDown, false);
        assert_eq!(e.buffer().line_of(e.caret()), 9);
        e.move_caret(Motion::PageDown, false);
        assert_eq!(e.buffer().line_of(e.caret()), 18);
        assert!(e.top_line() > 0 && e.top_line() <= 18);
        e.move_caret(Motion::DocEnd, false);
        assert_eq!(e.top_line(), 100 - 9);
    }

    #[test]
    fn find_selects_and_wraps_none() {
        let mut e = ed("one two one");
        assert!(e.find("one", true, true));
        assert_eq!(e.selection(), 0..3);
        assert!(e.find("one", true, true));
        assert_eq!(e.selection(), 8..11);
        assert!(!e.find("one", true, true));
        assert_eq!(e.selection(), 8..11);
        assert!(e.find("ONE", false, false));
        assert_eq!(e.selection(), 0..3);
    }

    #[test]
    fn caret_line_col_counts_chars() {
        let mut e = ed("ab\n\tcé✓d");
        assert_eq!(e.caret_line_col(), (1, 1));
        e.set_selection(2, 2);
        assert_eq!(e.caret_line_col(), (1, 3));
        e.move_caret(Motion::DocEnd, false);
        assert_eq!(e.caret_line_col(), (2, 6));
    }

    #[test]
    fn utf16_offsets() {
        let e = ed("a🎉b");
        assert_eq!(e.utf16_len(), 4);
        assert_eq!(e.utf16_offset(5), 3);
        assert_eq!(e.offset_of_utf16(3), 5);
    }
}
