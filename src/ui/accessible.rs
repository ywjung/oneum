//! The accessibility tree the UI library makes (AccessKit's), flattened for a screen
//! reader that the app tells the tree itself: Android's TalkBack, through a view of
//! the Java shell (android/…/A11yHost.java), where eframe offers nothing.
//!
//! The UI library sends the whole tree with every frame it draws while accessibility
//! is on. [`Mirror`] keeps it (an update may also bring only what changed), and
//! [`Mirror::flatten`] gives its nodes in reading order, each under a small number of
//! its own that stays the same from frame to frame: the numbers Android knows the
//! nodes by.
use eframe::egui::accesskit::{Action, Affine, Node, NodeId, Role, Toggled, TreeUpdate};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// What a node can do and is, as bits (A11yHost.java reads them by the same values).
pub const CLICKABLE: u32 = 1;
pub const FOCUSABLE: u32 = 1 << 1;
pub const FOCUSED: u32 = 1 << 2;
pub const CHECKABLE: u32 = 1 << 3;
pub const CHECKED: u32 = 1 << 4;
pub const DISABLED: u32 = 1 << 5;
pub const EDITABLE: u32 = 1 << 6;
pub const SELECTED: u32 = 1 << 7;
pub const HEADING: u32 = 1 << 8;
pub const PASSWORD: u32 = 1 << 9;

/// One node as Android is told it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FlatNode {
    /// Its number (0 and up), the same as long as the node is there.
    pub id: i32,
    /// Its parent's number; -1 under the root (the view itself).
    pub parent: i32,
    /// Left, top, right, bottom in pixels of the drawing surface.
    pub bounds: [i32; 4],
    /// The Android class it is shown as (what TalkBack calls it: "button", …).
    pub class: &'static str,
    /// What it says: a button's words, a label's text, what is typed into a field.
    pub text: String,
    /// What a field is for (shown and read while it is empty).
    pub hint: String,
    pub flags: u32,
    /// A slider's value, least and most.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<[f64; 3]>,
    /// Its bounds are those of what is in it (see `enclose`).
    #[serde(skip)]
    encloses: bool,
}

/// The tree in reading order, and the node that has the keyboard's focus.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Flat {
    pub nodes: Vec<FlatNode>,
    /// The number of the focused node, -1 where none is.
    pub focus: i32,
}

/// The tree as the UI library last told it.
#[derive(Default)]
pub struct Mirror {
    nodes: HashMap<NodeId, Node>,
    root: Option<NodeId>,
    focus: Option<NodeId>,
    numbers: HashMap<NodeId, i32>,
    by_number: HashMap<i32, NodeId>,
    next: i32,
}

impl Mirror {
    /// Takes in an update: its nodes replace those of the same ids.
    pub fn apply(&mut self, update: &TreeUpdate) {
        for (id, node) in &update.nodes {
            self.nodes.insert(*id, node.clone());
        }
        if let Some(tree) = &update.tree {
            self.root = Some(tree.root);
        }
        self.focus = Some(update.focus);
    }

    /// The node Android knows by `number`.
    pub fn node_of(&self, number: i32) -> Option<NodeId> {
        self.by_number.get(&number).copied()
    }

