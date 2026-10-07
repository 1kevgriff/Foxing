//! Text units for screen readers: character, word, line, and document boundaries over
//! a [`Buffer`], plus moving positions by units. Platform-neutral (UIA Text pattern on
//! Windows; the same model fits macOS and Linux accessibility APIs).

use crate::buffer::Buffer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Char,
    Word,
    Line,
    Document,
}

impl Unit {
    /// UIA `TextUnit`: Character 0, Format 1, Word 2, Line 3, Paragraph 4, Page 5,
    /// Document 6. Paragraphs are lines; format and page fall back to the document.
    pub fn from_uia(u: i32) -> Unit {
        match u {
            0 => Unit::Char,
            2 => Unit::Word,
            3 | 4 => Unit::Line,
            _ => Unit::Document,
        }
    }
}

fn char_at(b: &Buffer, pos: usize) -> Option<char> {
    b.chars_from(pos).next()
}

/// Next char boundary after `pos`; `\r\n` is one step.
pub fn next_char(b: &Buffer, pos: usize) -> usize {
    let len = b.len();
    if pos >= len {
        return len;
    }
    if b.byte_at(pos) == b'\r' && pos + 1 < len && b.byte_at(pos + 1) == b'\n' {
        return pos + 2;
    }
    pos + char_at(b, pos).map_or(1, char::len_utf8)
}

/// Previous char boundary before `pos`; `\r\n` is one step.
pub fn prev_char(b: &Buffer, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let mut p = pos - 1;
    while !b.is_char_boundary(p) {
        p -= 1;
    }
    if p > 0 && b.byte_at(p) == b'\n' && b.byte_at(p - 1) == b'\r' {
        p -= 1;
    }
    p
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Class {
    Space,
    Newline,
    Word,
    Punct,
}

fn class(c: char) -> Class {
    match c {
        '\n' | '\r' => Class::Newline,
        c if c.is_whitespace() => Class::Space,
        c if c.is_alphanumeric() || c == '_' => Class::Word,
        _ => Class::Punct,
    }
}

/// A word starts where a run of word chars, a run of punctuation, or a line break
/// begins. Trailing spaces belong to the word before them.
fn is_word_start(b: &Buffer, pos: usize) -> bool {
    if pos == 0 || pos >= b.len() {
        return true;
    }
    let Some(c) = char_at(b, pos).map(class) else {
        return true;
    };
    if c == Class::Space {
        return false;
    }
    let prev = char_at(b, prev_char(b, pos)).map(class);
    c == Class::Newline || prev != Some(c)
}

/// Start of the unit containing `pos`.
pub fn unit_start(b: &Buffer, pos: usize, unit: Unit) -> usize {
    let pos = pos.min(b.len());
    match unit {
        Unit::Char => pos,
        Unit::Word => {
            let mut p = pos;
            while !is_word_start(b, p) {
                p = prev_char(b, p);
            }
            p
        }
        Unit::Line => b.line_start(b.line_of(pos)),
        Unit::Document => 0,
    }
}

/// End (exclusive) of the unit that starts at or contains `pos`; equals the start of
/// the next unit. Lines include their line break.
pub fn unit_end(b: &Buffer, pos: usize, unit: Unit) -> usize {
    let len = b.len();
    let pos = pos.min(len);
    match unit {
        Unit::Char => next_char(b, pos),
        Unit::Word => {
            let mut p = next_char(b, pos);
            while p < len && !is_word_start(b, p) {
                p = next_char(b, p);
            }
            p
        }
        Unit::Line => {
            let line = b.line_of(pos);
            if line + 1 < b.line_count() {
                b.line_start(line + 1)
            } else {
                len
            }
        }
        Unit::Document => len,
    }
}

/// Moves `pos` by `count` unit starts (negative = backward). Returns the new position
/// and how many units it actually moved.
pub fn move_by(b: &Buffer, pos: usize, unit: Unit, count: i32) -> (usize, i32) {
    let len = b.len();
    let mut p = pos.min(len);
    let mut moved = 0;
    if count > 0 {
        while moved < count && p < len {
            p = unit_end(b, p, unit);
            moved += 1;
        }
    } else {
        while moved > count && p > 0 {
            let start = unit_start(b, p, unit);
            p = if start < p {
                start
            } else {
                unit_start(b, prev_char(b, p), unit)
            };
            moved -= 1;
        }
    }
    (p, moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(s: &str) -> Buffer {
        Buffer::from_text(s)
    }

    #[test]
    fn chars_treat_crlf_as_one() {
        let b = buf("a\r\né");
        assert_eq!(next_char(&b, 1), 3);
        assert_eq!(prev_char(&b, 3), 1);
        assert_eq!(next_char(&b, 3), 5, "é is two bytes");
        assert_eq!(move_by(&b, 0, Unit::Char, 10), (5, 3));
        assert_eq!(move_by(&b, 5, Unit::Char, -10), (0, -3));
    }

    #[test]
    fn words_include_trailing_spaces_and_split_punctuation() {
        let b = buf("Hello,  world!\nnext");
        let starts: Vec<usize> = (0..=b.len()).filter(|&p| is_word_start(&b, p)).collect();
        // Hello | ,   | world | ! | \n | next | end
        assert_eq!(starts, [0, 5, 8, 13, 14, 15, 19]);
        assert_eq!(unit_start(&b, 10, Unit::Word), 8);
        assert_eq!(unit_end(&b, 8, Unit::Word), 13);
        assert_eq!(
            unit_end(&b, 5, Unit::Word),
            8,
            "punctuation word includes its spaces"
        );
        assert_eq!(move_by(&b, 0, Unit::Word, 2), (8, 2));
        assert_eq!(
            move_by(&b, 10, Unit::Word, -1),
            (8, -1),
            "first step goes to own start"
        );
        assert_eq!(move_by(&b, 8, Unit::Word, -1), (5, -1));
    }

    #[test]
    fn lines_include_their_break() {
        let b = buf("one\r\ntwo\nthree");
        assert_eq!(unit_start(&b, 6, Unit::Line), 5);
        assert_eq!(unit_end(&b, 0, Unit::Line), 5);
        assert_eq!(unit_end(&b, 10, Unit::Line), b.len());
        assert_eq!(move_by(&b, 0, Unit::Line, 5), (b.len(), 3));
        assert_eq!(move_by(&b, 11, Unit::Line, -1), (9, -1));
        assert_eq!(move_by(&b, 9, Unit::Line, -2), (0, -2));
    }

    #[test]
    fn document_and_uia_mapping() {
        let b = buf("abc");
        assert_eq!(
            (
                unit_start(&b, 2, Unit::Document),
                unit_end(&b, 2, Unit::Document)
            ),
            (0, 3)
        );
        assert_eq!(move_by(&b, 0, Unit::Document, 1), (3, 1));
        assert_eq!(
            move_by(&b, 3, Unit::Document, 1),
            (3, 0),
            "nothing past the end"
        );
        assert_eq!(Unit::from_uia(0), Unit::Char);
        assert_eq!(Unit::from_uia(4), Unit::Line);
        assert_eq!(Unit::from_uia(5), Unit::Document);
    }

    #[test]
    fn empty_buffer_is_safe() {
        let b = buf("");
        assert_eq!(unit_end(&b, 0, Unit::Word), 0);
        assert_eq!(move_by(&b, 0, Unit::Line, 3), (0, 0));
        assert_eq!(move_by(&b, 0, Unit::Char, -3), (0, 0));
    }
}
