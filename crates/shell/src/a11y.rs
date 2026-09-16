//! Turning the tree the renderer sends into the objects a screen reader reads
//! (ADR-0019, #178).
//!
//! This is the trusted half. [`crate::access`] builds the tree in the child out
//! of the DOM and the box tree; it crosses as data, bounded; and here the parent
//! turns it into AccessKit nodes, which AccessKit turns into UI Automation on
//! Windows, NSAccessibility on macOS and AT-SPI on Linux. Taking that crate is
//! the whole reason the decision was affordable — three platform integrations
//! written here is not a serious proposal (ADR-0007).
//!
//! Two properties this file has to keep, because they are what ADR-0019 traded
//! for the larger parsing surface.
//!
//! **It runs only when something is listening.** AccessKit says when an
//! assistive technology has attached, and until then the child is never asked.
//! A page costs nothing when nothing is using it, and the code below is not
//! exercised at all.
//!
//! **It trusts the bounds and nothing else.** The tree arrived through a
//! decoder that refused anything past ADR-0019's four limits, so the walk here
//! is bounded by construction — which matters, because it is a recursion in the
//! process holding the network and the disk.

use accesskit::{Affine, NodeId, Rect, Role, TreeId, TreeInfo, TreeUpdate};
use sandbox::access;

/// The id of the node AccessKit is told is the root.
///
/// Ids are positions in the wire tree plus one, so zero is free for this. A
/// wire tree always has its own root at index 0 and would take id 1; this is
/// the window, which is a level above the page and is what a screen reader
/// first meets.
const WINDOW: NodeId = NodeId(0);

/// Where the page sits inside the window, and how it has been scrolled.
///
/// AccessKit wants coordinates relative to the window's origin, in physical
/// pixels, y downwards. The tree's rectangles are in *canvas* coordinates — the
/// document's own space, which the window draws below its chrome and shifted by
/// however far the reader has scrolled.
///
/// Carried as a pair rather than applied to every rectangle, because AccessKit
/// has somewhere to put exactly this: one transform on the root, which every
/// descendant inherits. Scrolling then changes one number instead of thousands,
/// and the nodes themselves stay in the coordinates the child sent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    /// Height of the chrome above the page, in physical pixels.
    pub chrome: f32,
    /// How far down the document the window is showing.
    pub scroll: f32,
}

impl Viewport {
    /// The transform from canvas coordinates to window coordinates.
    fn transform(self) -> Affine {
        Affine::translate((0.0, f64::from(self.chrome) - f64::from(self.scroll)))
    }
}

/// Builds the update AccessKit is given for a page.
///
/// A whole tree every time, never a patch. ADR-0003 means this engine does not
/// mutate a page after load, so there is no incremental case to get right —
/// which removes the entire category of tree-update bugs that dominates
/// accessibility work in scripted browsers, and is worth taking as the
/// advantage it is rather than treating the simpler design as a limitation.
pub fn update(tree: &access::Tree, title: &str, viewport: Viewport) -> TreeUpdate {
    let mut nodes = Vec::with_capacity(tree.nodes.len() + 1);

    // The window, which is not on the page and so is not in the tree. A screen
    // reader meets this first and reads its label as "what am I looking at",
    // which is the title bar's answer and not the document's.
    let mut window = accesskit::Node::new(Role::Window);
    window.set_label(title.to_owned());
    window.set_transform(viewport.transform());
    window.set_children(page_root(tree));
    nodes.push((WINDOW, window));

    for (at, node) in tree.nodes.iter().enumerate() {
        nodes.push((id_of(at), build(node, children_of(tree, at))));
    }

    TreeUpdate {
        nodes,
        tree: Some(TreeInfo {
            root: WINDOW,
            toolkit_name: Some("2kbrowser".to_owned()),
            toolkit_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        }),
        // Whatever holds the keyboard, or the window when nothing does. Never
        // left unset: AccessKit requires a focus, and a tree that named none
        // would be a page a screen reader could not start reading.
        focus: tree.focus.map(|at| id_of(at as usize)).unwrap_or(WINDOW),
        // The main tree and not a subtree. Subtrees are for an embedded
        // document with an id of its own, which a frameset here is not: its
        // frames are grafted into one tree by the child before it is sent.
        tree_id: TreeId::ROOT,
    }
}

/// An empty page, for a window with nothing in it yet.
///
/// Sent rather than nothing at all, because AccessKit needs a tree the moment
/// an assistive technology attaches and a window that answered "not yet" would
/// be a window it decided was broken.
pub fn nothing_yet(title: &str) -> TreeUpdate {
    let nowhere = Viewport {
        chrome: 0.0,
        scroll: 0.0,
    };
    update(&access::Tree::default(), title, nowhere)
}