    /// The tree in reading order (depth first, children in their order), without the
    /// root, which is the view itself. Nodes no longer in the tree are forgotten.
    pub fn flatten(&mut self) -> Flat {
        let mut flat = Vec::new();
        let mut seen = HashSet::new();
        if let Some(root) = self.root {
            // (Not recursive: a deep tree must not run out of stack.)
            let mut stack: Vec<(NodeId, i32, Affine)> = vec![(root, -1, Affine::IDENTITY)];
            while let Some((id, parent, outer)) = stack.pop() {
                if !seen.insert(id) {
                    continue;
                }
                if !self.nodes.contains_key(&id) {
                    continue;
                }
                let number = if id == root { -1 } else { self.number(id) };
                let node = &self.nodes[&id];
                let transform = match node.transform() {
                    Some(own) => outer * *own,
                    None => outer,
                };
                if node.is_hidden() {
                    continue;
                }
                if id != root {
                    flat.push(flat_node(
                        number,
                        parent,
                        node,
                        &transform,
                        self.focus == Some(id),
                    ));
                }
                // Pushed last first, so that the first child comes off the stack first.
                for child in node.children().iter().rev() {
                    stack.push((*child, number, transform));
                }
            }
        }
        enclose(&mut flat);
        drop_echoes(&mut flat);
        self.nodes.retain(|id, _| seen.contains(id));
        let by_number = &mut self.by_number;
        self.numbers.retain(|id, number| {
            let keep = seen.contains(id);
            if !keep {
                by_number.remove(number);
            }
            keep
        });
        let focus = self
            .focus
            .and_then(|id| self.numbers.get(&id).copied())
            .unwrap_or(-1);
        Flat { nodes: flat, focus }
    }

    fn number(&mut self, id: NodeId) -> i32 {
        if let Some(number) = self.numbers.get(&id) {
            return *number;
        }
        // (Numbers that are let go are not given again for a long while: Android may
        // still ask about a node by an old number.)
        let number = self.next;
        self.next = if self.next == i32::MAX {
            0
        } else {
            self.next + 1
        };
        self.numbers.insert(id, number);
        self.by_number.insert(number, id);
        number
    }
}

/// Leaves out a text that only says again what the button it lies on says (a row of
/// the phone's lists is a button with its name, and beside it in the tree the name as
/// it is drawn on it): TalkBack would read it twice.
fn drop_echoes(flat: &mut Vec<FlatNode>) {
    let buttons: Vec<([i32; 4], String)> = flat
        .iter()
        .filter(|node| node.flags & CLICKABLE != 0 && !node.text.is_empty())
        .map(|node| (node.bounds, node.text.clone()))
        .collect();
    let parents: HashSet<i32> = flat.iter().map(|node| node.parent).collect();
    let inside = |inner: [i32; 4], outer: [i32; 4]| {
        inner[0] >= outer[0] && inner[1] >= outer[1] && inner[2] <= outer[2] && inner[3] <= outer[3]
    };
    flat.retain(|node| {
        let echo = node.flags == 0
            && !node.text.is_empty()
            && !parents.contains(&node.id)
            && buttons
                .iter()
                .any(|(bounds, text)| *text == node.text && inside(node.bounds, *bounds));
        !echo
    });
}

/// Gives a node without bounds of its own (the UI library's containers) those of what
/// is in it: Android takes a node without bounds for one that cannot be seen, and
/// passes over all that is under it.
fn enclose(flat: &mut [FlatNode]) {
    let empty = |b: [i32; 4]| b[2] <= b[0] || b[3] <= b[1];
    let at: HashMap<i32, usize> = flat
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id, index))
        .collect();
    // Children come after their parents: from the end, a container's bounds are whole
    // before they count for its own parent.
    for index in (0..flat.len()).rev() {
        let child = flat[index].bounds;
        if empty(child) {
            continue;
        }
        let Some(&parent) = at.get(&flat[index].parent) else {
            continue;
        };
        if !flat[parent].encloses {
            if !empty(flat[parent].bounds) {
                continue;
            }
            flat[parent].encloses = true;
            flat[parent].bounds = child;
            continue;
        }
        let own = &mut flat[parent].bounds;
        *own = [
            own[0].min(child[0]),
            own[1].min(child[1]),
            own[2].max(child[2]),
            own[3].max(child[3]),
        ];
    }
}

