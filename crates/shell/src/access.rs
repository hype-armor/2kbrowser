//! Reading a page the way a screen reader would (ADR-0019, #9).
//!
//! The semantic tree is the DOM plus the box tree, and this is where the two
//! are put back together: the DOM says what a thing *is* and what it says, the
//! box tree says where it ended up. Neither crosses the boundary — that
//! restraint is the point of ADR-0012 — so this runs in the child and only its
//! answer travels.
//!
//! A document renderer with no scripting is unusually well suited to this. The
//! semantic tree is not fighting a framework that rebuilt the DOM three times
//! before paint: headings are headings, links are links, and ADR-0003 means
//! none of it moves after load. That removes the entire category of
//! tree-update bugs that dominates accessibility work in scripted browsers,
//! and it is why the tree is built once per render rather than incrementally.
//!
//! What is deliberately *not* here is any judgement about what a reader wants
//! to hear. This reports what the page is; the platform layer decides how to
//! say it.

use std::collections::HashMap;

use dom::{Document, NodeId};
use layout::{LayoutBox, Rect};
use sandbox::access::{MAX_DEPTH, MAX_NODES, Node, Role, Tree};

/// Deepest this walk will recurse, whether or not it is emitting anything.
///
/// Larger than [`MAX_DEPTH`] because most of a real page's nesting is
/// presentational and flattens away — the era's markup wraps three semantic
/// levels in thirty `<div>`s and `<table>`s — so tying the two together would
/// stop describing the bottom of ordinary pages. Small enough that the frames
/// it costs are nothing beside what the engine below already spends per level.
const MAX_WALK: usize = 256;

/// Where a node that was never laid out is said to be.
const NOWHERE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    width: 0.0,
    height: 0.0,
};

/// Builds the tree for one document.
///
/// `offset` moves the frame's own coordinates onto the canvas, so a frameset's
/// documents land where a reader would point at them.
pub fn tree_of(
    doc: &Document,
    layout: &layout::Layout,
    offset: (f32, f32),
    focused: Option<NodeId>,
) -> Tree {
    let mut rects = HashMap::new();
    collect_rects(&layout.root, offset, MAX_WALK, &mut rects);

    let mut nodes = Vec::new();
    let root = doc.find_element("body").unwrap_or_else(|| doc.root());
    nodes.push(Node {
        name: doc
            .find_element("title")
            .map(|t| doc.text_content(t))
            .unwrap_or_default(),
        ..Node::new(Role::Document, whole_page(layout, offset))
    });
    let mut source = Vec::new();
    source.push(None);
    let children = walk(doc, &rects, root, 1, MAX_WALK, &mut nodes, &mut source);
    nodes[0].children = children;
    // #151 gave the child a keyboard focus, and naming it here is what lets a
    // screen reader follow the keyboard rather than work it out from geometry.
    // As an index into this tree, because the element it came from is a
    // `NodeId` in an arena on this side of the boundary and means nothing on
    // the other.
    let focus = focused
        .and_then(|node| source.iter().position(|&from| from == Some(node)))
        .map(|at| at as u32);
    Tree { nodes, focus }
}

/// One tree for a canvas that may hold several documents.
///
/// A frameset is several documents on one page, and a screen reader is offered
/// one page. A single document is handed back as it was built — it already has
/// a `Document` at its root — and only a frameset grows the extra root that
/// says "these are all on this page", because inventing one for the common case
/// would put a node in every tree that describes nothing.
pub fn tree_of_frames<'a>(
    frames: impl IntoIterator<Item = (&'a Document, &'a layout::Layout, (f32, f32))>,
    focused: Option<NodeId>,
) -> Tree {
    let mut parts: Vec<Tree> = frames
        .into_iter()
        .map(|(doc, layout, offset)| tree_of(doc, layout, offset, focused))
        .filter(|tree| !tree.nodes.is_empty())
        .collect();
    match parts.len() {
        0 => Tree::default(),
        1 => parts.remove(0),
        _ => {
            let mut nodes =
                Vec::with_capacity(parts.iter().map(|t| t.nodes.len()).sum::<usize>() + 1);
            let mut root = Node::new(Role::Document, NOWHERE);
            root.children = parts.len() as u32;
            nodes.push(root);
            // Whichever frame found the focus, moved down by the root this
            // adds and by the frames emitted before it.
            let mut focus = None;
            for part in parts {
                if let Some(at) = part.focus {
                    focus = Some(at + nodes.len() as u32);
                }
                nodes.extend(part.nodes);
            }
            // The extra root is a level of its own, so a frame already at the
            // bound would push the tree past it. Refused here rather than on
            // the wire, where it would be a page described not at all.
            if nodes.len() > MAX_NODES {
                nodes.truncate(1);
                nodes[0].children = 0;
                focus = None;
            }
            Tree { nodes, focus }
        }
    }
}

