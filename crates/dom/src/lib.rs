//! Arena-allocated DOM tree, and the `html5ever` integration that builds it.
//!
//! Nodes live in a flat `Vec` and refer to each other by index (ADR-0007), not
//! through `Rc<RefCell<_>>`. Parent and child links are therefore plain data:
//! cheap to copy, trivially serialisable, and free of reference cycles.
//!
//! `html5ever`'s `TreeSink` takes `&self`, so the arena is wrapped in a
//! `RefCell` *during parsing only*. [`parse`] hands back a plain [`Document`]
//! with no interior mutability left in it.

mod depth;
mod meta_charset;

use std::cell::{Ref, RefCell};

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{BufferQueue, Tokenizer};
use html5ever::tree_builder::{
    ElemName, ElementFlags, NodeOrText, QuirksMode, TreeBuilder, TreeSink,
};
use html5ever::{Attribute, LocalName, Namespace, ParseOpts, QualName, TokenizerResult};

use meta_charset::DefuseMetaCharset;

/// Deepest a document may nest.
///
/// Markup past this is flattened rather than refused (#176). The number is not
/// about markup at all — it is about *stack*, because everything that consumes
/// this tree walks it recursively and the tree's depth is therefore the depth
/// of three recursions in a row.
///
/// Measured, per nesting level, in a release build:
///
/// | walk | stack per level |
/// | --- | --- |
/// | cascade | ~4 KiB |
/// | layout | ~16 KiB |
/// | paint | ~16 KiB |
///
/// A debug build costs four times that. So 512 levels is around 8 MiB of stack
/// in release and 32 MiB in debug — which is why the renderer runs on a stack
/// sized for it rather than on a thread's default 8 MiB, and why that stack and
/// this number have to move together.
///
/// 512 is the number Blink uses for the same job, and it is far past anything
/// real: the era fixture here is 14 deep across 185 elements, and the deepest
/// document in the CSS 2.1 suite is 13.
pub const MAX_DEPTH: usize = 512;

