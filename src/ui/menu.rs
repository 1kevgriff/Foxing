//! Menu bar with drop-down menus. Mouse and keyboard (Alt tap / F10, mnemonics,
//! arrows, Enter, Esc) both drive a small state machine; hosts forward input and act
//! on the returned [`MenuMsg`].

use super::{Cmd, DrawList, Font, Measure, Rect, Theme};

// Layout constants (96-DPI px).
const TITLE_PAD_X: i32 = 10;
const BAR_PAD_Y: i32 = 4;
const ITEM_PAD_Y: i32 = 5;
const SEP_H: i32 = 9;
const CHECK_W: i32 = 28;
const ACCEL_GAP: i32 = 40;
const RIGHT_PAD: i32 = 16;
const MIN_W: i32 = 180;
const DROP_PAD_Y: i32 = 4;
const HOT_INSET: i32 = 4;

#[derive(Debug, Clone, PartialEq)]
pub struct MenuItem {
    /// Label with `&` before the mnemonic; empty for a separator.
    pub label: String,
    /// Shortcut hint shown on the right, e.g. "Ctrl+S".
    pub accel: String,
    pub id: u16,
    pub checked: bool,
}

impl MenuItem {
    pub fn new(label: &str, accel: &str, id: u16) -> Self {
        MenuItem {
            label: label.to_owned(),
            accel: accel.to_owned(),
            id,
            checked: false,
        }
    }

    pub fn separator() -> Self {
        MenuItem::new("", "", 0)
    }