/// The canvas the document covers, which is what the document node stands for.
fn whole_page(layout: &layout::Layout, offset: (f32, f32)) -> Rect {
    Rect {
        x: offset.0,
        y: offset.1,
        width: layout.root.rect.width,
        height: layout.height,
    }
}

/// Where each element ended up, in canvas coordinates.
///
/// The *first* box for a node and not the last: an inline element broken across
/// three lines has three boxes, and a screen reader asking where a link is
/// wants the place a pointer would land on it rather than wherever its last
/// fragment happened to stop.
fn collect_rects(box_: &LayoutBox, at: (f32, f32), budget: usize, out: &mut HashMap<NodeId, Rect>) {
    if budget == 0 {
        return;
    }
    let x = at.0 + box_.rect.x;
    let y = at.1 + box_.rect.y;
    if let Some(node) = box_.node {
        out.entry(node).or_insert(Rect {
            x,
            y,
            width: box_.rect.width,
            height: box_.rect.height,
        });
    }
    for child in &box_.children {
        collect_rects(child, (x, y), budget - 1, out);
    }
}

/// Emits `node`'s children, returning how many were emitted at this level.
///
/// Two limits, not one, and conflating them is a bug this had before the test
/// for it did. `depth` is how deep the *emitted* tree is, which is what the
/// wire bounds; `budget` is how deep this function is willing to recurse, which
/// is what the stack bounds. They come apart because flattening descends
/// without emitting: a page wrapped in three hundred `<div>`s has a tree one
/// level deep and a walk three hundred frames deep, and bounding only the first
/// leaves the second free to run out of stack.
///
/// Both are the child's own protection. The wire bound exists because the
/// *parent* walks the result recursively, but a document deep enough to
/// overflow a stack there would overflow one here first — and the child is
/// where a hostile page actually is. (The engine below this has the same
/// vector and no such bound; filed as #176.)
fn walk(
    doc: &Document,
    rects: &HashMap<NodeId, Rect>,
    node: NodeId,
    depth: usize,
    budget: usize,
    out: &mut Vec<Node>,
    // Which element each emitted node came from, so the focus can be found
    // afterwards. Kept beside the nodes rather than on them: a `NodeId` is an
    // index into this process's arena and has no meaning on the wire.
    source: &mut Vec<Option<NodeId>>,
) -> u32 {
    if depth >= MAX_DEPTH || budget == 0 {
        return 0;
    }
    let mut emitted = 0u32;
    for &child in doc.children(node) {
        if out.len() >= MAX_NODES {
            break;
        }
        let Some(element) = doc.element(child) else {
            // Bare text inside a box that does not speak for its own contents —
            // the words of `<li>Apples</li>`, or the sentence a `<td>` holds
            // beside a link. Without a node of its own it is simply lost, which
            // is the quietest way an accessibility tree can be wrong.
            if let Some(text) = doc.text(child)
                && !text.trim().is_empty()
            {
                let mut said = Node::new(Role::Text, rects.get(&child).copied().unwrap_or(NOWHERE));
                said.name = collapse(text);
                out.push(said);
                source.push(None);
                emitted += 1;
            }
            continue;
        };
        let Some(role) = role_of(element) else {
            // Something with no role of its own — a `<div>`, a `<span>`, a
            // `<font>`. It is not a box a reader should have to step through,
            // so its children are emitted in its place rather than under it.
            // Flattening rather than grouping is what keeps a page wrapped in
            // six nested `<div>`s from reading as six nested groups.
            emitted += walk(doc, rects, child, depth, budget - 1, out, source);
            continue;
        };
        // A node the layout never placed — `display: none`, or an element
        // outside the box tree — gets an empty rectangle rather than being
        // dropped. It is still on the page semantically, and a reader stepping
        // through headings should not lose one because it was hidden by a
        // stylesheet the author wrote for print.
        let rect = rects.get(&child).copied().unwrap_or(NOWHERE);
        // Whether this node's own text is its label, so its contents are not
        // emitted as well. A link reads as one thing, and saying what is inside
        // it too would have a reader hear the same words twice.
        //
        // For a list item or a table cell the answer depends on the page rather
        // than on the role: `<li>Apples</li>` is a label, and `<li><a>Next</a>
        // and more</li>` is a box holding a link. Asking whether anything
        // inside would become a node is what tells them apart.
        let flat = role.always_speaks_for_its_contents()
            || (role.speaks_when_it_holds_nothing_else() && !holds_an_element_node(doc, child));
        let at = out.len();
        out.push(describe(doc, child, element, role, rect, flat));
        source.push(Some(child));
        let children = if flat {
            0
        } else {
            walk(doc, rects, child, depth + 1, budget - 1, out, source)
        };
        out[at].children = children;
        emitted += 1;
    }
    emitted
}

