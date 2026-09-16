//! The accessibility tree, as it crosses the boundary (ADR-0019, #9).
//!
//! ADR-0012 put parsing, cascade and layout in a sandboxed child, and the
//! parent deliberately receives only pixels, link rectangles, a title, the
//! rendering mode and a little form geometry. A screen reader wants none of
//! those: it wants a semantic tree, and the semantic tree is the DOM plus the
//! box tree, both of which stay on the far side.
//!
//! ADR-0019 settled the shape. The tree crosses as *data* and the parent builds
//! the native objects from it, because the alternative — registering with UI
//! Automation or AT-SPI from inside the sandbox — hands platform API handles to
//! the process that must not have them, which is the one thing ADR-0012 bought.
//!
//! The cost of that choice is this file. The wire stops being a pixmap and some
//! rectangles and gains an arbitrary-depth tree of nodes with text in it, every
//! byte chosen by the untrusted side. So the bounds below are not tidiness:
//! they are the reason the decision was affordable, and each is recorded with
//! the measurement it came from.

use layout::Rect;

use crate::wire::{Reader, WireError, Writer};

/// Deepest tree the parent will accept.
///
/// The parent walks this recursively to build one native node per entry, so
/// depth is a stack-overflow vector in the *trusted* process. Measured: the era
/// fixture is 14 deep across 185 elements, and the deepest document in the CSS
/// 2.1 suite is 13.
pub const MAX_DEPTH: usize = 64;

/// Most nodes the parent will accept.
///
/// One allocation per entry. Measured: the era fixture yields 40 and the
/// largest fixture here 63, so this is three orders of magnitude of headroom
/// and a bounded, predictable allocation. `MAX_FRAME` alone is not enough — at
/// the size a small node encodes to, a legal 64 MiB frame could carry on the
/// order of a million of them.
pub const MAX_NODES: usize = 65_536;

/// Longest single string, in bytes.
///
/// A name is a label, not a document. The longest measured on any fixture here
/// is 430 bytes.
pub const MAX_STRING: usize = 4 * 1024;

/// Most text in the whole tree, in bytes.
///
/// Bounds the text independently of how it is divided into nodes, so 65 536
/// names of 4 KiB each is not a legal frame.
pub const MAX_TEXT: usize = 1024 * 1024;

/// What a node *is*, from a set the wire knows.
///
/// Never a string. A role that crossed as text would be an attacker-chosen
/// string handed to a platform accessibility API, and an unrecognised
/// discriminant is a [`WireError::Unknown`] that refuses the frame — which is
/// how every other enum on this wire already behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The page itself, and the only node with no parent.
    Document,
    /// A box that groups its children and says nothing else.
    Group,
    /// A run of text that is not any of the below.
    Text,
    /// A paragraph.
    Paragraph,
    /// A heading. Its `level` says which.
    Heading,
    /// A link, whose `value` is where it goes.
    Link,
    /// A button.
    Button,
    /// A field that is typed into, whose `value` is what is in it.
    TextField,
    /// A checkbox. `on` says whether it is ticked.
    CheckBox,
    /// A radio button. `on` says whether it is chosen.
    RadioButton,
    /// A `<select>`, whose `value` is the option it is on.
    ComboBox,
    /// A list.
    List,
    /// One item of a list.
    ListItem,
    /// A table.
    Table,
    /// One row of a table.
    Row,
    /// One cell of a row.
    Cell,
    /// A cell that heads its column or row.
    HeaderCell,
    /// An image, whose `name` is its alternative text.
    Image,
    /// A horizontal rule.
    Separator,
}