fn flat_node(number: i32, parent: i32, node: &Node, transform: &Affine, focused: bool) -> FlatNode {
    let role = node.role();
    let editable = matches!(
        role,
        Role::TextInput
            | Role::MultilineTextInput
            | Role::SearchInput
            | Role::PasswordInput
            | Role::EmailInput
            | Role::NumberInput
            | Role::PhoneNumberInput
            | Role::UrlInput
    );
    let label = node.label().unwrap_or_default().trim();
    let value = node.value().unwrap_or_default().trim();
    let (text, hint) = if editable {
        let hint = node.placeholder().unwrap_or(label);
        (value.to_string(), hint.trim().to_string())
    } else if label.is_empty() {
        (value.to_string(), String::new())
    } else if value.is_empty() || role == Role::Slider {
        (label.to_string(), String::new())
    } else {
        (format!("{label}, {value}"), String::new())
    };
    let bounds = node
        .bounds()
        .map(|rect| {
            let rect = transform.transform_rect_bbox(rect);
            [
                rect.x0.round() as i32,
                rect.y0.round() as i32,
                rect.x1.round() as i32,
                rect.y1.round() as i32,
            ]
        })
        .unwrap_or_default();
    let toggled = node.toggled();
    let mut flags = 0;
    for (on, flag) in [
        (node.supports_action(Action::Click), CLICKABLE),
        (node.supports_action(Action::Focus), FOCUSABLE),
        (focused, FOCUSED),
        (toggled.is_some(), CHECKABLE),
        (toggled == Some(Toggled::True), CHECKED),
        (node.is_disabled(), DISABLED),
        (editable, EDITABLE),
        (node.is_selected() == Some(true), SELECTED),
        (role == Role::Heading, HEADING),
        (role == Role::PasswordInput, PASSWORD),
    ] {
        if on {
            flags |= flag;
        }
    }
    let range = match (
        node.numeric_value(),
        node.min_numeric_value(),
        node.max_numeric_value(),
    ) {
        (Some(value), Some(min), Some(max)) if role == Role::Slider => Some([value, min, max]),
        _ => None,
    };
    FlatNode {
        id: number,
        parent,
        bounds,
        class: class_of(role, editable),
        text,
        hint,
        flags,
        range,
        encloses: false,
    }
}

/// The Android class a node of `role` is shown as.
fn class_of(role: Role, editable: bool) -> &'static str {
    if editable {
        return "android.widget.EditText";
    }
    match role {
        Role::Button | Role::DefaultButton | Role::Link | Role::MenuItem | Role::Tab => {
            "android.widget.Button"
        }
        Role::CheckBox | Role::MenuItemCheckBox => "android.widget.CheckBox",
        Role::Switch => "android.widget.Switch",
        Role::RadioButton | Role::MenuItemRadio => "android.widget.RadioButton",
        Role::Slider => "android.widget.SeekBar",
        Role::ComboBox => "android.widget.Spinner",
        Role::ProgressIndicator => "android.widget.ProgressBar",
        Role::Image => "android.widget.ImageView",
        Role::Label | Role::Paragraph | Role::Heading | Role::TextRun => "android.widget.TextView",
        Role::ScrollView => "android.widget.ScrollView",
        _ => "android.view.View",
    }
}