/// Fills in what a node says, beyond what its role already said.
fn describe(
    doc: &Document,
    node: NodeId,
    element: &dom::ElementData,
    role: Role,
    rect: Rect,
    flat: bool,
) -> Node {
    let mut out = Node::new(role, rect);
    out.name = match role {
        // An image is named by its alternative text, and by nothing else. A
        // filename is not a description — reading one out is worse than
        // silence, because it sounds like information.
        Role::Image => element.attr("alt").unwrap_or_default().to_owned(),
        Role::TextField | Role::CheckBox | Role::RadioButton | Role::ComboBox => element
            .attr("aria-label")
            .or_else(|| element.attr("title"))
            .or_else(|| element.attr("name"))
            .unwrap_or_default()
            .to_owned(),
        Role::Separator => String::new(),
        // A table is named by its caption, if it has one. Not by its contents:
        // a table whose name is every word in it is a page read twice, once as
        // the table's name and again as its cells.
        Role::Table => caption_of(doc, node),
        // And nor is anything else that holds other nodes. A box's name has to
        // come from something that *names* it; where nothing does, silence is
        // right, and the contents speak for themselves as their own nodes.
        _ if !flat => element
            .attr("aria-label")
            .or_else(|| element.attr("title"))
            .unwrap_or_default()
            .to_owned(),
        _ => collapse(&doc.text_content(node)),
    };
    out.value = match role {
        Role::Link => element.attr("href").map(str::to_owned),
        // What the reader typed, and failing that what the markup put there.
        // `value_of` holds only edits, so a field nobody has touched answers
        // `None` — and reading an untouched field as empty is describing a form
        // that has already been filled in as one that has not.
        Role::TextField => Some(
            doc.value_of(node)
                .or_else(|| element.attr("value"))
                .unwrap_or_default()
                .to_owned(),
        ),
        Role::ComboBox => chosen_option(doc, node),
        _ => None,
    };
    out.on = matches!(role, Role::CheckBox | Role::RadioButton) && doc.is_on(node);
    out.level = if role == Role::Heading {
        heading_level(element.local_name())
    } else {
        0
    };
    // A name that is only whitespace is no name. Left as the empty string so
    // the parent has one thing to test rather than two.
    if out.name.trim().is_empty() {
        out.name.clear();
    }
    out
}

/// Which option a `<select>` is on, by the same rule the renderer draws.
fn chosen_option(doc: &Document, node: NodeId) -> Option<String> {
    let options: Vec<NodeId> = doc
        .descendants(node)
        .into_iter()
        .filter(|&id| {
            doc.element(id)
                .is_some_and(|e| e.local_name().eq_ignore_ascii_case("option"))
        })
        .collect();
    let on = options
        .iter()
        .find(|&&id| doc.is_on(id))
        .or_else(|| options.first())?;
    Some(doc.text_content(*on))
}

fn heading_level(name: &str) -> u8 {
    match name {
        "h1" => 1,
        "h2" => 2,
        "h3" => 3,
        "h4" => 4,
        "h5" => 5,
        _ => 6,
    }
}

/// Whether a role's own text is its label, so its children are not nodes too.
trait SpeaksForItself {
    /// True whatever the page puts inside it.
    fn always_speaks_for_its_contents(self) -> bool;
    /// True only where nothing inside would become a node of its own.
    fn speaks_when_it_holds_nothing_else(self) -> bool;
}

impl SpeaksForItself for Role {
    fn always_speaks_for_its_contents(self) -> bool {
        matches!(
            self,
            Role::Heading
                | Role::Link
                | Role::Button
                | Role::Paragraph
                | Role::Text
                | Role::Image
                | Role::Separator
                | Role::TextField
                | Role::CheckBox
                | Role::RadioButton
                | Role::ComboBox
        )
    }