impl Role {
    fn tag(self) -> u8 {
        match self {
            Role::Document => 0,
            Role::Group => 1,
            Role::Text => 2,
            Role::Paragraph => 3,
            Role::Heading => 4,
            Role::Link => 5,
            Role::Button => 6,
            Role::TextField => 7,
            Role::CheckBox => 8,
            Role::RadioButton => 9,
            Role::ComboBox => 10,
            Role::List => 11,
            Role::ListItem => 12,
            Role::Table => 13,
            Role::Row => 14,
            Role::Cell => 15,
            Role::HeaderCell => 16,
            Role::Image => 17,
            Role::Separator => 18,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, WireError> {
        Ok(match tag {
            0 => Role::Document,
            1 => Role::Group,
            2 => Role::Text,
            3 => Role::Paragraph,
            4 => Role::Heading,
            5 => Role::Link,
            6 => Role::Button,
            7 => Role::TextField,
            8 => Role::CheckBox,
            9 => Role::RadioButton,
            10 => Role::ComboBox,
            11 => Role::List,
            12 => Role::ListItem,
            13 => Role::Table,
            14 => Role::Row,
            15 => Role::Cell,
            16 => Role::HeaderCell,
            17 => Role::Image,
            18 => Role::Separator,
            _ => return Err(WireError::Unknown),
        })
    }
}

/// One node of the tree.
///
/// Flat rather than nested, with `children` counting the entries that follow it
/// in pre-order. That is not a storage detail: a nested encoding is decoded
/// recursively, and recursion driven by a length field the untrusted side chose
/// is a stack overflow waiting for the right frame. This shape is read in one
/// loop with an explicit stack, so the depth bound is something the decoder
/// *measures* rather than something it survives.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// What it is.
    pub role: Role,
    /// Its label: a link's text, an image's alternative text, a heading's words.
    pub name: String,
    /// What it holds, where that is a different thing from its label — a
    /// link's target, a field's contents, the option a `<select>` is on.
    pub value: Option<String>,
    /// Where it is, in canvas coordinates.
    pub rect: Rect,
    /// Heading level, 1 to 6. Zero for everything else.
    pub level: u8,
    /// Whether a checkbox is ticked or a radio chosen.
    pub on: bool,
    /// How many of the entries after this one are its immediate children.
    pub children: u32,
}

impl Node {
    /// A node of `role` with no name and nothing in it.
    pub fn new(role: Role, rect: Rect) -> Self {
        Self {
            role,
            name: String::new(),
            value: None,
            rect,
            level: 0,
            on: false,
            children: 0,
        }
    }

    /// Its own text, for the total-text bound.
    fn text_bytes(&self) -> usize {
        self.name.len() + self.value.as_ref().map_or(0, String::len)
    }

    fn write(&self, writer: &mut Writer) {
        writer.tag(self.role.tag());
        writer.str(&self.name);
        writer.some(self.value.is_some());
        if let Some(value) = &self.value {
            writer.str(value);
        }
        writer.f32(self.rect.x);
        writer.f32(self.rect.y);
        writer.f32(self.rect.width);
        writer.f32(self.rect.height);
        writer.tag(self.level);
        writer.some(self.on);
        writer.u32(self.children);
    }

    fn read(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let role = Role::from_tag(reader.tag()?)?;
        let name = bounded_str(reader)?;
        let value = reader.some()?.then(|| bounded_str(reader)).transpose()?;
        let rect = Rect {
            x: reader.f32()?,
            y: reader.f32()?,
            width: reader.f32()?,
            height: reader.f32()?,
        };
        let level = reader.tag()?;
        let on = reader.some()?;
        let children = reader.u32()?;
        Ok(Self {
            role,
            name,
            value,
            rect,
            level,
            on,
            children,
        })
    }
}

/// A string, refused rather than truncated if it is over the bound.
///
/// Truncating would be worse than refusing: a name cut in half is a control
/// described wrongly, and the reader has no way to tell that it was cut.
fn bounded_str(reader: &mut Reader<'_>) -> Result<String, WireError> {
    let text = reader.str()?;
    if text.len() > MAX_STRING {
        return Err(WireError::BadLength);
    }
    Ok(text)
}

/// The page as a screen reader would read it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tree {
    /// Every node, in pre-order, the document first.
    pub nodes: Vec<Node>,
    /// Which node holds the keyboard, as an index into `nodes`.
    ///
    /// #151 gave the child a keyboard focus; naming it here is what lets a
    /// screen reader follow the keyboard rather than guess from geometry.
    pub focus: Option<u32>,
}

