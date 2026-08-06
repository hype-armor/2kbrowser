//! Arena-allocated DOM tree, and the `html5ever` integration that builds it.
//!
//! Nodes live in a flat `Vec` and refer to each other by index (ADR-0007), not
//! through `Rc<RefCell<_>>`. Parent and child links are therefore plain data:
//! cheap to copy, trivially serialisable, and free of reference cycles.
//!
//! `html5ever`'s `TreeSink` takes `&self`, so the arena is wrapped in a
//! `RefCell` *during parsing only*. [`parse`] hands back a plain [`Document`]
//! with no interior mutability left in it.

use std::cell::{Ref, RefCell};

use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::tree_builder::{ElemName, ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::{Attribute, LocalName, Namespace, ParseOpts, QualName, parse_document};

/// Index of a node within a [`Document`]'s arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub usize);

/// An element's tag name and attributes.
#[derive(Debug, Clone)]
pub struct ElementData {
    /// Qualified tag name, including namespace.
    pub name: QualName,
    /// Attributes in source order.
    pub attrs: Vec<Attribute>,
}

impl ElementData {
    /// The local tag name, lowercased by the parser (`div`, `p`, `a`, …).
    pub fn local_name(&self) -> &str {
        &self.name.local
    }

    /// Value of an attribute, matched on local name.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| &*a.name.local == name)
            .map(|a| &*a.value)
    }

    /// The `id` attribute, if present.
    pub fn id(&self) -> Option<&str> {
        self.attr("id")
    }

    /// Whitespace-separated values of the `class` attribute.
    pub fn classes(&self) -> impl Iterator<Item = &str> {
        self.attr("class")
            .unwrap_or_default()
            .split_ascii_whitespace()
    }
}

/// What a node actually is.
#[derive(Debug, Clone)]
pub enum NodeData {
    /// The document root. Exactly one per [`Document`].
    Document,
    /// A doctype declaration.
    Doctype {
        /// The declared name, e.g. `html`.
        name: String,
    },
    /// An element.
    Element(ElementData),
    /// A run of character data.
    Text(String),
    /// A comment.
    Comment(String),
}

/// A node: its payload plus its position in the tree.
#[derive(Debug, Clone)]
pub struct Node {
    /// Parent, or `None` for the document root and for detached nodes.
    pub parent: Option<NodeId>,
    /// Children in document order.
    pub children: Vec<NodeId>,
    /// The node's payload.
    pub data: NodeData,
}

/// A parsed document.
#[derive(Debug, Clone)]
pub struct Document {
    nodes: Vec<Node>,
    root: NodeId,
    quirks: QuirksMode,
}

impl Document {
    /// An empty document containing only the root node.
    fn new() -> Self {
        Self {
            nodes: vec![Node {
                parent: None,
                children: Vec::new(),
                data: NodeData::Document,
            }],
            root: NodeId(0),
            quirks: QuirksMode::NoQuirks,
        }
    }

    /// The document root.
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Quirks mode, as determined by the parser from the doctype.
    ///
    /// Layout consults this: pages of the target era were frequently authored
    /// against quirks-mode behaviour (ADR-0004).
    pub fn quirks_mode(&self) -> QuirksMode {
        self.quirks
    }

    /// Whether the document is in any flavour of quirks mode.
    ///
    /// Exposed so consumers do not have to depend on `html5ever`'s enum just to
    /// ask the one question they actually have.
    pub fn is_quirks(&self) -> bool {
        self.quirks != QuirksMode::NoQuirks
    }