    fn speaks_when_it_holds_nothing_else(self) -> bool {
        matches!(self, Role::ListItem | Role::Cell | Role::HeaderCell)
    }
}

/// Whether anything inside `node` is an element that would become a node.
///
/// Text does not count, and that is the distinction the whole rule turns on.
/// `<td>Ada</td>` holds only words, so the cell *is* the word Ada and reads as
/// one thing. `<td>See <a href="/x">this</a></td>` holds a link, so the cell is
/// a box with two things in it — and flattening it to one label would lose
/// which part of the sentence a reader could follow.
fn holds_an_element_node(doc: &Document, node: NodeId) -> bool {
    doc.descendants(node)
        .into_iter()
        .any(|id| id != node && doc.element(id).is_some_and(|e| role_of(e).is_some()))
}

/// A table's `<caption>`, which is the one thing that names a table.
fn caption_of(doc: &Document, node: NodeId) -> String {
    doc.children(node)
        .iter()
        .find(|&&id| {
            doc.element(id)
                .is_some_and(|e| e.local_name().eq_ignore_ascii_case("caption"))
        })
        .map(|&id| collapse(&doc.text_content(id)))
        .unwrap_or_default()
}

/// Whitespace collapsed the way the page renders it.
///
/// What a reader hears should be what a reader sees, and the source's line
/// breaks and indentation are neither.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What an element is, where it is anything.
///
/// `None` is not a failure: most elements on a page are presentational, and a
/// tree that made a node for every `<div>` would be a tree nobody could move
/// through. The mapping is by tag, because that is what this engine has —
/// ADR-0003 means no script ever set a role, and CSS 2.1 has none to set.
fn role_of(element: &dom::ElementData) -> Option<Role> {
    let name = element.local_name();
    Some(match name {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Role::Heading,
        "p" => Role::Paragraph,
        // Only a link that goes somewhere. An `<a name="top">` is an anchor,
        // which is a place rather than a control, and offering it as a link
        // would give a reader something that does nothing when followed.
        "a" => {
            if element.attr("href").is_some() {
                Role::Link
            } else {
                return None;
            }
        }
        "button" => Role::Button,
        "textarea" => Role::TextField,
        "input" => return input_role(element),
        "select" => Role::ComboBox,
        "ul" | "ol" | "dl" | "menu" | "dir" => Role::List,
        "li" | "dt" | "dd" => Role::ListItem,
        "table" => Role::Table,
        "tr" => Role::Row,
        "td" => Role::Cell,
        "th" => Role::HeaderCell,
        "img" => Role::Image,
        "hr" => Role::Separator,
        // A quotation and a preformatted block are boxes a reader steps
        // through, which a `<div>` is not.
        "blockquote" | "pre" => Role::Group,
        _ => return None,
    })
}