/// The id for the wire node at `at`.
///
/// Position plus one, so that [`WINDOW`] can have zero. Positions are stable
/// for the life of one tree and every tree is sent whole, so nothing depends on
/// an id meaning the same thing twice.
fn id_of(at: usize) -> NodeId {
    NodeId(at as u64 + 1)
}

/// The page's own root, as the window's only child.
fn page_root(tree: &access::Tree) -> Vec<NodeId> {
    if tree.nodes.is_empty() {
        Vec::new()
    } else {
        vec![id_of(0)]
    }
}

/// The ids of the node at `at`'s immediate children.
///
/// The wire tree counts children rather than naming them, so this walks
/// forward: the first child is the next entry, and each subsequent sibling
/// starts after the previous one's whole subtree. Bounded by the list's own
/// length, which the decoder has already bounded.
fn children_of(tree: &access::Tree, at: usize) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut child = at + 1;
    for _ in 0..tree.nodes[at].children {
        if child >= tree.nodes.len() {
            break;
        }
        out.push(id_of(child));
        child += subtree_len(tree, child);
    }
    out
}

/// How many entries the subtree rooted at `at` occupies, itself included.
fn subtree_len(tree: &access::Tree, at: usize) -> usize {
    let mut len = 1;
    let mut remaining = tree.nodes[at].children;
    let mut cursor = at + 1;
    while remaining > 0 && cursor < tree.nodes.len() {
        let taken = subtree_len(tree, cursor);
        len += taken;
        cursor += taken;
        remaining -= 1;
    }
    len
}

/// One AccessKit node from one wire node.
fn build(node: &access::Node, children: Vec<NodeId>) -> accesskit::Node {
    let mut out = accesskit::Node::new(role_of(node.role));
    if !node.name.is_empty() {
        out.set_label(node.name.clone());
    }
    if let Some(value) = &node.value {
        out.set_value(value.clone());
    }
    // A node the layout never placed has an empty rectangle, and a zero-sized
    // box is not somewhere a pointer can be. Left unset rather than sent as a
    // point: "I do not know where this is" is a thing AccessKit understands,
    // and a rectangle at the origin is a lie about it.
    if node.rect.width > 0.0 && node.rect.height > 0.0 {
        out.set_bounds(Rect {
            x0: f64::from(node.rect.x),
            y0: f64::from(node.rect.y),
            x1: f64::from(node.rect.x + node.rect.width),
            y1: f64::from(node.rect.y + node.rect.height),
        });
    }
    if node.role == access::Role::Heading && node.level > 0 {
        out.set_level(usize::from(node.level));
    }
    if matches!(
        node.role,
        access::Role::CheckBox | access::Role::RadioButton
    ) {
        out.set_toggled(if node.on {
            accesskit::Toggled::True
        } else {
            accesskit::Toggled::False
        });
    }
    if matches!(node.role, access::Role::Cell | access::Role::HeaderCell) {
        // Only where they are more than one. A cell that spans nothing is every
        // cell, and saying so on all of them is noise in the tree for no
        // question anybody asked.
        if node.span.0 > 1 {
            out.set_column_span(node.span.0 as usize);
        }
        if node.span.1 > 1 {
            out.set_row_span(node.span.1 as usize);
        }
    }
    if !children.is_empty() {
        out.set_children(children);
    }
    out
}