    /// Total node count, including the root.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the document holds nothing but its root node.
    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    /// Borrow a node.
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0]
    }

    /// The node's ancestors, nearest first.
    pub fn ancestors(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        std::iter::successors(self.nodes[id.0].parent, |&node| self.nodes[node.0].parent)
    }

    /// The nearest `<a href>` at or above `id`.
    ///
    /// A click lands on the text, and the text belongs to whatever `<b>` or
    /// `<font>` happens to wrap it; the href is further up. An `<a>` without an
    /// href is a named destination, not something to follow.
    pub fn enclosing_link(&self, id: NodeId) -> Option<(NodeId, &str)> {
        std::iter::once(id)
            .chain(self.ancestors(id))
            .find_map(|node| {
                let element = self.element(node)?;
                if element.local_name() != "a" {
                    return None;
                }
                let href = element.attr("href")?.trim();
                (!href.is_empty()).then_some((node, href))
            })
    }

    /// Children of a node, in document order.
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.nodes[id.0].children
    }

    /// Element data for a node, or `None` if it is not an element.
    pub fn element(&self, id: NodeId) -> Option<&ElementData> {
        match &self.nodes[id.0].data {
            NodeData::Element(e) => Some(e),
            _ => None,
        }
    }

    /// Text content for a node, or `None` if it is not a text node.
    pub fn text(&self, id: NodeId) -> Option<&str> {
        match &self.nodes[id.0].data {
            NodeData::Text(t) => Some(t),
            _ => None,
        }
    }

    /// Concatenated text of a subtree, skipping comments and doctypes.
    ///
    /// Used by the document-fallback classifier (ADR-0009), which weighs pages
    /// by how much *text* sits under unsupported layout rather than by element
    /// count.
    pub fn text_content(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.collect_text(id, &mut out);
        out
    }

    fn collect_text(&self, id: NodeId, out: &mut String) {
        match &self.nodes[id.0].data {
            NodeData::Text(t) => out.push_str(t),
            NodeData::Element(_) | NodeData::Document => {
                for &child in &self.nodes[id.0].children {
                    self.collect_text(child, out);
                }
            }
            NodeData::Doctype { .. } | NodeData::Comment(_) => {}
        }
    }

    /// Depth-first iterator over every node id, in document order.
    pub fn descendants(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(current) = stack.pop() {
            out.push(current);
            stack.extend(self.nodes[current.0].children.iter().rev().copied());
        }
        out
    }

    /// First element in document order whose local name matches.
    pub fn find_element(&self, local_name: &str) -> Option<NodeId> {
        self.descendants(self.root).into_iter().find(|&id| {
            self.element(id)
                .is_some_and(|e| e.local_name() == local_name)
        })
    }

    fn push(&mut self, data: NodeData) -> NodeId {
        self.nodes.push(Node {
            parent: None,
            children: Vec::new(),
            data,
        });
        NodeId(self.nodes.len() - 1)
    }

    fn detach(&mut self, id: NodeId) {
        if let Some(parent) = self.nodes[id.0].parent.take() {
            self.nodes[parent.0].children.retain(|&c| c != id);
        }
    }

    fn append(&mut self, parent: NodeId, child: NodeId) {
        self.detach(child);
        self.nodes[child.0].parent = Some(parent);
        self.nodes[parent.0].children.push(child);
    }

    /// Appends text to `parent`, merging into a trailing text node when there
    /// is one. The parser emits character data in chunks, and leaving those
    /// unmerged would fragment every paragraph into many text nodes.
    fn append_text(&mut self, parent: NodeId, text: &str) {
        if let Some(&last) = self.nodes[parent.0].children.last()
            && let NodeData::Text(existing) = &mut self.nodes[last.0].data
        {
            existing.push_str(text);
            return;
        }
        let node = self.push(NodeData::Text(text.to_owned()));
        self.append(parent, node);
    }

    fn insert_before(&mut self, sibling: NodeId, child: NodeId) {
        let Some(parent) = self.nodes[sibling.0].parent else {
            return;
        };
        self.detach(child);
        let index = self.nodes[parent.0]
            .children
            .iter()
            .position(|&c| c == sibling)
            .unwrap_or(self.nodes[parent.0].children.len());
        self.nodes[child.0].parent = Some(parent);
        self.nodes[parent.0].children.insert(index, child);
    }

    fn insert_text_before(&mut self, sibling: NodeId, text: &str) {
        let node = self.push(NodeData::Text(text.to_owned()));
        self.insert_before(sibling, node);
    }
}

/// Builds a [`Document`] from `html5ever` callbacks.
struct DomSink {
    doc: RefCell<Document>,
}

impl TreeSink for DomSink {
    type Handle = NodeId;
    type Output = Document;
    type ElemName<'a> = ElemNameRef<'a>;

    fn finish(self) -> Document {
        self.doc.into_inner()
    }

    // Parse errors are expected, not exceptional: the HTML parsing algorithm is
    // an error-recovery specification (ADR-0007), and the era's markup trips it
    // constantly. Recovery is the parser's job and it has already happened.
    fn parse_error(&self, _msg: std::borrow::Cow<'static, str>) {}

    fn get_document(&self) -> NodeId {
        self.doc.borrow().root
    }

    fn elem_name<'a>(&'a self, target: &'a NodeId) -> ElemNameRef<'a> {
        let id = *target;
        ElemNameRef {
            name: Ref::map(self.doc.borrow(), |doc| match &doc.nodes[id.0].data {
                NodeData::Element(e) => &e.name,
                _ => panic!("elem_name on a non-element node"),
            }),
        }
    }

    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, _: ElementFlags) -> NodeId {
        self.doc
            .borrow_mut()
            .push(NodeData::Element(ElementData { name, attrs }))
    }

    fn create_comment(&self, text: StrTendril) -> NodeId {
        self.doc
            .borrow_mut()
            .push(NodeData::Comment(text.to_string()))
    }

    fn create_pi(&self, target: StrTendril, data: StrTendril) -> NodeId {
        self.doc
            .borrow_mut()
            .push(NodeData::Comment(format!("{target} {data}")))
    }

    fn append(&self, parent: &NodeId, child: NodeOrText<NodeId>) {
        let mut doc = self.doc.borrow_mut();
        match child {
            NodeOrText::AppendNode(node) => doc.append(*parent, node),
            NodeOrText::AppendText(text) => doc.append_text(*parent, &text),
        }
    }

