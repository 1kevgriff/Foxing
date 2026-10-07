//! Platform-neutral UI layer: geometry, theme, draw lists, and components.
//!
//! Components lay themselves out and emit a [`DrawList`]; each platform renders the list
//! and supplies text measurement through [`Measure`]. Nothing here touches an OS API.

pub mod list;
pub mod menu;
pub mod scroll;
pub mod status;

/// 0xRRGGBB.
pub type Color = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Rect { x, y, w, h }
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }

    /// Shrinks by `d` on every side.
    pub fn inset(&self, d: i32) -> Rect {
        Rect::new(
            self.x + d,
            self.y + d,
            (self.w - 2 * d).max(0),
            (self.h - 2 * d).max(0),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Font {
    /// Monospace editing font.
    Text,
    /// Proportional font for chrome (menus, status bar).
    Ui,
}

/// Text measurement and DPI, supplied by the platform renderer.
pub trait Measure {
    fn text_width(&self, font: Font, s: &str) -> i32;
    fn line_height(&self, font: Font) -> i32;
    fn dpi(&self) -> u32;

    /// Scales a 96-DPI length to the current DPI.
    fn px(&self, v: i32) -> i32 {
        v * self.dpi() as i32 / 96
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Fill {
        rect: Rect,
        color: Color,
    },
    /// Horizontal or vertical 1-px line.
    Line {
        from: (i32, i32),
        to: (i32, i32),
        color: Color,
    },
    /// UI text, top-left at (x, y), clipped to `clip`.
    Text {
        x: i32,
        y: i32,
        font: Font,
        color: Color,
        text: String,
        clip: Rect,
    },
    /// Connected line segments (any direction), `width` px thick.
    Polyline {
        points: Vec<(i32, i32)>,
        color: Color,
        width: i32,
    },
    /// Grid text: one advance (px) per char, optional background behind the run.
    Glyphs {
        x: i32,
        y: i32,
        color: Color,
        bg: Option<(Color, i32)>,
        chars: Vec<char>,
        advances: Vec<i32>,
    },
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct DrawList {
    pub cmds: Vec<Cmd>,
}

impl DrawList {
    pub fn fill(&mut self, rect: Rect, color: Color) {
        self.cmds.push(Cmd::Fill { rect, color });
    }

    pub fn line(&mut self, from: (i32, i32), to: (i32, i32), color: Color) {
        self.cmds.push(Cmd::Line { from, to, color });
    }

    /// Moves every command by (dx, dy), e.g. to draw into a window placed elsewhere.
    pub fn offset(&mut self, dx: i32, dy: i32) {
        let mv = |r: &mut Rect| {
            r.x += dx;
            r.y += dy;
        };
        for c in &mut self.cmds {
            match c {
                Cmd::Fill { rect, .. } => mv(rect),
                Cmd::Line { from, to, .. } => {
                    *from = (from.0 + dx, from.1 + dy);
                    *to = (to.0 + dx, to.1 + dy);
                }
                Cmd::Text { x, y, clip, .. } => {
                    *x += dx;
                    *y += dy;
                    mv(clip);
                }
                Cmd::Polyline { points, .. } => {
                    for p in points {
                        *p = (p.0 + dx, p.1 + dy);
                    }
                }
                Cmd::Glyphs { x, y, .. } => {
                    *x += dx;
                    *y += dy;
                }
            }
        }
    }

    pub fn text(&mut self, x: i32, y: i32, font: Font, color: Color, text: &str, clip: Rect) {
        self.cmds.push(Cmd::Text {
            x,
            y,
            font,
            color,
            text: text.to_owned(),
            clip,
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub text_bg: Color,
    pub text_fg: Color,
    pub sel_bg: Color,
    pub sel_fg: Color,
    pub chrome_bg: Color,
    pub chrome_fg: Color,
    pub border: Color,
    pub scroll_track: Color,
    pub scroll_thumb: Color,
    pub scroll_thumb_hot: Color,
    pub hot_bg: Color,
}

impl Theme {
    pub const LIGHT: Theme = Theme {
        text_bg: 0xFFFFFF,
        text_fg: 0x000000,
        sel_bg: 0x0078D7,
        sel_fg: 0xFFFFFF,
        chrome_bg: 0xF3F3F3,
        chrome_fg: 0x1B1B1B,
        border: 0xE0E0E0,
        scroll_track: 0xF3F3F3,
        scroll_thumb: 0xC2C2C2,
        scroll_thumb_hot: 0x8A8A8A,
        hot_bg: 0xE0E0E0,
    };

    pub const DARK: Theme = Theme {
        text_bg: 0x1E1E1E,
        text_fg: 0xD4D4D4,
        sel_bg: 0x264F78,
        sel_fg: 0xFFFFFF,
        chrome_bg: 0x202020,
        chrome_fg: 0xE0E0E0,
        border: 0x3A3A3A,
        scroll_track: 0x1E1E1E,
        scroll_thumb: 0x4E4E4E,
        scroll_thumb_hot: 0x7A7A7A,
        hot_bg: 0x3A3A3A,
    };
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// Fixed-metric measurer: 7 px per char, 16 px lines, configurable DPI.
    pub struct FakeMeasure(pub u32);

    impl Measure for FakeMeasure {
        fn text_width(&self, _: Font, s: &str) -> i32 {
            s.chars().count() as i32 * 7 * self.0 as i32 / 96
        }
        fn line_height(&self, _: Font) -> i32 {
            16 * self.0 as i32 / 96
        }
        fn dpi(&self) -> u32 {
            self.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_helpers() {
        let r = Rect::new(10, 20, 30, 40);
        assert_eq!((r.right(), r.bottom()), (40, 60));
        assert!(r.contains(10, 20) && !r.contains(40, 20));
        assert_eq!(r.inset(5), Rect::new(15, 25, 20, 30));
        assert_eq!(r.inset(50).w, 0);
        assert!(r.intersects(&Rect::new(39, 59, 5, 5)));
        assert!(!r.intersects(&Rect::new(40, 20, 5, 5)));
    }

    #[test]
    fn offset_moves_everything() {
        let mut dl = DrawList::default();
        dl.fill(Rect::new(1, 2, 3, 4), 0);
        dl.line((0, 0), (5, 0), 0);
        dl.text(1, 1, Font::Ui, 0, "x", Rect::new(0, 0, 9, 9));
        dl.offset(10, 20);
        assert_eq!(
            dl.cmds[0],
            Cmd::Fill {
                rect: Rect::new(11, 22, 3, 4),
                color: 0
            }
        );
        assert!(matches!(
            dl.cmds[1],
            Cmd::Line {
                from: (10, 20),
                to: (15, 20),
                ..
            }
        ));
        assert!(matches!(dl.cmds[2], Cmd::Text { x: 11, y: 21, .. }));
    }

    #[test]
    fn px_scales_with_dpi() {
        assert_eq!(testing::FakeMeasure(96).px(10), 10);
        assert_eq!(testing::FakeMeasure(144).px(10), 15);
    }
}