/// What an `<input>` is, which is entirely its `type`.
fn input_role(element: &dom::ElementData) -> Option<Role> {
    let kind = element.attr("type").unwrap_or("text").to_ascii_lowercase();
    Some(match kind.as_str() {
        "checkbox" => Role::CheckBox,
        "radio" => Role::RadioButton,
        "submit" | "button" | "reset" | "image" => Role::Button,
        // Not a control at all: it carries a value the reader never sees and
        // cannot change, and announcing one would be describing the page's
        // plumbing rather than the page.
        "hidden" => return None,
        _ => Role::TextField,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tree for a document, laid out at 600 wide.
    fn tree(html: &str) -> Tree {
        let doc = dom::parse(html);
        let styles = css::cascade::cascade(&doc, &[css::Stylesheet::parse(css::ua::UA_STYLESHEET)]);
        let mut fonts = text::FontStore::new();
        let out = layout::layout(
            &doc,
            &styles,
            &mut fonts,
            &layout::IntrinsicSizes::new(),
            600.0,
            600.0,
        );
        tree_of(&doc, &out, (0.0, 0.0), None)
    }

    /// Every node's role and name, the document first.
    fn shape(tree: &Tree) -> Vec<(Role, String)> {
        tree.nodes
            .iter()
            .map(|node| (node.role, node.name.clone()))
            .collect()
    }

    #[test]
    fn a_page_reads_as_what_it_is() {
        let tree = tree(
            "<html><head><title>A page</title></head><body>\
             <h2>Chapter one</h2><p>Some words.</p>\
             <a href=\"/next\">Next</a></body></html>",
        );
        assert_eq!(
            shape(&tree),
            vec![
                (Role::Document, "A page".to_owned()),
                (Role::Heading, "Chapter one".to_owned()),
                (Role::Paragraph, "Some words.".to_owned()),
                (Role::Link, "Next".to_owned()),
            ],
        );
        assert_eq!(tree.nodes[0].children, 3);
        assert_eq!(tree.nodes[1].level, 2, "a heading says which it is");
        assert_eq!(tree.nodes[3].value.as_deref(), Some("/next"));
    }

    #[test]
    fn presentational_wrappers_do_not_become_nodes() {
        // The era's pages are built out of nested `<div>`s and `<table>`s used
        // for layout. A tree with a group per wrapper is a tree nobody can move
        // through, so a wrapper with no role of its own hands its children up.
        let tree = tree("<body><div><div><span><h1>Deep</h1></span></div></div></body>");
        assert_eq!(
            shape(&tree),
            vec![
                (Role::Document, String::new()),
                (Role::Heading, "Deep".to_owned()),
            ],
        );
        assert_eq!(tree.nodes[0].children, 1, "the heading is the body's child");
    }

    #[test]
    fn a_link_reads_once_rather_than_twice() {
        // Its text is its label, so emitting what is inside it as well would
        // have a reader hear the same words twice.
        let tree = tree("<body><a href=\"/x\">Go <b>now</b></a></body>");
        assert_eq!(
            shape(&tree),
            vec![
                (Role::Document, String::new()),
                (Role::Link, "Go now".to_owned()),
            ],
        );
    }

    #[test]
    fn an_anchor_that_goes_nowhere_is_not_a_link() {
        // `<a name="top">` is a place, not a control. Offering it as a link
        // gives a reader something that does nothing when followed.
        let tree = tree("<body><a name=\"top\"></a><a href=\"#top\">Top</a></body>");
        assert_eq!(shape(&tree).len(), 2);
        assert_eq!(shape(&tree)[1].0, Role::Link);
    }

    #[test]
    fn an_image_is_named_by_its_alt_and_never_by_its_file() {
        // A filename read out sounds like information and is not. Silence is
        // the honest answer for an image nobody described.
        let tree = tree("<body><img src=\"cat.gif\" alt=\"A cat\"><img src=\"spacer.gif\"></body>");
        assert_eq!(
            shape(&tree),
            vec![
                (Role::Document, String::new()),
                (Role::Image, "A cat".to_owned()),
                (Role::Image, String::new()),
            ],
        );
    }

    #[test]
    fn a_control_says_what_it_is_and_what_it_holds() {
        let tree = tree(
            "<body><form><input name=\"who\" value=\"Ada\">\
             <input type=\"checkbox\" name=\"ok\" checked>\
             <input type=\"hidden\" name=\"token\" value=\"secret\">\
             <input type=\"submit\" value=\"Send\"></form></body>",
        );
        let roles: Vec<Role> = tree.nodes.iter().map(|n| n.role).collect();
        assert_eq!(
            roles,
            vec![
                Role::Document,
                Role::TextField,
                Role::CheckBox,
                Role::Button,
            ],
            "a hidden input is plumbing, not a control",
        );
        assert_eq!(tree.nodes[1].value.as_deref(), Some("Ada"));
        assert!(tree.nodes[2].on, "a ticked box says so");
    }

    #[test]
    fn a_table_keeps_its_shape() {
        let tree = tree("<body><table><tr><th>Name</th><td>Ada</td></tr></table></body>");
        assert_eq!(
            shape(&tree),
            vec![
                (Role::Document, String::new()),
                (Role::Table, String::new()),
                (Role::Row, String::new()),
                (Role::HeaderCell, "Name".to_owned()),
                (Role::Cell, "Ada".to_owned()),
            ],
        );
        assert_eq!(tree.nodes[1].children, 1, "the table holds one row");
        assert_eq!(tree.nodes[2].children, 2, "the row holds two cells");
    }

    #[test]
    fn a_node_says_where_it_is() {
        // Geometry is the half of this that only the box tree knows, and a
        // screen reader that could not point at a thing would be describing a
        // page nobody could touch.
        let tree = tree("<body><h1>Title</h1></body>");
        let heading = &tree.nodes[1];
        assert!(
            heading.rect.width > 0.0 && heading.rect.height > 0.0,
            "the heading has no box: {:?}",
            heading.rect,
        );
    }

    #[test]
    fn a_wrapper_pile_flattens_rather_than_running_out_of_stack() {
        // The two limits coming apart. These wrappers have no role, so the
        // emitted tree is one level deep however many there are — but the walk
        // descends through every one of them, and bounding only the emitted
        // depth left the recursion free to run out of stack. The first version
        // of this did exactly that, and this test is what found it.
        //
        // On a stack of its own, because the *engine* runs out first: #176 has
        // `layout` overflowing at around 48 levels in a debug test thread's
        // 2 MiB. That is a real bug and not this one, so this test buys the
        // room to get past it and test what it came to test. If #176 is fixed
        // the `stack_size` can go; until then, removing it would only prove
        // that #176 is still open.
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(|| {
                let wrappers = 200;
                let deep = format!(
                    "<body>{}<h1>bottom</h1>{}</body>",
                    "<div>".repeat(wrappers),
                    "</div>".repeat(wrappers),
                );
                let tree = tree(&deep);
                assert_eq!(
                    shape(&tree),
                    vec![
                        (Role::Document, String::new()),
                        (Role::Heading, "bottom".to_owned()),
                    ],
                );
            })
            .expect("a thread")
            .join()
            .expect("it did not fall over");
    }

    #[test]
    fn the_tree_is_within_every_bound_the_wire_will_check() {
        // The decoder refuses a tree that breaks a bound, so a builder that
        // could produce one would make a page silently undescribable. Asserted
        // by round-tripping rather than by re-checking the bounds here, because
        // the decoder is the thing whose opinion matters.
        let mut body = String::from("<body>");
        for at in 0..40 {
            body.push_str(&format!("<h2>Heading {at}</h2><p>Words {at}.</p>"));
        }
        body.push_str("</body>");
        let tree = tree(&body);
        assert_eq!(tree.nodes.len(), 81, "a document and forty pairs");

        let mut writer = sandbox::wire::Writer::new();
        tree.write(&mut writer);
        let bytes = writer.finish();
        let mut reader = sandbox::wire::Reader::new(&bytes);
        assert_eq!(Tree::read(&mut reader), Ok(tree));
    }

    #[test]
    fn a_cell_holding_a_link_is_a_box_and_not_a_label() {
        // The rule that decides this is about the page rather than the role. A
        // cell of plain words reads as those words; one holding a link is a box
        // with two things in it, and flattening it would lose which part a
        // reader could follow.
        let tree = tree(
            "<body><table><tr><td>Ada</td><td>See <a href=\"/x\">this</a></td>\
             </tr></table></body>",
        );
        assert_eq!(
            shape(&tree),
            vec![
                (Role::Document, String::new()),
                (Role::Table, String::new()),
                (Role::Row, String::new()),
                (Role::Cell, "Ada".to_owned()),
                (Role::Cell, String::new()),
                (Role::Text, "See".to_owned()),
                (Role::Link, "this".to_owned()),
            ],
        );
    }

    #[test]
    fn a_table_is_named_by_its_caption_and_by_nothing_else() {
        let tree =
            tree("<body><table><caption>Staff</caption><tr><td>Ada</td></tr></table></body>");
        assert_eq!(tree.nodes[1].role, Role::Table);
        assert_eq!(tree.nodes[1].name, "Staff");
    }

    #[test]
    fn the_keyboard_travels_with_the_tree() {
        // #151 gave the child a focus; naming it here is what lets a screen
        // reader follow the keyboard rather than guess from geometry. As an
        // index into this tree, because the element it came from is an arena
        // index on this side of the boundary and means nothing on the other.
        let doc = dom::parse("<body><a href=\"/a\">One</a><a href=\"/b\">Two</a></body>");
        let styles = css::cascade::cascade(&doc, &[css::Stylesheet::parse(css::ua::UA_STYLESHEET)]);
        let mut fonts = text::FontStore::new();
        let out = layout::layout(
            &doc,
            &styles,
            &mut fonts,
            &layout::IntrinsicSizes::new(),
            600.0,
            600.0,
        );
        let second = doc
            .descendants(doc.root())
            .into_iter()
            .filter(|&id| doc.element(id).is_some_and(|e| e.local_name() == "a"))
            .nth(1)
            .expect("two links");
        let tree = tree_of(&doc, &out, (0.0, 0.0), Some(second));
        assert_eq!(tree.focus, Some(2), "the second link is the third node");
        assert_eq!(tree_of(&doc, &out, (0.0, 0.0), None).focus, None);
    }
}