/// Stack a walk over a [`MAX_DEPTH`] document needs.
///
/// Here, beside the cap, because the two are a pair: raising one without the
/// other is how #176 comes back. It is not a fact about the DOM — the walks
/// that spend this stack are the cascade, layout and paint, in three other
/// crates — but it is a fact about what this cap *costs*, and splitting the
/// pair across crates is exactly how the pairing gets lost.
///
/// A debug build spends about 64 KiB per nesting level across those three
/// walks, so the cap costs around 32 MiB. This is that with room to spare.
///
/// Reserved address space rather than memory: pages are committed as they are
/// touched, so a thread that renders an ordinary page costs what it always did.
/// Anything that renders a page a stranger wrote needs a thread with this much
/// — the renderer child, and the fuzzer, which is a renderer with worse taste
/// in documents.
pub const DEPTH_STACK: usize = 64 * 1024 * 1024;

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
    /// What form controls currently hold, where that is no longer what the
    /// markup said (#110).
    ///
    /// Separate from the attributes on purpose, because in HTML they are
    /// separate things: `<input value="...">` is the field's *default*, and
    /// what a reader has typed is a property of the control rather than of the
    /// document. Writing the attribute instead would mean a page that styles
    /// `input[value=""]` changed how it looked as soon as somebody typed —
    /// which is a rule about the markup being answered with a fact about the
    /// session.
    ///
    /// Empty on a freshly parsed document, which is every document until a key
    /// is pressed in one.
    values: std::collections::HashMap<NodeId, String>,
    /// Which controls the reader has turned on or off, where that is no longer
    /// what the markup said.
    ///
    /// The same separation as `values`, for the same reason: `checked` and
    /// `selected` in the markup are *defaults*, and what somebody has ticked is
    /// a property of the control. A page styling `input:checked` or
    /// `option[selected]` would otherwise restyle itself the moment a box was
    /// ticked, which is a rule about the markup answered with a fact about the
    /// session.
    ///
    /// One map covers a checkbox, a radio *and* an `<option>`, because all
    /// three ask the same question — is this one on? — and keying an option by
    /// its own node is what lets a `<select>` share this rather than need a
    /// second record shaped differently.
    chosen: std::collections::HashMap<NodeId, bool>,
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
            values: std::collections::HashMap::new(),
            chosen: std::collections::HashMap::new(),
        }
    }

    /// The document root.
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// What a form control currently holds, if it is not what the markup said.
    ///
    /// `None` means nobody has typed in it, and the caller falls back to the
    /// markup — the `value` attribute, or a `<textarea>`'s content.
    pub fn value_of(&self, id: NodeId) -> Option<&str> {
        self.values.get(&id).map(String::as_str)
    }

    /// Records what a control holds now.
    pub fn set_value(&mut self, id: NodeId, value: impl Into<String>) {
        self.values.insert(id, value.into());
    }

    /// Whether a control is on, if the reader has said either way.
    ///
    /// `None` means nobody has touched it and the caller falls back to the
    /// markup — the `checked` or `selected` attribute. `Some(false)` is a
    /// distinct answer from `None` and the reason this is an `Option` at all:
    /// it is a box that *was* ticked by the markup and has been unticked, which
    /// is the case a form with a pre-ticked "send me email" box turns on.
    pub fn chosen(&self, id: NodeId) -> Option<bool> {
        self.chosen.get(&id).copied()
    }

    /// Records that a control is on or off.
    pub fn set_chosen(&mut self, id: NodeId, on: bool) {
        self.chosen.insert(id, on);
    }

    /// Whether a control is on: a ticked box, a chosen radio, a selected
    /// option.
    ///
    /// The reader's answer first, the markup's only as a default — the same
    /// order [`Document::value_of`] is read in and for the same reason. It
    /// lives here rather than in layout or in the cascade because both of those
    /// ask it, of the same node, and must not be able to disagree: the cascade
    /// decides which option a dropdown *shows* and layout decides which one it
    /// *sends*, and a browser where those two differ is one that submits
    /// something other than what is on screen.
    ///
    /// Which attribute carries the default is the element's business: an
    /// `<option>` is `selected` and everything else is `checked`.
    pub fn is_on(&self, id: NodeId) -> bool {
        if let Some(chosen) = self.chosen(id) {
            return chosen;
        }
        self.element(id).is_some_and(|element| {
            let attribute = match element.local_name() {
                "option" => "selected",
                _ => "checked",
            };
            element.attr(attribute).is_some()
        })
    }

    /// Whether the reader has changed anything in this document at all.
    pub fn is_edited(&self) -> bool {
        !self.values.is_empty() || !self.chosen.is_empty()
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

    /// The element a fragment names, by `id` or by an anchor's `name`.
    ///
    /// Both spellings, because the era's pages use both — `<a name="top">` is
    /// how a page written before `id` was universal marks its own sections,
    /// and plenty of pages still carry them side by side. Document order
    /// decides when two elements claim the same name, which is malformed but
    /// common.
    pub fn fragment_target(&self, name: &str) -> Option<NodeId> {
        if name.is_empty() {
            return None;
        }
        self.descendants(self.root).into_iter().find(|&id| {
            self.element(id).is_some_and(|element| {
                element.id() == Some(name)
                    || (element.local_name() == "a" && element.attr("name") == Some(name))
            })
        })
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

    /// How deep `id` sits below the root.
    ///
    /// Walked rather than stored because the parser moves subtrees around —
    /// the adoption agency algorithm exists to do exactly that — and a stored
    /// depth would be wrong for every descendant of anything it moved. The walk
    /// is bounded by [`MAX_DEPTH`], which is the invariant `no_deeper_than`
    /// keeps, so this is a short climb and not a tree traversal.
    fn depth_of(&self, id: NodeId) -> usize {
        let mut depth = 0;
        let mut at = id;
        while let Some(parent) = self.nodes[at.0].parent {
            depth += 1;
            at = parent;
        }
        depth
    }

    /// `id`, or its nearest ancestor shallow enough to take a child.
    ///
    /// The whole of the depth cap (#176). Everything downstream of the parser
    /// — the cascade, layout, and paint — walks this tree recursively, so the
    /// depth of the tree is the depth of three recursions in a row, and a page
    /// that nests deeply enough overflows the stack of the process holding it.
    /// Bounding it here rather than in each of those three is one rule in one
    /// place, and it is what every browser does: the markup past the cap is
    /// flattened rather than refused, so the page still renders and the content
    /// is still there.
    fn no_deeper_than(&self, id: NodeId, limit: usize) -> NodeId {
        let depth = self.depth_of(id);
        if depth < limit {
            return id;
        }
        let mut at = id;
        for _ in 0..=(depth - limit) {
            match self.nodes[at.0].parent {
                Some(parent) => at = parent,
                None => break,
            }
        }
        at
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
        // The one place nesting is created, and so the one place it is capped
        // (#176). Past the cap the markup is flattened: the element is still
        // created and still in the tree, just as a sibling rather than a child,
        // which is what a browser does with markup this deep and is a great
        // deal better than not rendering the page at all.
        let parent = doc.no_deeper_than(*parent, MAX_DEPTH);
        match child {
            NodeOrText::AppendNode(node) => doc.append(parent, node),
            NodeOrText::AppendText(text) => doc.append_text(parent, &text),
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
///
/// This is `html5ever::parse_document` assembled by hand rather than called,
/// because one extra layer has to go between the tokenizer and the tree
/// builder — see [`meta_charset`] for the bug that needs it and for what to
/// delete when it is fixed upstream. Everything else here is what
/// `parse_document` and `TendrilSink::one` do between them: feed the whole
/// string, run the tokenizer dry, end it, and take the tree.
pub fn parse(html: &str) -> Document {
    let sink = DomSink {
        doc: RefCell::new(Document::new()),
    };
    let options = ParseOpts::default();
    let tree_builder = TreeBuilder::new(sink, options.tree_builder);
    // Two wrappers between the tokenizer and the tree builder, and the order
    // is not arbitrary: the depth cap is outermost, so a tag it refuses never
    // reaches the charset workaround either. Both are working around the same
    // library from the same side.
    let tokenizer = Tokenizer::new(
        depth::CapDepth::new(DefuseMetaCharset(tree_builder)),
        options.tokenizer,
    );

    let input = BufferQueue::default();
    input.push_back(StrTendril::from(html));
    while !matches!(tokenizer.feed(&input), TokenizerResult::Done) {
        // The other result is `Script`, which upstream's own loop also does
        // nothing with. There is no script engine here to hand the element to
        // (ADR-0003), so the tokenizer is simply asked to carry on.
    }
    debug_assert!(input.is_empty(), "the parser stopped with input left");
    tokenizer.end();
    tokenizer.sink.inner.0.sink.finish()
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
    fn defuses_a_meta_charset_that_would_index_past_the_end() {
        // Found by `tests/fuzz`, seed 0xa1, as a mutation of the reference
        // fixtures' own `<meta http-equiv="Content-Type" content="text/html;
        // charset=iso-8859-1">` with the `=` turned into a `"`. That ends the
        // attribute value early and leaves it terminating in `charset`, which
        // walks `html5ever` 0.39.0's cursor one past the end of the string and
        // panics inside the tree builder. See `meta_charset`.
        //
        // Every shape of it, because the whitespace-skipping step is a second
        // way to arrive one past the end and a fix for the first would not
        // necessarily catch it.
        for document in [
            "<meta http-equiv=content-type content=charset>",
            r#"<meta http-equiv="Content-Type" content="text/html; charset">"#,
            "<meta http-equiv=content-type content='text/html; charset  '>",
            "<html><head><meta http-equiv=CONTENT-TYPE content=charsetcharset>",
        ] {
            let doc = parse(document);
            assert!(
                doc.find_element("meta").is_some(),
                "{document:?} lost its meta element"
            );
        }
    }

    #[test]
    fn the_other_four_tags_that_reach_the_same_scan() {
        // `html5ever` runs the charset extraction for the whole "in head" tag
        // group, not for `<meta>` alone. The first version of this fix matched
        // `<meta>` and the next soak found a `<link>` carrying the same
        // attributes — a document with nothing about it that reads as a charset
        // declaration.
        for tag in ["base", "basefont", "bgsound", "link"] {
            let document = format!("<{tag} http-equiv=content-type content='text/html; charset '>");
            let doc = parse(&document);
            assert!(
                doc.find_element(tag).is_some(),
                "{document:?} lost its {tag} element"
            );
        }
    }

    #[test]
    fn a_defused_meta_keeps_the_rest_of_the_document() {
        // The workaround rewrites a token on the way past. If it rewrote the
        // wrong one, or dropped it, the page after it would be the casualty.
        let doc = parse(
            "<html><head><meta http-equiv=content-type content=charset>\
             <title>Still here</title></head><body><p>and so is this</p></body></html>",
        );
        let body = doc.find_element("body").expect("body");
        assert_eq!(doc.text_content(body), "and so is this");
        let title = doc.find_element("title").expect("title");
        assert_eq!(doc.text_content(title), "Still here");
    }

    #[test]
    fn a_charset_attribute_stops_the_content_branch_being_reached() {
        // The tree builder takes `charset` first and only falls through to
        // `content` if there is none, so this is not a trap and must come out
        // byte for byte. A workaround that rewrote it anyway would be changing
        // a document for no reason at all.
        let doc = parse("<meta charset=utf-8 http-equiv=content-type content=charset>");
        let meta = doc.find_element("meta").expect("meta");
        let element = doc.element(meta).expect("meta is an element");
        assert_eq!(element.attr("content"), Some("charset"));
    }

    #[test]
    fn an_ordinary_meta_charset_is_left_alone() {
        // The workaround must not touch a well-formed page, which is nearly
        // all of them. `content` comes back byte for byte.
        let doc = parse(r#"<meta http-equiv="Content-Type" content="text/html; charset=utf-8">"#);
        let meta = doc.find_element("meta").expect("meta");
        let element = doc.element(meta).expect("meta is an element");
        assert_eq!(element.attr("content"), Some("text/html; charset=utf-8"));
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

    #[test]
    fn a_fragment_finds_an_id_or_an_old_pages_named_anchor() {
        let doc = parse(r#"<body><h2 id="here">A</h2><a name="there">B</a></body>"#);

        assert_eq!(
            doc.element(doc.fragment_target("here").expect("by id"))
                .map(ElementData::local_name),
            Some("h2")
        );
        assert_eq!(
            doc.element(doc.fragment_target("there").expect("by name"))
                .map(ElementData::local_name),
            Some("a")
        );
        assert_eq!(doc.fragment_target("elsewhere"), None);
    }

    #[test]
    fn an_empty_fragment_names_nothing() {
        // Not even an element that carries an empty one. `href="#"` means the
        // top of the page, which is the caller's business to know — an
        // element is the wrong answer to give it, and a page with `id=""` on
        // it is malformed rather than an invitation.
        let doc = parse(r#"<body><p id="">x</p><a name="">y</a></body>"#);

        assert_eq!(doc.fragment_target(""), None);
    }

    #[test]
    fn nesting_past_the_cap_is_flattened_rather_than_kept() {
        // #176. Everything downstream of this walks the tree recursively — the
        // cascade, layout, and paint, one after another — so the tree's depth
        // is the depth of three recursions and a deep enough page took the
        // renderer's stack with it.
        let deep = format!(
            "<body>{}<p>bottom</p>{}</body>",
            "<div>".repeat(MAX_DEPTH * 4),
            "</div>".repeat(MAX_DEPTH * 4),
        );
        let doc = parse(&deep);
        let deepest = (0..doc.len())
            .map(NodeId)
            .map(|id| doc.depth_of(id))
            .max()
            .unwrap_or(0);
        assert!(
            deepest <= MAX_DEPTH,
            "{deepest} deep against a cap of {MAX_DEPTH}",
        );
    }

    #[test]
    fn the_content_past_the_cap_is_still_there() {
        // Flattened, not refused. A page that nests too deeply is still a page,
        // and the words at the bottom of it are still what somebody came to
        // read — which is the whole difference between this and giving up.
        let deep = format!(
            "<body>{}<p>bottom</p>{}</body>",
            "<div>".repeat(MAX_DEPTH * 4),
            "</div>".repeat(MAX_DEPTH * 4),
        );
        let doc = parse(&deep);
        assert!(
            doc.text_content(doc.root()).contains("bottom"),
            "the text past the cap was dropped",
        );
    }

    #[test]
    fn an_ordinary_page_is_untouched_by_the_cap() {
        // The cap is far past anything real — the era fixture here is 14 deep
        // and the deepest document in the CSS 2.1 suite is 13 — so this is the
        // assertion that matters most: the common case does not notice.
        let doc = parse(
            "<body><div><table><tr><td><p>Words <b>and <i>more</i></b></p>\
             </td></tr></table></div></body>",
        );
        let deepest = (0..doc.len())
            .map(NodeId)
            .map(|id| doc.depth_of(id))
            .max()
            .unwrap_or(0);
        assert!(deepest < 16, "an ordinary page came out {deepest} deep");
        assert!(doc.text_content(doc.root()).contains("Words and more"));
    }
}