/// What an action Android asks for (A11yHost.java's numbers) is to the UI library.
pub fn actions_for(code: i32) -> &'static [Action] {
    match code {
        0 => &[Action::Click],
        1 => &[Action::Focus],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::accesskit::{Rect, TreeId, TreeInfo};

    fn node(role: Role, label: &str, bounds: [f64; 4], children: &[u64]) -> Node {
        let mut node = Node::new(role);
        if !label.is_empty() {
            node.set_label(label);
        }
        node.set_bounds(Rect::new(bounds[0], bounds[1], bounds[2], bounds[3]));
        node.set_children(children.iter().map(|id| NodeId(*id)).collect::<Vec<_>>());
        node
    }

    fn update(nodes: Vec<(u64, Node)>, focus: u64) -> TreeUpdate {
        TreeUpdate {
            nodes: nodes.into_iter().map(|(id, n)| (NodeId(id), n)).collect(),
            tree: Some(TreeInfo::new(NodeId(1))),
            tree_id: TreeId::ROOT,
            focus: NodeId(focus),
        }
    }

    /// The nodes come in reading order, in pixels (the root says how many to a
    /// point), with what each is and can do; the root itself is the view.
    #[test]
    fn the_tree_is_flattened_in_reading_order_in_pixels() {
        let mut root = node(Role::Window, "", [0.0, 0.0, 400.0, 800.0], &[2, 5]);
        root.set_transform(Affine::scale(2.5));
        let list = node(
            Role::GenericContainer,
            "",
            [0.0, 0.0, 400.0, 700.0],
            &[3, 4],
        );
        let mut play = node(Role::Button, "Play", [10.0, 20.0, 50.0, 60.0], &[]);
        play.add_action(Action::Click);
        play.add_action(Action::Focus);
        let mut song = node(Role::Label, "", [0.0, 100.0, 400.0, 140.0], &[]);
        song.set_value("Song 하나");
        let mut search = node(Role::TextInput, "Search", [0.0, 700.0, 400.0, 740.0], &[]);
        search.set_value("iu");
        search.add_action(Action::Focus);
        let mut mirror = Mirror::default();
        mirror.apply(&update(
            vec![(1, root), (2, list), (3, play), (4, song), (5, search)],
            5,
        ));
        let flat = mirror.flatten();
        let texts: Vec<&str> = flat.nodes.iter().map(|n| n.text.as_str()).collect();
        assert_eq!(texts, ["", "Play", "Song 하나", "iu"]);
        let play = &flat.nodes[1];
        assert_eq!(play.bounds, [25, 50, 125, 150]);
        assert_eq!(play.class, "android.widget.Button");
        assert_eq!(play.flags, CLICKABLE | FOCUSABLE);
        assert_eq!(play.parent, flat.nodes[0].id);
        assert_eq!(flat.nodes[0].parent, -1);
        let search = &flat.nodes[3];
        assert_eq!(
            (search.hint.as_str(), search.class),
            ("Search", "android.widget.EditText")
        );
        assert_eq!(search.flags, FOCUSABLE | FOCUSED | EDITABLE);
        assert_eq!(flat.focus, search.id);
        assert_eq!(mirror.node_of(play.id), Some(NodeId(3)));
    }

    /// A node keeps its number while it is there; one that left is forgotten, and an
    /// update that brings only what changed leaves the rest as it was.
    #[test]
    fn numbers_stay_with_their_nodes() {
        let mut mirror = Mirror::default();
        let a = node(Role::Button, "A", [0.0, 0.0, 1.0, 1.0], &[]);
        let b = node(Role::Button, "B", [0.0, 0.0, 1.0, 1.0], &[]);
        let root = |children: &[u64]| node(Role::Window, "", [0.0, 0.0, 1.0, 1.0], children);
        mirror.apply(&update(vec![(1, root(&[2, 3])), (2, a.clone()), (3, b)], 1));
        let first = mirror.flatten();
        let number_of_a = first.nodes[0].id;
        assert_eq!(
            first.focus, -1,
            "the root is the view: no node of it has focus"
        );
        // B leaves; A stays and keeps its number.
        mirror.apply(&update(vec![(1, root(&[2]))], 1));
        let second = mirror.flatten();
        assert_eq!(second.nodes.len(), 1);
        assert_eq!(second.nodes[0].id, number_of_a);
        assert_eq!(mirror.node_of(first.nodes[1].id), None);
        // A hidden node and what is under it are left out.
        let mut hidden = node(Role::GenericContainer, "", [0.0, 0.0, 1.0, 1.0], &[2]);
        hidden.set_hidden();
        mirror.apply(&update(vec![(1, root(&[4])), (4, hidden), (2, a)], 1));
        assert!(mirror.flatten().nodes.is_empty());
    }

    /// The words drawn in a button are not a node of their own beside it.
    #[test]
    fn a_text_that_says_again_what_its_button_says_is_left_out() {
        let mut mirror = Mirror::default();
        let mut row = node(Role::Button, "Songs", [0.0, 0.0, 10.0, 10.0], &[]);
        row.add_action(Action::Click);
        let between = node(Role::GenericContainer, "", [0.0, 0.0, 10.0, 10.0], &[3, 4]);
        let mut elsewhere = node(Role::Label, "", [20.0, 1.0, 30.0, 5.0], &[]);
        elsewhere.set_value("Songs");
        let mut name = node(Role::Label, "", [1.0, 1.0, 5.0, 5.0], &[]);
        name.set_value("Songs");
        let mut count = node(Role::Label, "", [6.0, 1.0, 9.0, 5.0], &[]);
        count.set_value("9");
        let root = node(Role::Window, "", [0.0, 0.0, 30.0, 10.0], &[2, 5, 6]);
        mirror.apply(&update(
            vec![
                (1, root),
                (2, row),
                (5, between),
                (3, name),
                (4, count),
                (6, elsewhere),
            ],
            1,
        ));
        let texts: Vec<String> = mirror.flatten().nodes.into_iter().map(|n| n.text).collect();
        assert_eq!(
            texts,
            ["Songs", "", "9", "Songs"],
            "the same words elsewhere stay"
        );
    }

    /// A container without bounds of its own takes those of all that is in it, so that
    /// Android does not pass over it as unseen.
    #[test]
    fn a_container_without_bounds_takes_those_of_its_children() {
        let mut mirror = Mirror::default();
        let mut outer = Node::new(Role::GenericContainer);
        outer.set_children(vec![NodeId(3)]);
        let mut inner = Node::new(Role::GenericContainer);
        inner.set_children(vec![NodeId(4), NodeId(5)]);
        let a = node(Role::Button, "A", [10.0, 10.0, 20.0, 20.0], &[]);
        let b = node(Role::Button, "B", [30.0, 5.0, 40.0, 15.0], &[]);
        let root = node(Role::Window, "", [0.0, 0.0, 100.0, 100.0], &[2]);
        mirror.apply(&update(
            vec![(1, root), (2, outer), (3, inner), (4, a), (5, b)],
            1,
        ));
        let flat = mirror.flatten();
        assert_eq!(flat.nodes[0].bounds, [10, 5, 40, 20], "the outer one");
        assert_eq!(flat.nodes[1].bounds, [10, 5, 40, 20], "the inner one");
        assert_eq!(
            flat.nodes[2].bounds,
            [10, 10, 20, 20],
            "a button keeps its own"
        );
    }

    #[test]
    fn what_android_asks_for_is_an_action_of_the_library() {
        assert_eq!(actions_for(0), [Action::Click]);
        assert_eq!(actions_for(1), [Action::Focus]);
        assert!(actions_for(2).is_empty());
    }

    #[test]
    fn a_checkbox_and_a_slider_say_their_state() {
        let mut mirror = Mirror::default();
        let mut check = node(Role::CheckBox, "Shuffle", [0.0, 0.0, 1.0, 1.0], &[]);
        check.set_toggled(Toggled::True);
        let mut slider = node(Role::Slider, "Volume", [0.0, 0.0, 1.0, 1.0], &[]);
        slider.set_numeric_value(0.5);
        slider.set_min_numeric_value(0.0);
        slider.set_max_numeric_value(1.0);
        slider.set_value("50%");
        let root = node(Role::Window, "", [0.0, 0.0, 1.0, 1.0], &[2, 3]);
        mirror.apply(&update(vec![(1, root), (2, check), (3, slider)], 1));
        let flat = mirror.flatten();
        assert_eq!(flat.nodes[0].flags, CHECKABLE | CHECKED);
        assert_eq!(flat.nodes[1].range, Some([0.5, 0.0, 1.0]));
        assert_eq!(flat.nodes[1].text, "Volume");
        assert!(
            serde_json::to_string(&flat)
                .unwrap()
                .contains("\"class\":\"android.widget.SeekBar\"")
        );
    }
}