    fn append_based_on_parent_node(
        &self,
        element: &NodeId,
        prev_element: &NodeId,
        child: NodeOrText<NodeId>,
    ) {
        let has_parent = self.doc.borrow().nodes[element.0].parent.is_some();
        if has_parent {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_doctype_to_document(&self, name: StrTendril, _: StrTendril, _: StrTendril) {
        let mut doc = self.doc.borrow_mut();
        let node = doc.push(NodeData::Doctype {
            name: name.to_string(),
        });
        let root = doc.root;
        doc.append(root, node);
    }

    fn get_template_contents(&self, target: &NodeId) -> NodeId {
        *target
    }

    fn same_node(&self, x: &NodeId, y: &NodeId) -> bool {
        x == y
    }

    fn set_quirks_mode(&self, mode: QuirksMode) {
        self.doc.borrow_mut().quirks = mode;
    }

    fn append_before_sibling(&self, sibling: &NodeId, new_node: NodeOrText<NodeId>) {
        let mut doc = self.doc.borrow_mut();
        match new_node {
            NodeOrText::AppendNode(node) => doc.insert_before(*sibling, node),
            NodeOrText::AppendText(text) => doc.insert_text_before(*sibling, &text),
        }
    }

    fn add_attrs_if_missing(&self, target: &NodeId, attrs: Vec<Attribute>) {
        let mut doc = self.doc.borrow_mut();
        let NodeData::Element(element) = &mut doc.nodes[target.0].data else {
            return;
        };
        for attr in attrs {
            if !element.attrs.iter().any(|a| a.name == attr.name) {
                element.attrs.push(attr);
            }
        }
    }

    fn remove_from_parent(&self, target: &NodeId) {
        self.doc.borrow_mut().detach(*target);
    }

    fn reparent_children(&self, node: &NodeId, new_parent: &NodeId) {
        let mut doc = self.doc.borrow_mut();
        let moved = std::mem::take(&mut doc.nodes[node.0].children);
        for child in moved {
            doc.nodes[child.0].parent = Some(*new_parent);
            doc.nodes[new_parent.0].children.push(child);
        }
    }
}

/// Borrowed element name, as required by `TreeSink::elem_name`.
pub struct ElemNameRef<'a> {
    name: Ref<'a, QualName>,
}

impl std::fmt::Debug for ElemNameRef<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&*self.name, f)
    }
}

impl ElemName for ElemNameRef<'_> {
    fn ns(&self) -> &Namespace {
        &self.name.ns
    }

    fn local_name(&self) -> &LocalName {
        &self.name.local
    }
}

/// Parses an HTML document.
///
/// Never fails: the HTML parsing algorithm defines recovery for every input,
/// so malformed markup yields a tree rather than an error (ADR-0007).
pub fn parse(html: &str) -> Document {
    let sink = DomSink {
        doc: RefCell::new(Document::new()),
    };
    parse_document(sink, ParseOpts::default()).one(html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_minimal_document() {
        let doc = parse("<!doctype html><html><body><p>hello</p></body></html>");
        let body = doc.find_element("body").expect("body");
        assert_eq!(doc.text_content(body), "hello");
        assert_eq!(doc.quirks_mode(), QuirksMode::NoQuirks);
    }

    #[test]
    fn synthesises_missing_structure() {
        // No html, head, or body tags: the parser must invent them.
        let doc = parse("hello");
        assert!(doc.find_element("html").is_some());
        assert!(doc.find_element("head").is_some());
        let body = doc.find_element("body").expect("body");
        assert_eq!(doc.text_content(body), "hello");
    }

    #[test]
    fn recovers_from_bad_markup() {
        // Misnested tags are the parser's whole reason for existing.
        let doc = parse("<p>one<b>two<p>three</b>");
        let body = doc.find_element("body").expect("body");
        assert_eq!(doc.text_content(body), "onetwothree");
    }

    #[test]
    fn a_legacy_doctype_selects_quirks_mode() {
        let doc = parse("<html><body>x</body></html>");
        assert_eq!(doc.quirks_mode(), QuirksMode::Quirks);
    }

    #[test]
    fn merges_adjacent_character_data() {
        // Entities make the tokenizer emit text in several chunks; without
        // merging, every paragraph would fragment into many text nodes.
        let doc = parse("<p>a&amp;b&amp;c</p>");
        let p = doc.find_element("p").expect("p");
        assert_eq!(doc.children(p).len(), 1);
        assert_eq!(doc.text_content(p), "a&b&c");
    }

    #[test]
    fn reads_attributes_and_classes() {
        let doc = parse(r#"<div id="main" class="a  b">x</div>"#);
        let div = doc.find_element("div").expect("div");
        let element = doc.element(div).expect("element");
        assert_eq!(element.id(), Some("main"));
        assert_eq!(element.classes().collect::<Vec<_>>(), ["a", "b"]);
    }
}