    pub fn is_separator(&self) -> bool {
        self.label.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Menu {
    pub title: String,
    pub items: Vec<MenuItem>,
}

impl Menu {
    pub fn new(title: &str, items: Vec<MenuItem>) -> Self {
        Menu {
            title: title.to_owned(),
            items,
        }
    }
}

/// Display text and the char index of the mnemonic. `&&` is a literal `&`.
pub fn parse_label(label: &str) -> (String, Option<usize>) {
    let mut out = String::with_capacity(label.len());
    let mut mnemonic = None;
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            match chars.next() {
                Some('&') => out.push('&'),
                Some(m) => {
                    mnemonic.get_or_insert(out.chars().count());
                    out.push(m);
                }
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    (out, mnemonic)
}

fn mnemonic_char(label: &str) -> Option<char> {
    let (text, i) = parse_label(label);
    text.chars().nth(i?).map(|c| c.to_ascii_lowercase())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKey {
    Left,
    Right,
    Up,
    Down,
    Enter,
    Escape,
    Char(char),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuMsg {
    Nothing,
    Repaint,
    /// An item was chosen; the menu has closed.
    Command(u16),
    /// The menu closed without a choice.
    Closed,
}

#[derive(Debug, Clone, Default)]
pub struct MenuBar {
    pub menus: Vec<Menu>,
    /// Menu mode: the bar owns keyboard and mouse input.
    active: bool,
    /// Menu whose drop-down is showing.
    open: Option<usize>,
    /// Highlighted title in menu mode.
    hot: Option<usize>,
    /// Highlighted item in the open drop-down.
    item: Option<usize>,
    /// Show mnemonic underlines (keyboard activation).
    underline: bool,
    /// Title under the mouse while not in menu mode.
    hover: Option<usize>,
}

impl MenuBar {
    pub fn new(menus: Vec<Menu>) -> Self {
        MenuBar {
            menus,
            ..Default::default()
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn open_menu(&self) -> Option<usize> {
        self.open
    }

    pub fn set_checked(&mut self, id: u16, on: bool) -> bool {
        let mut changed = false;
        for it in self.menus.iter_mut().flat_map(|m| m.items.iter_mut()) {
            if it.id == id && it.checked != on {
                it.checked = on;
                changed = true;
            }
        }
        changed
    }

    pub fn is_checked(&self, id: u16) -> bool {
        self.menus
            .iter()
            .flat_map(|m| m.items.iter())
            .any(|it| it.id == id && it.checked)
    }

    // ---- layout ----

    pub fn height(&self, m: &dyn Measure) -> i32 {
        m.line_height(Font::Ui) + 2 * m.px(BAR_PAD_Y)
    }

    pub fn title_rects(&self, bounds: Rect, m: &dyn Measure) -> Vec<Rect> {
        let mut x = bounds.x;
        self.menus
            .iter()
            .map(|menu| {
                let w = m.text_width(Font::Ui, &parse_label(&menu.title).0) + 2 * m.px(TITLE_PAD_X);
                let r = Rect::new(x, bounds.y, w, bounds.h);
                x += w;
                r
            })
            .collect()
    }

    fn item_h(&self, it: &MenuItem, m: &dyn Measure) -> i32 {
        if it.is_separator() {
            m.px(SEP_H)
        } else {
            m.line_height(Font::Ui) + 2 * m.px(ITEM_PAD_Y)
        }
    }

    /// Drop-down rect and item rects (same coordinate space as `bounds`), if open.
    pub fn dropdown(&self, bounds: Rect, m: &dyn Measure) -> Option<(Rect, Vec<Rect>)> {
        let i = self.open?;
        let menu = &self.menus[i];
        let title = self.title_rects(bounds, m)[i];
        let label_w = menu
            .items
            .iter()
            .map(|it| m.text_width(Font::Ui, &parse_label(&it.label).0))
            .max()
            .unwrap_or(0);
        let accel_w = menu
            .items
            .iter()
            .map(|it| m.text_width(Font::Ui, &it.accel))
            .max()
            .unwrap_or(0);
        let w = (m.px(CHECK_W) + label_w + m.px(ACCEL_GAP) + accel_w + m.px(RIGHT_PAD))
            .max(m.px(MIN_W));
        let mut y = bounds.bottom() + 1 + m.px(DROP_PAD_Y);
        let items: Vec<Rect> = menu
            .items
            .iter()
            .map(|it| {
                let h = self.item_h(it, m);
                let r = Rect::new(title.x + 1, y, w - 2, h);
                y += h;
                r
            })
            .collect();
        let rect = Rect::new(
            title.x,
            bounds.bottom(),
            w,
            y + m.px(DROP_PAD_Y) + 1 - bounds.bottom(),
        );
        Some((rect, items))
    }

    fn title_at(&self, bounds: Rect, m: &dyn Measure, x: i32, y: i32) -> Option<usize> {
        self.title_rects(bounds, m)
            .iter()
            .position(|r| r.contains(x, y))
    }

    fn item_at(&self, bounds: Rect, m: &dyn Measure, x: i32, y: i32) -> Option<usize> {
        let (_, items) = self.dropdown(bounds, m)?;
        let i = items.iter().position(|r| r.contains(x, y))?;
        (!self.menus[self.open?].items[i].is_separator()).then_some(i)
    }

    fn selectable(&self, menu: usize) -> impl Iterator<Item = usize> + '_ {
        self.menus[menu]
            .items
            .iter()
            .enumerate()
            .filter(|(_, it)| !it.is_separator())
            .map(|(i, _)| i)
    }

    /// Next selectable item after (or before) `from`, wrapping around.
    fn step_item(&self, menu: usize, from: Option<usize>, forward: bool) -> Option<usize> {
        let all: Vec<usize> = self.selectable(menu).collect();
        if all.is_empty() {
            return None;
        }
        let pos = from.and_then(|f| all.iter().position(|&i| i == f));
        Some(match (pos, forward) {
            (None, true) => all[0],
            (None, false) => all[all.len() - 1],
            (Some(p), true) => all[(p + 1) % all.len()],
            (Some(p), false) => all[(p + all.len() - 1) % all.len()],
        })
    }

    // ---- state changes ----

    pub fn close(&mut self) -> MenuMsg {
        let was = self.active;
        self.active = false;
        self.open = None;
        self.hot = None;
        self.item = None;
        self.underline = false;
        if was {
            MenuMsg::Closed
        } else {
            MenuMsg::Nothing
        }
    }

    fn choose(&mut self, menu: usize, item: usize) -> MenuMsg {
        let id = self.menus[menu].items[item].id;
        self.close();
        MenuMsg::Command(id)
    }

    fn open_at(&mut self, menu: usize, first_item: bool) {
        self.active = true;
        self.hot = Some(menu);
        self.open = Some(menu);
        self.item = if first_item {
            self.step_item(menu, None, true)
        } else {
            None
        };
    }

    /// Alt tap or F10: enter or leave menu mode with the first title highlighted.
    pub fn toggle_keyboard(&mut self) -> MenuMsg {
        if self.active {
            return self.close();
        }
        if self.menus.is_empty() {
            return MenuMsg::Nothing;
        }
        self.active = true;
        self.hot = Some(0);
        self.open = None;
        self.item = None;
        self.underline = true;
        MenuMsg::Repaint
    }

    /// Alt+letter: opens the menu with that mnemonic.
    pub fn mnemonic(&mut self, c: char) -> MenuMsg {
        let c = c.to_ascii_lowercase();
        match self
            .menus
            .iter()
            .position(|m| mnemonic_char(&m.title) == Some(c))
        {
            Some(i) => {
                self.open_at(i, true);
                self.underline = true;
                MenuMsg::Repaint
            }
            None => MenuMsg::Nothing,
        }
    }

    pub fn key(&mut self, k: MenuKey) -> MenuMsg {
        if !self.active {
            return MenuMsg::Nothing;
        }
        let n = self.menus.len();
        let hot = self.hot.unwrap_or(0);
        match k {
            MenuKey::Left | MenuKey::Right => {
                let next = if k == MenuKey::Right {
                    (hot + 1) % n
                } else {
                    (hot + n - 1) % n
                };
                if self.open.is_some() {
                    self.open_at(next, true);
                } else {
                    self.hot = Some(next);
                }
            }
            MenuKey::Down | MenuKey::Up => match self.open {
                None => self.open_at(hot, true),
                Some(menu) => self.item = self.step_item(menu, self.item, k == MenuKey::Down),
            },
            MenuKey::Enter => match (self.open, self.item) {
                (Some(menu), Some(item)) => return self.choose(menu, item),
                (None, _) => self.open_at(hot, true),
                _ => {}
            },
            MenuKey::Escape => {
                if self.open.is_some() {
                    self.open = None;
                    self.item = None;
                } else {
                    return self.close();
                }
            }
            MenuKey::Char(c) => {
                let c = c.to_ascii_lowercase();
                if let Some(menu) = self.open {
                    let hit = self.menus[menu]
                        .items
                        .iter()
                        .position(|it| mnemonic_char(&it.label) == Some(c));
                    if let Some(item) = hit {
                        return self.choose(menu, item);
                    }
                }
                return match self
                    .menus
                    .iter()
                    .position(|m| mnemonic_char(&m.title) == Some(c))
                {
                    Some(i) => {
                        self.open_at(i, true);
                        MenuMsg::Repaint
                    }
                    None => MenuMsg::Nothing,
                };
            }
        }
        MenuMsg::Repaint
    }

    pub fn mouse_move(&mut self, bounds: Rect, m: &dyn Measure, x: i32, y: i32) -> MenuMsg {
        let title = self.title_at(bounds, m, x, y);
        if !self.active {
            return if std::mem::replace(&mut self.hover, title) != title {
                MenuMsg::Repaint
            } else {
                MenuMsg::Nothing
            };
        }
        let before = (self.open, self.hot, self.item);
        if let (Some(t), Some(_)) = (title, self.open) {
            if Some(t) != self.open {
                self.open_at(t, false);
            }
        }
        if self.open.is_some() {
            self.item = self.item_at(bounds, m, x, y);
        }
        if (self.open, self.hot, self.item) != before {
            MenuMsg::Repaint
        } else {
            MenuMsg::Nothing
        }
    }

    pub fn mouse_leave(&mut self) -> MenuMsg {
        if self.hover.take().is_some() {
            MenuMsg::Repaint
        } else {
            MenuMsg::Nothing
        }
    }

    pub fn mouse_down(&mut self, bounds: Rect, m: &dyn Measure, x: i32, y: i32) -> MenuMsg {
        if let Some(t) = self.title_at(bounds, m, x, y) {
            if self.active && self.open == Some(t) {
                return self.close();
            }
            self.open_at(t, false);
            self.underline = false;
            self.hover = None;
            return MenuMsg::Repaint;
        }
        if !self.active {
            return MenuMsg::Nothing;
        }
        let inside = self
            .dropdown(bounds, m)
            .is_some_and(|(r, _)| r.contains(x, y));
        if inside {
            MenuMsg::Nothing
        } else {
            self.close()
        }
    }

    pub fn mouse_up(&mut self, bounds: Rect, m: &dyn Measure, x: i32, y: i32) -> MenuMsg {
        if !self.active {
            return MenuMsg::Nothing;
        }
        match (self.open, self.item_at(bounds, m, x, y)) {
            (Some(menu), Some(item)) => self.choose(menu, item),
            _ => MenuMsg::Nothing,
        }
    }

    // ---- painting ----

    fn label(
        dl: &mut DrawList,
        m: &dyn Measure,
        (x, y): (i32, i32),
        color: u32,
        label: &str,
        underline: bool,
        clip: Rect,
    ) {
        let (text, mn) = parse_label(label);
        dl.text(x, y, Font::Ui, color, &text, clip);
        if let (true, Some(i)) = (underline, mn) {
            let prefix: String = text.chars().take(i).collect();
            let ch: String = text.chars().skip(i).take(1).collect();
            let ux = x + m.text_width(Font::Ui, &prefix);
            let uy = y + m.line_height(Font::Ui) - 2;
            dl.line((ux, uy), (ux + m.text_width(Font::Ui, &ch) - 1, uy), color);
        }
    }

    pub fn paint_bar(&self, bounds: Rect, theme: &Theme, m: &dyn Measure, dl: &mut DrawList) {
        dl.fill(bounds, theme.chrome_bg);
        let ty = bounds.y + m.px(BAR_PAD_Y);
        for (i, r) in self.title_rects(bounds, m).into_iter().enumerate() {
            let lit = self.open == Some(i)
                || (self.active && self.hot == Some(i))
                || (!self.active && self.hover == Some(i));
            if lit {
                dl.fill(r.inset(m.px(2)), theme.hot_bg);
            }
            let x = r.x + m.px(TITLE_PAD_X);
            Self::label(
                dl,
                m,
                (x, ty),
                theme.chrome_fg,
                &self.menus[i].title,
                self.underline,
                r,
            );
        }
    }

    /// Paints the open drop-down (same coordinate space as `bounds`).
    pub fn paint_dropdown(&self, bounds: Rect, theme: &Theme, m: &dyn Measure, dl: &mut DrawList) {
        let Some((rect, items)) = self.dropdown(bounds, m) else {
            return;
        };
        let menu = &self.menus[self.open.expect("dropdown implies open")];
        dl.fill(rect, theme.chrome_bg);
        let (l, t, r, b) = (rect.x, rect.y, rect.right() - 1, rect.bottom() - 1);
        dl.line((l, t), (r, t), theme.border);
        dl.line((l, b), (r, b), theme.border);
        dl.line((l, t), (l, b), theme.border);
        dl.line((r, t), (r, b), theme.border);
        for (i, (it, ir)) in menu.items.iter().zip(items).enumerate() {
            if it.is_separator() {
                let y = ir.y + ir.h / 2;
                dl.line(
                    (ir.x + m.px(HOT_INSET), y),
                    (ir.right() - m.px(HOT_INSET), y),
                    theme.border,
                );
                continue;
            }
            if self.item == Some(i) {
                let hot = Rect::new(
                    ir.x + m.px(HOT_INSET),
                    ir.y,
                    ir.w - 2 * m.px(HOT_INSET),
                    ir.h,
                );
                dl.fill(hot, theme.hot_bg);
            }
            let ty = ir.y + m.px(ITEM_PAD_Y);
            if it.checked {
                // A check mark drawn as two strokes.
                let cx = ir.x + m.px(CHECK_W) / 2;
                let cy = ir.y + ir.h / 2;
                let s = m.px(4);
                dl.cmds.push(Cmd::Polyline {
                    points: vec![
                        (cx - s, cy),
                        (cx - s / 3, cy + s * 2 / 3),
                        (cx + s + s / 3, cy - s),
                    ],
                    color: theme.chrome_fg,
                    width: m.px(2).max(1),
                });
            }
            Self::label(
                dl,
                m,
                (ir.x + m.px(CHECK_W), ty),
                theme.chrome_fg,
                &it.label,
                self.underline,
                ir,
            );
            if !it.accel.is_empty() {
                let aw = m.text_width(Font::Ui, &it.accel);
                dl.text(
                    ir.right() - m.px(RIGHT_PAD) - aw,
                    ty,
                    Font::Ui,
                    theme.chrome_fg,
                    &it.accel,
                    ir,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::testing::FakeMeasure;

    const M: FakeMeasure = FakeMeasure(96);

    fn bar() -> MenuBar {
        MenuBar::new(vec![
            Menu::new(
                "&File",
                vec![
                    MenuItem::new("&New", "Ctrl+N", 1),
                    MenuItem::new("&Open...", "Ctrl+O", 2),
                    MenuItem::separator(),
                    MenuItem::new("E&xit", "", 3),
                ],
            ),
            Menu::new("&Edit", vec![MenuItem::new("&Undo", "Ctrl+Z", 10)]),
            Menu::new("&View", vec![MenuItem::new("&Status Bar", "", 20)]),
        ])
    }

    fn bounds() -> Rect {
        Rect::new(0, 0, 800, bar().height(&M))
    }

    #[test]
    fn labels_and_mnemonics() {
        assert_eq!(parse_label("&File"), ("File".into(), Some(0)));
        assert_eq!(parse_label("E&xit"), ("Exit".into(), Some(1)));
        assert_eq!(parse_label("Save && Quit"), ("Save & Quit".into(), None));
        assert_eq!(mnemonic_char("Select &All"), Some('a'));
    }

    #[test]
    fn layout_titles_and_dropdown() {
        let mut b = bar();
        let t = b.title_rects(bounds(), &M);
        assert_eq!(t[0], Rect::new(0, 0, 4 * 7 + 20, 24));
        assert_eq!(t[1].x, t[0].right());
        assert!(b.dropdown(bounds(), &M).is_none());
        b.mouse_down(bounds(), &M, 5, 5);
        let (r, items) = b.dropdown(bounds(), &M).unwrap();
        assert_eq!(r.y, 24);
        assert_eq!(items.len(), 4);
        assert!(r.w >= 180);
        assert_eq!(items[2].h, 9, "separator is short");
        assert!(items[3].y > items[2].y);
    }

    #[test]
    fn mouse_click_open_hover_switch_and_choose() {
        let mut b = bar();
        let t = b.title_rects(bounds(), &M);
        assert_eq!(b.mouse_down(bounds(), &M, t[0].x + 2, 5), MenuMsg::Repaint);
        assert_eq!(b.open_menu(), Some(0));
        // Hovering another title while open switches menus.
        b.mouse_move(bounds(), &M, t[2].x + 2, 5);
        assert_eq!(b.open_menu(), Some(2));
        // Release over an item runs it and closes.
        let (_, items) = b.dropdown(bounds(), &M).unwrap();
        let (x, y) = (items[0].x + 5, items[0].y + 2);
        b.mouse_move(bounds(), &M, x, y);
        assert_eq!(b.mouse_up(bounds(), &M, x, y), MenuMsg::Command(20));
        assert!(!b.is_active());
    }

    #[test]
    fn click_outside_or_title_again_closes() {
        let mut b = bar();
        b.mouse_down(bounds(), &M, 5, 5);
        assert_eq!(b.mouse_down(bounds(), &M, 5, 5), MenuMsg::Closed);
        b.mouse_down(bounds(), &M, 5, 5);
        assert_eq!(b.mouse_down(bounds(), &M, 700, 500), MenuMsg::Closed);
        // Separators and empty space inside the drop-down do nothing.
        b.mouse_down(bounds(), &M, 5, 5);
        let (_, items) = b.dropdown(bounds(), &M).unwrap();
        assert_eq!(
            b.mouse_up(bounds(), &M, items[2].x + 5, items[2].y + 2),
            MenuMsg::Nothing
        );
        assert!(b.is_active());
    }

    #[test]
    fn keyboard_navigation() {
        let mut b = bar();
        assert_eq!(b.toggle_keyboard(), MenuMsg::Repaint);
        assert!(b.is_active() && b.open_menu().is_none());
        b.key(MenuKey::Right);
        b.key(MenuKey::Right);
        b.key(MenuKey::Right); // wraps to File
        b.key(MenuKey::Down);
        assert_eq!(b.open_menu(), Some(0));
        // Down skips the separator: New -> Open -> Exit.
        b.key(MenuKey::Down);
        b.key(MenuKey::Down);
        assert_eq!(b.key(MenuKey::Enter), MenuMsg::Command(3));
        // Esc closes the drop-down first, then menu mode.
        b.toggle_keyboard();
        b.key(MenuKey::Down);
        b.key(MenuKey::Escape);
        assert!(b.is_active() && b.open_menu().is_none());
        assert_eq!(b.key(MenuKey::Escape), MenuMsg::Closed);
        assert_eq!(b.toggle_keyboard(), MenuMsg::Repaint);
        assert_eq!(b.toggle_keyboard(), MenuMsg::Closed);
    }

    #[test]
    fn mnemonics_open_menus_and_choose_items() {
        let mut b = bar();
        assert_eq!(b.mnemonic('v'), MenuMsg::Repaint);
        assert_eq!(b.open_menu(), Some(2));
        assert_eq!(b.key(MenuKey::Char('S')), MenuMsg::Command(20));
        assert_eq!(b.mnemonic('q'), MenuMsg::Nothing);
        // In an open menu a letter that isn't an item mnemonic switches menus.
        b.mnemonic('f');
        b.key(MenuKey::Char('e'));
        assert_eq!(b.open_menu(), Some(1));
    }

    #[test]
    fn checks_and_paint() {
        let mut b = bar();
        assert!(b.set_checked(20, true));
        assert!(!b.set_checked(20, true));
        assert!(b.is_checked(20) && !b.is_checked(1));
        b.mnemonic('v');
        let mut dl = DrawList::default();
        b.paint_dropdown(bounds(), &Theme::LIGHT, &M, &mut dl);
        assert!(
            dl.cmds.iter().any(|c| matches!(c, Cmd::Polyline { .. })),
            "check mark"
        );
        let mut dl = DrawList::default();
        b.paint_bar(bounds(), &Theme::LIGHT, &M, &mut dl);
        let underlines = dl
            .cmds
            .iter()
            .filter(|c| matches!(c, Cmd::Line { from, to, .. } if from.1 == to.1))
            .count();
        assert_eq!(underlines, 3, "mnemonic underlines while keyboard-opened");
    }
}
