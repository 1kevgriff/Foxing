//! Accessibility tree: each component describes itself as nodes (role, name, bounds,
//! state, children). Platform shells expose these to screen readers (UI Automation on
//! Windows) and route actions back as [`Action`]s.

use super::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Document,
    MenuBar,
    Menu,
    MenuItem,
    Separator,
    List,
    ListItem,
    StatusBar,
    Text,
}

/// What activating a node does; the host maps it to its own commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Open or close menu `i` (menu bar titles).
    ToggleMenu(usize),
    /// Run a command id (menu items).
    Command(u16),
    /// Activate list row `i`.
    ActivateRow(usize),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Node {
    pub role: Option<Role>,
    pub name: String,
    /// Bounds in the host window's client coordinates.
    pub rect: Rect,
    /// Text value (e.g. a document), if any.
    pub value: Option<String>,
    pub checked: Option<bool>,
    pub expanded: Option<bool>,
    pub selected: Option<bool>,
    /// The item with keyboard focus within this component.
    pub focused: bool,
    pub action: Option<Action>,
    pub children: Vec<Node>,
}

impl Node {
    pub fn new(role: Role, name: &str, rect: Rect) -> Self {
        Node {
            role: Some(role),
            name: name.to_owned(),
            rect,
            ..Default::default()
        }
    }

    /// Follows child indexes from this node.
    pub fn at(&self, path: &[usize]) -> Option<&Node> {
        path.iter().try_fold(self, |n, &i| n.children.get(i))
    }

    /// Path of the deepest node containing (x, y). Children are checked even outside
    /// the parent's bounds (a drop-down hangs below its menu bar).
    pub fn hit(&self, x: i32, y: i32) -> Option<Vec<usize>> {
        for (i, c) in self.children.iter().enumerate() {
            if let Some(mut p) = c.hit(x, y) {
                p.insert(0, i);
                return Some(p);
            }
        }
        self.rect.contains(x, y).then(Vec::new)
    }

    /// Path of the focused node, if any.
    pub fn focus_path(&self) -> Option<Vec<usize>> {
        if self.focused {
            return Some(Vec::new());
        }
        self.children.iter().enumerate().find_map(|(i, c)| {
            c.focus_path().map(|mut p| {
                p.insert(0, i);
                p
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Node {
        let mut root = Node::new(Role::MenuBar, "Menu", Rect::new(0, 0, 200, 20));
        let mut file = Node::new(Role::MenuItem, "File", Rect::new(0, 0, 50, 20));
        let mut open = Node::new(Role::MenuItem, "Open", Rect::new(0, 20, 120, 20));
        open.focused = true;
        file.children
            .push(Node::new(Role::MenuItem, "New", Rect::new(0, 40, 120, 20)));
        file.children.push(open);
        root.children.push(file);
        root.children
            .push(Node::new(Role::MenuItem, "Edit", Rect::new(50, 0, 50, 20)));
        root
    }

    #[test]
    fn paths_hits_and_focus() {
        let t = tree();
        assert_eq!(t.at(&[0, 1]).unwrap().name, "Open");
        assert!(t.at(&[5]).is_none());
        assert_eq!(t.hit(60, 5), Some(vec![1]));
        assert_eq!(t.hit(150, 5), Some(vec![]), "inside the bar, on no item");
        assert_eq!(t.hit(500, 5), None);
        assert_eq!(
            t.hit(10, 45),
            Some(vec![0, 0]),
            "child outside its parent's rect"
        );
        assert_eq!(t.focus_path(), Some(vec![0, 1]));
    }
}