impl Tree {
    /// Encodes the tree into `writer`.
    pub fn write(&self, writer: &mut Writer) {
        writer.u32(self.nodes.len() as u32);
        for node in &self.nodes {
            node.write(writer);
        }
        writer.some(self.focus.is_some());
        if let Some(focus) = self.focus {
            writer.u32(focus);
        }
    }

    /// Decodes a tree, refusing one that breaks any of ADR-0019's bounds.
    ///
    /// Refused rather than truncated, throughout. A truncated accessibility
    /// tree is a page described wrongly, which is worse than a page described
    /// not at all — because the reader has no way to tell which they have.
    pub fn read(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let count = reader.count()?;
        if count > MAX_NODES {
            return Err(WireError::BadLength);
        }
        let mut nodes = Vec::with_capacity(count);
        let mut text = 0usize;
        for _ in 0..count {
            let node = Node::read(reader)?;
            text = text.saturating_add(node.text_bytes());
            if text > MAX_TEXT {
                return Err(WireError::BadLength);
            }
            nodes.push(node);
        }
        let focus = reader.some()?.then(|| reader.u32()).transpose()?;
        if let Some(focus) = focus
            && focus as usize >= nodes.len()
        {
            return Err(WireError::BadLength);
        }
        let tree = Self { nodes, focus };
        tree.check_shape()?;
        Ok(tree)
    }

