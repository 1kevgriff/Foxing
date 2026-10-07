//! Scrollbar: track + thumb, thumb dragging, and page-by-page track clicks.
//! Positions are `u64`, so huge documents need no scaling.

use super::{DrawList, Measure, Rect, Theme};

/// Thickness of a scrollbar (96-DPI px).
pub const THICKNESS: i32 = 14;
/// Smallest thumb length (96-DPI px), so it stays grabbable on huge documents.
const MIN_THUMB: i32 = 24;
/// Thumb inset from the track edges (96-DPI px).
const INSET: i32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollAction {
    PageBack,
    PageForward,
    /// Thumb grabbed; follow-up moves report positions.
    Grab,
}

#[derive(Debug, Clone)]
pub struct ScrollBar {
    vertical: bool,
    total: u64,
    page: u64,
    pos: u64,
    hot: bool,
    /// Offset of the grab point from the thumb start while dragging.
    grab: Option<i32>,
}

impl ScrollBar {
    pub fn new(vertical: bool) -> Self {
        ScrollBar {
            vertical,
            total: 0,
            page: 1,
            pos: 0,
            hot: false,
            grab: None,
        }
    }

    pub fn thickness(m: &dyn Measure) -> i32 {
        m.px(THICKNESS)
    }

    /// `total` units, `page` visible at once, first visible at `pos`.
    pub fn set(&mut self, total: u64, page: u64, pos: u64) {
        self.total = total;
        self.page = page.max(1);
        self.pos = pos.min(self.max_pos());
    }

    pub fn pos(&self) -> u64 {
        self.pos
    }

    /// (total, page, pos): what the bar looks like, for change detection.
    pub fn state(&self) -> (u64, u64, u64) {
        (self.total, self.page, self.pos)
    }

    pub fn visible(&self) -> bool {
        self.total > self.page
    }

    pub fn dragging(&self) -> bool {
        self.grab.is_some()
    }

    fn max_pos(&self) -> u64 {
        self.total.saturating_sub(self.page)
    }

    /// (start, length) of the track along the scroll axis.
    fn track(&self, b: Rect) -> (i32, i32) {
        if self.vertical {
            (b.y, b.h)
        } else {
            (b.x, b.w)
        }
    }

    fn axis(&self, x: i32, y: i32) -> i32 {
        if self.vertical {
            y
        } else {
            x
        }
    }

    fn thumb_span(&self, b: Rect, m: &dyn Measure) -> (i32, i32) {
        let (start, len) = self.track(b);
        if self.total == 0 {
            return (start, len);
        }
        let thumb = ((len as u128 * self.page as u128 / self.total as u128) as i32)
            .clamp(m.px(MIN_THUMB).min(len), len);
        let max = self.max_pos();
        let off = if max == 0 {
            0
        } else {
            ((len - thumb) as u128 * self.pos as u128 / max as u128) as i32
        };
        (start + off, thumb)
    }

    pub fn thumb(&self, b: Rect, m: &dyn Measure) -> Rect {
        let (s, l) = self.thumb_span(b, m);
        let i = m.px(INSET);
        if self.vertical {
            Rect::new(b.x + i, s, (b.w - 2 * i).max(1), l)
        } else {
            Rect::new(s, b.y + i, l, (b.h - 2 * i).max(1))
        }
    }

    pub fn paint(&self, b: Rect, theme: &Theme, m: &dyn Measure, dl: &mut DrawList) {
        dl.fill(b, theme.scroll_track);
        if self.visible() {
            let color = if self.hot || self.dragging() {
                theme.scroll_thumb_hot
            } else {
                theme.scroll_thumb
            };
            dl.fill(self.thumb(b, m), color);
        }
    }

    pub fn mouse_down(&mut self, b: Rect, m: &dyn Measure, x: i32, y: i32) -> Option<ScrollAction> {
        if !self.visible() || !b.contains(x, y) {
            return None;
        }
        let (s, l) = self.thumb_span(b, m);
        let a = self.axis(x, y);
        Some(if a < s {
            ScrollAction::PageBack
        } else if a >= s + l {
            ScrollAction::PageForward
        } else {
            self.grab = Some(a - s);
            ScrollAction::Grab
        })
    }

