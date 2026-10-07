//! Scrollable single-column list with a header (the folder sidebar).

use super::{DrawList, Font, Measure, Rect, Theme};

const PAD_X: i32 = 12;
const ROW_PAD_Y: i32 = 4;
const HEADER_PAD_Y: i32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListMsg {
    Nothing,
    Repaint,
    /// The user picked this row.
    Activate(usize),
}

#[derive(Debug, Clone, Default)]
pub struct List {
    pub header: String,
    items: Vec<String>,
    /// Row marked as current (e.g. the open file).
    selected: Option<usize>,
    hot: Option<usize>,
    /// First visible row.
    top: usize,
}

impl List {
    pub fn new(header: &str) -> Self {
        List {
            header: header.to_owned(),
            ..Default::default()
        }
    }

    pub fn items(&self) -> &[String] {
        &self.items
    }

    pub fn set_items(&mut self, items: Vec<String>) {
        self.items = items;
        self.hot = None;
        self.selected = self.selected.filter(|&s| s < self.items.len());
        self.top = self.top.min(self.items.len().saturating_sub(1));
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub fn set_selected(&mut self, sel: Option<usize>) -> bool {
        let sel = sel.filter(|&s| s < self.items.len());
        std::mem::replace(&mut self.selected, sel) != sel
    }

    pub fn header_h(&self, m: &dyn Measure) -> i32 {
        m.line_height(Font::Ui) + 2 * m.px(HEADER_PAD_Y)
    }

    pub fn row_h(&self, m: &dyn Measure) -> i32 {
        m.line_height(Font::Ui) + 2 * m.px(ROW_PAD_Y)
    }

    fn visible_rows(&self, b: Rect, m: &dyn Measure) -> usize {
        ((b.h - self.header_h(m)).max(0) / self.row_h(m)) as usize
    }

    pub fn row_rect(&self, b: Rect, m: &dyn Measure, i: usize) -> Option<Rect> {
        let idx = i.checked_sub(self.top)?;
        if idx > self.visible_rows(b, m) || i >= self.items.len() {
            return None;
        }
        let y = b.y + self.header_h(m) + idx as i32 * self.row_h(m);
        Some(Rect::new(b.x, y, b.w, self.row_h(m)))
    }

    fn row_at(&self, b: Rect, m: &dyn Measure, x: i32, y: i32) -> Option<usize> {
        if !b.contains(x, y) || y < b.y + self.header_h(m) {
            return None;
        }
        let i = self.top + ((y - b.y - self.header_h(m)) / self.row_h(m)) as usize;
        (i < self.items.len()).then_some(i)
    }

    pub fn scroll(&mut self, b: Rect, m: &dyn Measure, rows: i64) -> ListMsg {
        let max = self.items.len().saturating_sub(self.visible_rows(b, m));
        let top = (self.top as i64 + rows).clamp(0, max as i64) as usize;
        if std::mem::replace(&mut self.top, top) != top {
            ListMsg::Repaint
        } else {
            ListMsg::Nothing
        }
    }

    /// Scrolls so `i` is visible.
    pub fn reveal(&mut self, b: Rect, m: &dyn Measure, i: usize) {
        let rows = self.visible_rows(b, m).max(1);
        if i < self.top {
            self.top = i;
        } else if i >= self.top + rows {
            self.top = i + 1 - rows;
        }
    }

    pub fn mouse_move(&mut self, b: Rect, m: &dyn Measure, x: i32, y: i32) -> ListMsg {
        let hot = self.row_at(b, m, x, y);
        if std::mem::replace(&mut self.hot, hot) != hot {
            ListMsg::Repaint
        } else {
            ListMsg::Nothing
        }
    }

    pub fn mouse_leave(&mut self) -> ListMsg {
        if self.hot.take().is_some() {
            ListMsg::Repaint
        } else {
            ListMsg::Nothing
        }
    }

    pub fn click(&mut self, b: Rect, m: &dyn Measure, x: i32, y: i32) -> ListMsg {
        match self.row_at(b, m, x, y) {
            Some(i) => {
                self.selected = Some(i);
                ListMsg::Activate(i)
            }
            None => ListMsg::Nothing,
        }
    }

    pub fn paint(&self, b: Rect, theme: &Theme, m: &dyn Measure, dl: &mut DrawList) {
        dl.fill(b, theme.chrome_bg);
        // Right edge separates the list from the text.
        dl.line(
            (b.right() - 1, b.y),
            (b.right() - 1, b.bottom()),
            theme.border,
        );
        let px = m.px(PAD_X);
        let hh = self.header_h(m);
        let header_clip = Rect::new(b.x + px, b.y, (b.w - 2 * px).max(0), hh);
        dl.text(
            b.x + px,
            b.y + m.px(HEADER_PAD_Y),
            Font::Ui,
            theme.chrome_fg,
            &self.header,
            header_clip,
        );
        dl.line(
            (b.x, b.y + hh - 1),
            (b.right() - 1, b.y + hh - 1),
            theme.border,
        );
        for i in self.top..self.items.len() {
            let Some(r) = self.row_rect(b, m, i) else {
                break;
            };
            if r.y >= b.bottom() {
                break;
            }
            let bg = if self.selected == Some(i) {
                Some(theme.sel_bg)
            } else if self.hot == Some(i) {
                Some(theme.hot_bg)
            } else {
                None
            };
            if let Some(bg) = bg {
                dl.fill(Rect::new(r.x, r.y, r.w - 1, r.h), bg);
            }
            let fg = if self.selected == Some(i) {
                theme.sel_fg
            } else {
                theme.chrome_fg
            };
            let clip = Rect::new(r.x + px, r.y, (r.w - 2 * px).max(0), r.h);
            dl.text(
                r.x + px,
                r.y + m.px(ROW_PAD_Y),
                Font::Ui,
                fg,
                &self.items[i],
                clip,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::testing::FakeMeasure;
    use crate::ui::Cmd;

    const M: FakeMeasure = FakeMeasure(96);
    // Header 32 px, rows 24 px: four rows fit in 32 + 96.
    const B: Rect = Rect::new(0, 0, 200, 128);

    fn list(n: usize) -> List {
        let mut l = List::new("My folder");
        l.set_items((0..n).map(|i| format!("file{i}.txt")).collect());
        l
    }

    #[test]
    fn clicks_map_to_rows_below_the_header() {
        let mut l = list(10);
        assert_eq!(l.click(B, &M, 10, 10), ListMsg::Nothing, "header");
        assert_eq!(l.click(B, &M, 10, 32), ListMsg::Activate(0));
        assert_eq!(l.click(B, &M, 10, 32 + 24 * 2 + 5), ListMsg::Activate(2));
        assert_eq!(l.selected(), Some(2));
        let mut short = list(1);
        assert_eq!(
            short.click(B, &M, 10, 32 + 24 + 5),
            ListMsg::Nothing,
            "past the end"
        );
    }

    #[test]
    fn scrolling_clamps_and_shifts_hits() {
        let mut l = list(10);
        assert_eq!(l.scroll(B, &M, 3), ListMsg::Repaint);
        assert_eq!(l.click(B, &M, 10, 33), ListMsg::Activate(3));
        l.scroll(B, &M, 100);
        assert_eq!(l.click(B, &M, 10, 33), ListMsg::Activate(6), "last page");
        assert_eq!(l.scroll(B, &M, 100), ListMsg::Nothing);
        l.reveal(B, &M, 0);
        assert_eq!(l.click(B, &M, 10, 33), ListMsg::Activate(0));
    }

    #[test]
    fn selection_survives_shorter_lists() {
        let mut l = list(5);
        assert!(l.set_selected(Some(4)));
        l.set_items(vec!["only.txt".into()]);
        assert_eq!(l.selected(), None);
        assert!(!l.set_selected(Some(9)));
    }

    #[test]
    fn paint_draws_header_rows_and_selection() {
        let mut l = list(3);
        l.set_selected(Some(1));
        let mut dl = DrawList::default();
        l.paint(B, &Theme::LIGHT, &M, &mut dl);
        let texts: Vec<&str> = dl
            .cmds
            .iter()
            .filter_map(|c| match c {
                Cmd::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["My folder", "file0.txt", "file1.txt", "file2.txt"]);
        assert!(dl
            .cmds
            .iter()
            .any(|c| matches!(c, Cmd::Fill { color, .. } if *color == Theme::LIGHT.sel_bg)));
    }
}