/// What AccessKit calls each of the roles the wire knows.
///
/// A total function over a closed enum, so adding a role to the wire is a
/// compile error here rather than a node that silently reads as `Unknown`.
fn role_of(role: access::Role) -> Role {
    match role {
        // Not `Document`: `RootWebArea` is what every browser reports for the
        // page itself, and screen readers have modes that key off it — "browse
        // mode" on Windows is entered because of this node.
        access::Role::Document => Role::RootWebArea,
        access::Role::Group => Role::GenericContainer,
        access::Role::Text => Role::Label,
        access::Role::Paragraph => Role::Paragraph,
        access::Role::Heading => Role::Heading,
        access::Role::Link => Role::Link,
        access::Role::Button => Role::Button,
        access::Role::TextField => Role::TextInput,
        access::Role::CheckBox => Role::CheckBox,
        access::Role::RadioButton => Role::RadioButton,
        access::Role::ComboBox => Role::ComboBox,
        access::Role::List => Role::List,
        access::Role::ListItem => Role::ListItem,
        access::Role::Table => Role::Table,
        access::Role::Row => Role::Row,
        access::Role::Cell => Role::Cell,
        access::Role::HeaderCell => Role::ColumnHeader,
        access::Role::Image => Role::Image,
        access::Role::Separator => Role::Splitter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32) -> layout::Rect {
        layout::Rect {
            x,
            y,
            width: 100.0,
            height: 20.0,
        }
    }

    fn node(role: access::Role, children: u32) -> access::Node {
        access::Node {
            children,
            ..access::Node::new(role, rect(10.0, 20.0))
        }
    }

    /// A page, a heading, and a table of one row and two cells.
    fn page() -> access::Tree {
        access::Tree {
            nodes: vec![
                node(access::Role::Document, 2),
                access::Node {
                    name: "Chapter one".to_owned(),
                    level: 3,
                    ..node(access::Role::Heading, 0)
                },
                node(access::Role::Table, 1),
                node(access::Role::Row, 2),
                access::Node {
                    name: "Name".to_owned(),
                    span: (2, 1),
                    ..node(access::Role::HeaderCell, 0)
                },
                access::Node {
                    name: "Ada".to_owned(),
                    ..node(access::Role::Cell, 0)
                },
            ],
            focus: Some(1),
        }
    }

    fn viewport() -> Viewport {
        Viewport {
            chrome: 46.0,
            scroll: 100.0,
        }
    }

    /// The node with a given id, from an update.
    fn at(update: &TreeUpdate, id: NodeId) -> &accesskit::Node {
        &update
            .nodes
            .iter()
            .find(|(node_id, _)| *node_id == id)
            .expect("a node with that id")
            .1
    }

    #[test]
    fn the_window_is_the_root_and_the_page_is_its_child() {
        // A screen reader meets the window first and reads its label as "what
        // am I looking at" — which is the title bar's answer, not the page's.
        let built = update(&page(), "A page — 2kbrowser", viewport());
        let tree = built.tree.as_ref().expect("a tree");
        assert_eq!(tree.root, WINDOW);

        let window = at(&built, WINDOW);
        assert_eq!(window.role(), Role::Window);
        assert_eq!(window.label(), Some("A page — 2kbrowser"));
        assert_eq!(window.children(), &[id_of(0)]);
        assert_eq!(at(&built, id_of(0)).role(), Role::RootWebArea);
    }

    #[test]
    fn the_child_counts_become_the_right_children() {
        // The wire counts children rather than naming them, so this is where a
        // subtree of the wrong length would quietly reparent half a page.
        let built = update(&page(), "t", viewport());
        assert_eq!(
            at(&built, id_of(0)).children(),
            &[id_of(1), id_of(2)],
            "the page holds the heading and the table, not the row",
        );
        assert_eq!(at(&built, id_of(2)).children(), &[id_of(3)]);
        assert_eq!(at(&built, id_of(3)).children(), &[id_of(4), id_of(5)]);
        assert!(at(&built, id_of(4)).children().is_empty());
    }

    #[test]
    fn the_page_is_moved_by_the_chrome_and_the_scroll() {
        // One transform on the root rather than arithmetic on every rectangle:
        // scrolling then changes one number, and the nodes stay in the
        // coordinates the child sent.
        let built = update(&page(), "t", viewport());
        let moved = at(&built, WINDOW)
            .transform()
            .expect("the window carries the transform")
            .as_coeffs();
        // A row 100 down the document, with 46 of chrome and 100 scrolled away,
        // is 54 above the window's origin — which is to say under the chrome,
        // where the page area starts.
        assert_eq!([moved[4], moved[5]], [0.0, -54.0]);

        // And the nodes themselves are untouched.
        let bounds = at(&built, id_of(1)).bounds().expect("a heading has a box");
        assert_eq!((bounds.x0, bounds.y0), (10.0, 20.0));
        assert_eq!((bounds.x1, bounds.y1), (110.0, 40.0));
    }

    #[test]
    fn a_node_nobody_placed_has_no_bounds_rather_than_a_point() {
        // "I do not know where this is" is a thing AccessKit understands; a
        // rectangle at the origin is a lie about it.
        let tree = access::Tree {
            nodes: vec![access::Node::new(
                access::Role::Document,
                layout::Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 0.0,
                    height: 0.0,
                },
            )],
            focus: None,
        };
        let built = update(&tree, "t", viewport());
        assert!(at(&built, id_of(0)).bounds().is_none());
    }

    #[test]
    fn a_heading_a_span_and_a_tick_survive_the_mapping() {
        let built = update(&page(), "t", viewport());
        assert_eq!(at(&built, id_of(1)).level(), Some(3));
        assert_eq!(at(&built, id_of(4)).column_span(), Some(2));
        assert_eq!(
            at(&built, id_of(5)).column_span(),
            None,
            "a cell that spans nothing says nothing",
        );

        let ticked = access::Tree {
            nodes: vec![access::Node {
                on: true,
                ..access::Node::new(access::Role::CheckBox, rect(0.0, 0.0))
            }],
            focus: None,
        };
        let built = update(&ticked, "t", viewport());
        assert_eq!(
            at(&built, id_of(0)).toggled(),
            Some(accesskit::Toggled::True)
        );
    }

    #[test]
    fn the_focus_is_the_node_that_holds_the_keyboard() {
        let built = update(&page(), "t", viewport());
        assert_eq!(built.focus, id_of(1));
    }

    #[test]
    fn a_tree_with_no_focus_focuses_the_window() {
        // AccessKit requires one, and a tree that named none would be a page a
        // screen reader could not start reading.
        let mut unfocused = page();
        unfocused.focus = None;
        assert_eq!(update(&unfocused, "t", viewport()).focus, WINDOW);
    }

    #[test]
    fn an_empty_page_is_still_a_tree() {
        // What a window answers with before it has rendered anything. Sent
        // rather than nothing at all: a window that said "not yet" is a window
        // an assistive technology decides is broken.
        let built = nothing_yet("2kbrowser");
        let tree = built.tree.as_ref().expect("a tree");
        assert_eq!(tree.root, WINDOW);
        assert!(at(&built, WINDOW).children().is_empty());
        assert_eq!(built.focus, WINDOW);
    }

    #[test]
    fn a_tree_that_lies_about_its_children_does_not_run_off_the_end() {
        // The decoder refuses one of these, so this is belt to that brace —
        // and the brace is on the other side of a process boundary.
        let tree = access::Tree {
            nodes: vec![node(access::Role::Document, 9), node(access::Role::Text, 0)],
            focus: None,
        };
        let built = update(&tree, "t", viewport());
        assert_eq!(at(&built, id_of(0)).children(), &[id_of(1)]);
    }

    /// The whole path, from markup to the nodes a screen reader is handed.
    ///
    /// Everything except the pipe itself: the child builds the tree, the wire
    /// types carry it, and this maps it. What the wire does to it in between is
    /// tested where the wire is, and what this covers is the two ends agreeing
    /// about what a page *is* — which is the part that could quietly be wrong
    /// without anything failing to compile.
    fn from_markup(html: &str) -> TreeUpdate {
        let doc = dom::parse(html);
        let styles = css::cascade::cascade(&doc, &[css::Stylesheet::parse(css::ua::UA_STYLESHEET)]);
        let mut fonts = text::FontStore::new();
        let laid_out = layout::layout(
            &doc,
            &styles,
            &mut fonts,
            &layout::IntrinsicSizes::new(),
            600.0,
            600.0,
        );
        let tree = crate::access::tree_of(&doc, &laid_out, (0.0, 0.0), None);
        update(&tree, "A page", viewport())
    }

    #[test]
    fn a_real_page_becomes_a_tree_a_screen_reader_could_read() {
        let built = from_markup(
            "<html><head><title>A page</title></head><body>\
             <h1>Heading</h1>\
             <p>Some words and <a href=\"/next\">a link</a>.</p>\
             <table><tr><th colspan=\"2\">Both</th></tr>\
             <tr><td>Ada</td><td>1815</td></tr></table>\
             </body></html>",
        );

        // Every node AccessKit was given, by role and label, in the order the
        // page reads.
        let mut seen: Vec<(Role, Option<String>)> = built
            .nodes
            .iter()
            .map(|(_, node)| (node.role(), node.label().map(str::to_owned)))
            .collect();
        seen.remove(0); // the window, which is not on the page

        assert_eq!(
            seen,
            vec![
                (Role::RootWebArea, Some("A page".to_owned())),
                (Role::Heading, Some("Heading".to_owned())),
                (Role::Paragraph, Some("Some words and a link.".to_owned())),
                (Role::Table, None),
                (Role::Row, None),
                (Role::ColumnHeader, Some("Both".to_owned())),
                (Role::Row, None),
                (Role::Cell, Some("Ada".to_owned())),
                (Role::Cell, Some("1815".to_owned())),
            ],
        );

        // And the shape, not only the list: the table holds two rows and the
        // header spans both columns.
        let table = built
            .nodes
            .iter()
            .find(|(_, node)| node.role() == Role::Table)
            .expect("a table");
        assert_eq!(table.1.children().len(), 2);
        let header = built
            .nodes
            .iter()
            .find(|(_, node)| node.role() == Role::ColumnHeader)
            .expect("a header cell");
        assert_eq!(header.1.column_span(), Some(2));
    }

    #[test]
    fn a_form_reads_as_its_labels_rather_than_its_field_names() {
        let built = from_markup(
            "<body><form>\
             <label for=\"e\">Email address</label><input id=\"e\" name=\"txtEmail1\">\
             <input type=\"submit\" value=\"Send\">\
             </form></body>",
        );
        let controls: Vec<(Role, Option<String>)> = built
            .nodes
            .iter()
            .map(|(_, node)| (node.role(), node.label().map(str::to_owned)))
            .filter(|(role, _)| matches!(role, Role::TextInput | Role::Button))
            .collect();
        assert_eq!(
            controls,
            vec![
                (Role::TextInput, Some("Email address".to_owned())),
                (Role::Button, Some("Send".to_owned())),
            ],
        );
    }
}