    /// While dragging, returns the new position for the pointer.
    pub fn mouse_move(&mut self, b: Rect, m: &dyn Measure, x: i32, y: i32) -> Option<u64> {
        let grab = self.grab?;
        let (start, len) = self.track(b);
        let (_, thumb) = self.thumb_span(b, m);
        let room = (len - thumb).max(1) as i64;
        let off = (self.axis(x, y) - grab - start).clamp(0, room as i32) as i64;
        let pos = (off as u128 * self.max_pos() as u128 / room as u128) as u64;
        self.pos = pos.min(self.max_pos());
        Some(self.pos)
    }

    /// Ends a drag; returns true if one was active.
    pub fn mouse_up(&mut self) -> bool {
        self.grab.take().is_some()
    }

    /// Returns true if the hover state changed.
    pub fn set_hot(&mut self, hot: bool) -> bool {
        std::mem::replace(&mut self.hot, hot) != hot
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::testing::FakeMeasure;
    use crate::ui::Cmd;

    const M: FakeMeasure = FakeMeasure(96);
    const B: Rect = Rect::new(0, 0, 14, 200);

    #[test]
    fn hidden_when_everything_fits() {
        let mut s = ScrollBar::new(true);
        s.set(10, 20, 0);
        assert!(!s.visible());
        assert_eq!(s.mouse_down(B, &M, 5, 50), None);
    }

    #[test]
    fn thumb_size_and_position() {
        let mut s = ScrollBar::new(true);
        s.set(100, 25, 0);
        assert_eq!(s.thumb(B, &M), Rect::new(3, 0, 8, 50));
        s.set(100, 25, 75);
        assert_eq!(s.thumb(B, &M).bottom(), 200);
        s.set(100, 25, 1000);
        assert_eq!(s.pos(), 75, "clamped to max");
        // Huge document: thumb keeps its minimum size.
        s.set(33_000_000_000, 40, 0);
        assert_eq!(s.thumb(B, &M).h, 24);
    }

    #[test]
    fn track_clicks_page_and_thumb_drags() {
        let mut s = ScrollBar::new(true);
        s.set(100, 25, 50);
        let t = s.thumb(B, &M);
        assert_eq!(
            s.mouse_down(B, &M, 5, t.y - 1),
            Some(ScrollAction::PageBack)
        );
        assert_eq!(
            s.mouse_down(B, &M, 5, t.bottom() + 1),
            Some(ScrollAction::PageForward)
        );
        assert_eq!(s.mouse_down(B, &M, 5, t.y + 10), Some(ScrollAction::Grab));
        assert!(s.dragging());
        // Drag to the very top, then past the bottom.
        assert_eq!(s.mouse_move(B, &M, 5, 10 - 1000), Some(0));
        assert_eq!(s.mouse_move(B, &M, 5, 10_000), Some(75));
        assert!(s.mouse_up());
        assert_eq!(s.mouse_move(B, &M, 5, 50), None);
    }

    #[test]
    fn horizontal_uses_x() {
        let b = Rect::new(0, 0, 200, 14);
        let mut s = ScrollBar::new(false);
        s.set(400, 100, 0);
        assert_eq!(s.thumb(b, &M), Rect::new(0, 3, 50, 8));
        assert_eq!(s.mouse_down(b, &M, 150, 5), Some(ScrollAction::PageForward));
    }

    #[test]
    fn paint_uses_hot_color() {
        let mut s = ScrollBar::new(true);
        s.set(100, 25, 0);
        let mut dl = DrawList::default();
        s.paint(B, &Theme::LIGHT, &M, &mut dl);
        assert!(
            matches!(dl.cmds[1], Cmd::Fill { color, .. } if color == Theme::LIGHT.scroll_thumb)
        );
        assert!(s.set_hot(true));
        assert!(!s.set_hot(true));
        let mut dl = DrawList::default();
        s.paint(B, &Theme::LIGHT, &M, &mut dl);
        assert!(
            matches!(dl.cmds[1], Cmd::Fill { color, .. } if color == Theme::LIGHT.scroll_thumb_hot)
        );
    }
}
