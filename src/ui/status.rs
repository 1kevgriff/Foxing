//! Status bar: a flexible first part, then fixed-width parts aligned to the right.

use super::{DrawList, Font, Measure, Rect, Theme};

/// Horizontal text padding inside a part (96-DPI px).
const PAD: i32 = 8;
/// Vertical padding above and below the text (96-DPI px).
const VPAD: i32 = 4;

pub struct StatusBar {
    texts: Vec<String>,
    /// 96-DPI widths of parts 1.. (part 0 takes the remaining space).
    widths: Vec<i32>,
}

impl StatusBar {
    /// `widths` are the fixed parts after the flexible first one.
    pub fn new(widths: &[i32]) -> Self {
        StatusBar {
            texts: vec![String::new(); widths.len() + 1],
            widths: widths.to_vec(),
        }
    }

    pub fn parts(&self) -> usize {
        self.texts.len()
    }

    pub fn text(&self, part: usize) -> &str {
        &self.texts[part]
    }

    /// Returns true if the text changed (the caller repaints only then).
    pub fn set_text(&mut self, part: usize, text: &str) -> bool {
        if self.texts[part] == text {
            return false;
        }
        text.clone_into(&mut self.texts[part]);
        true
    }

    pub fn height(&self, m: &dyn Measure) -> i32 {
        m.line_height(Font::Ui) + 2 * m.px(VPAD) + 1
    }

    /// Bounds of each part within `bounds`.
    pub fn part_rects(&self, bounds: Rect, m: &dyn Measure) -> Vec<Rect> {
        let mut rects = vec![Rect::default(); self.parts()];
        let mut right = bounds.right();
        for (i, w) in self.widths.iter().enumerate().rev() {
            let w = m.px(*w);
            rects[i + 1] = Rect::new(right - w, bounds.y, w, bounds.h);
            right -= w;
        }
        rects[0] = Rect::new(bounds.x, bounds.y, (right - bounds.x).max(0), bounds.h);
        rects
    }

    pub fn paint(&self, bounds: Rect, theme: &Theme, m: &dyn Measure, dl: &mut DrawList) {
        dl.fill(bounds, theme.chrome_bg);
        dl.line(
            (bounds.x, bounds.y),
            (bounds.right(), bounds.y),
            theme.border,
        );
        let text_y = bounds.y + 1 + m.px(VPAD);
        for (i, r) in self.part_rects(bounds, m).into_iter().enumerate() {
            if i > 0 {
                let inset = m.px(VPAD);
                dl.line(
                    (r.x, r.y + 1 + inset),
                    (r.x, r.bottom() - inset),
                    theme.border,
                );
            }
            if !self.texts[i].is_empty() {
                let clip = Rect::new(r.x + m.px(PAD), r.y, (r.w - 2 * m.px(PAD)).max(0), r.h);
                dl.text(
                    clip.x,
                    text_y,
                    Font::Ui,
                    theme.chrome_fg,
                    &self.texts[i],
                    clip,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::testing::FakeMeasure;
    use crate::ui::Cmd;

    #[test]
    fn parts_align_right_and_scale() {
        let bar = StatusBar::new(&[100, 50]);
        let r = bar.part_rects(Rect::new(0, 0, 400, 20), &FakeMeasure(96));
        assert_eq!(
            r,
            [
                Rect::new(0, 0, 250, 20),
                Rect::new(250, 0, 100, 20),
                Rect::new(350, 0, 50, 20)
            ]
        );
        let r = bar.part_rects(Rect::new(0, 0, 400, 30), &FakeMeasure(192));
        assert_eq!(r[1], Rect::new(100, 0, 200, 30));
        assert_eq!(r[0].w, 100);
        // Too narrow: the flexible part collapses rather than going negative.
        let r = bar.part_rects(Rect::new(0, 0, 100, 20), &FakeMeasure(96));
        assert_eq!(r[0].w, 0);
    }

    #[test]
    fn set_text_reports_changes() {
        let mut bar = StatusBar::new(&[100]);
        assert!(bar.set_text(1, "Ln 1"));
        assert!(!bar.set_text(1, "Ln 1"));
        assert_eq!(bar.text(1), "Ln 1");
    }

    #[test]
    fn paint_emits_clipped_text_and_separators() {
        let mut bar = StatusBar::new(&[100, 50]);
        bar.set_text(1, "Ln 1, Col 1");
        bar.set_text(2, "UTF-8");
        let m = FakeMeasure(96);
        let mut dl = DrawList::default();
        bar.paint(
            Rect::new(0, 100, 400, bar.height(&m)),
            &Theme::LIGHT,
            &m,
            &mut dl,
        );
        let texts: Vec<(&str, i32, Rect)> = dl
            .cmds
            .iter()
            .filter_map(|c| match c {
                Cmd::Text { text, x, clip, .. } => Some((text.as_str(), *x, *clip)),
                _ => None,
            })
            .collect();
        assert_eq!(texts.len(), 2);
        assert_eq!(texts[0].0, "Ln 1, Col 1");
        assert_eq!(texts[0].1, 258);
        assert_eq!(texts[0].2.w, 84);
        let separators = dl
            .cmds
            .iter()
            .filter(|c| matches!(c, Cmd::Line { from, to, .. } if from.0 == to.0))
            .count();
        assert_eq!(separators, 2);
        assert!(matches!(
            dl.cmds[0],
            Cmd::Fill {
                color: 0xF3F3F3,
                ..
            }
        ));
    }
}