    /// Whether the child counts describe one tree, and how deep it is.
    ///
    /// Two failures to tell apart, and both matter. A tree whose counts do not
    /// add up is not a tree at all — the parent would build a forest, or walk
    /// off the end of the list — and a tree that is merely *deep* is a stack
    /// overflow in the process that must not fall over. Neither is detectable
    /// from the frame's length, which is why this is a pass of its own rather
    /// than something the read loop could have noticed.
    fn check_shape(&self) -> Result<(), WireError> {
        if self.nodes.is_empty() {
            return Ok(());
        }
        // How many children are still owed at each level, innermost last.
        let mut owed: Vec<u32> = Vec::new();
        for (at, node) in self.nodes.iter().enumerate() {
            while owed.last() == Some(&0) {
                owed.pop();
            }
            match owed.last_mut() {
                Some(remaining) => *remaining -= 1,
                // Nothing is owed a child, so this node has no parent. Only the
                // first node may be in that position: a second root is a forest
                // wearing a tree's encoding.
                None if at > 0 => return Err(WireError::BadLength),
                None => {}
            }
            if owed.len() + 1 > MAX_DEPTH {
                return Err(WireError::BadLength);
            }
            owed.push(node.children);
        }
        // Anything still owed is a node that promised children the frame did
        // not carry.
        if owed.iter().any(|remaining| *remaining != 0) {
            return Err(WireError::BadLength);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> Rect {
        Rect {
            x: 1.0,
            y: 2.0,
            width: 3.0,
            height: 4.0,
        }
    }

    fn node(role: Role, children: u32) -> Node {
        Node {
            children,
            ..Node::new(role, rect())
        }
    }

    fn round_trip(tree: &Tree) -> Result<Tree, WireError> {
        let mut writer = Writer::new();
        tree.write(&mut writer);
        let bytes = writer.finish();
        let mut reader = Reader::new(&bytes);
        let out = Tree::read(&mut reader)?;
        reader.finish()?;
        Ok(out)
    }

    #[test]
    fn a_tree_survives_the_wire() {
        let tree = Tree {
            nodes: vec![
                node(Role::Document, 2),
                Node {
                    name: "Chapter one".to_owned(),
                    level: 2,
                    ..node(Role::Heading, 0)
                },
                Node {
                    name: "Home".to_owned(),
                    value: Some("https://example.com/".to_owned()),
                    ..node(Role::Link, 0)
                },
            ],
            focus: Some(2),
        };
        assert_eq!(round_trip(&tree), Ok(tree));
    }

    #[test]
    fn an_empty_tree_is_a_legal_tree() {
        // A page rendered before anything was listening, and the shape the
        // parent holds until it asks.
        assert_eq!(round_trip(&Tree::default()), Ok(Tree::default()));
    }

    #[test]
    fn a_role_the_wire_does_not_know_refuses_the_frame() {
        // The reason roles are an enum and not a string: this is an
        // attacker-chosen byte on its way to a platform accessibility API.
        let mut writer = Writer::new();
        writer.u32(1);
        writer.tag(200);
        let bytes = writer.finish();
        assert_eq!(
            Tree::read(&mut Reader::new(&bytes)),
            Err(WireError::Unknown),
        );
    }

    #[test]
    fn a_tree_deeper_than_the_bound_is_refused() {
        // Each node claims one child, so the list is a chain.
        let nodes: Vec<Node> = (0..=MAX_DEPTH)
            .map(|at| node(Role::Group, u32::from(at < MAX_DEPTH)))
            .collect();
        assert_eq!(
            round_trip(&Tree { nodes, focus: None }),
            Err(WireError::BadLength),
        );
    }

    #[test]
    fn a_tree_exactly_at_the_depth_bound_is_accepted() {
        // The bound is a limit and not an off-by-one: a document 64 deep is
        // legal, and refusing it would be describing a page not at all because
        // it was one level past a number somebody picked.
        let nodes: Vec<Node> = (0..MAX_DEPTH)
            .map(|at| node(Role::Group, u32::from(at + 1 < MAX_DEPTH)))
            .collect();
        assert!(round_trip(&Tree { nodes, focus: None }).is_ok());
    }

    #[test]
    fn a_node_promising_children_the_frame_does_not_carry_is_refused() {
        let tree = Tree {
            nodes: vec![node(Role::Document, 4), node(Role::Text, 0)],
            focus: None,
        };
        assert_eq!(round_trip(&tree), Err(WireError::BadLength));
    }

    #[test]
    fn a_second_root_is_refused() {
        // Two nodes, neither owed to the other. The parent walks from the first
        // node and would silently never see the second.
        let tree = Tree {
            nodes: vec![node(Role::Document, 0), node(Role::Document, 0)],
            focus: None,
        };
        assert_eq!(round_trip(&tree), Err(WireError::BadLength));
    }

    #[test]
    fn a_name_longer_than_the_bound_is_refused_rather_than_cut() {
        let tree = Tree {
            nodes: vec![Node {
                name: "a".repeat(MAX_STRING + 1),
                ..node(Role::Text, 0)
            }],
            focus: None,
        };
        assert_eq!(round_trip(&tree), Err(WireError::BadLength));
    }

    #[test]
    fn text_is_bounded_across_the_whole_tree_and_not_only_per_node() {
        // Otherwise 65 536 names of 4 KiB each is a legal frame, and the two
        // per-node bounds multiply into a quarter of a gigabyte.
        let each = MAX_STRING;
        let needed = MAX_TEXT / each + 2;
        let nodes: Vec<Node> = (0..needed)
            .map(|at| Node {
                name: "a".repeat(each),
                children: u32::from(at + 1 < needed),
                ..node(Role::Group, 0)
            })
            .collect();
        assert_eq!(
            round_trip(&Tree { nodes, focus: None }),
            Err(WireError::BadLength),
        );
    }

    #[test]
    fn a_focus_that_names_no_node_is_refused() {
        let tree = Tree {
            nodes: vec![node(Role::Document, 0)],
            focus: Some(7),
        };
        assert_eq!(round_trip(&tree), Err(WireError::BadLength));
    }

    #[test]
    fn a_count_larger_than_the_frame_allocates_nothing() {
        // `Vec::with_capacity` on an unchecked count is the allocation this
        // whole file exists to make impossible.
        let mut writer = Writer::new();
        writer.u32(u32::MAX);
        let bytes = writer.finish();
        assert_eq!(
            Tree::read(&mut Reader::new(&bytes)),
            Err(WireError::BadLength),
        );
    }
}
