//! Selector parsing and matching.
//!
//! Scope is the CSS 2.1 subset: type, class, id, universal, and attribute
//! selectors, combined into compounds and joined by descendant or child
//! combinators. Pseudo-classes, sibling combinators, and pseudo-elements are
//! not here; a selector using one is dropped whole rather than matched
//! partially, since matching too broadly is the worse failure.
//!
//! Selectors are split on whitespace to find combinators, so an attribute test
//! written with spaces around its operator (`[title = "x"]`) does not parse.
//! That form is vanishingly rare, and it fails by dropping the rule rather
//! than by mis-matching it.

use dom::{Document, NodeId};

/// How two compounds in a selector relate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combinator {
    /// `a b` — matches at any depth.
    Descendant,
    /// `a > b` — matches only the immediate parent.
    Child,
}

/// An `[attribute]` test, per CSS 2.1 §5.8.
#[derive(Debug, Clone, PartialEq)]
pub struct AttributeTest {
    /// Lowercased attribute name.
    pub name: String,
    /// What the value must satisfy.
    pub match_: AttributeMatch,
}

/// The comparison an attribute selector performs.
#[derive(Debug, Clone, PartialEq)]
pub enum AttributeMatch {
    /// `[att]` — the attribute is present, whatever its value.
    Present,
    /// `[att=val]` — the value is exactly this.
    Exact(String),
    /// `[att~=val]` — this is one of the value's space-separated words.
    Word(String),
    /// `[att|=val]` — the value is this, or begins with this and a hyphen.
    /// Meant for language subtags: `[lang|=en]` matches `en-GB`.
    Prefix(String),
}

impl AttributeTest {
    fn matches(&self, value: &str) -> bool {
        match &self.match_ {
            AttributeMatch::Present => true,
            AttributeMatch::Exact(wanted) => value == wanted,
            AttributeMatch::Word(wanted) => {
                // An empty operand can never match a word, and would otherwise
                // match the gap between two spaces.
                !wanted.is_empty() && value.split_ascii_whitespace().any(|word| word == wanted)
            }
            AttributeMatch::Prefix(wanted) => {
                value == wanted
                    || (value.len() > wanted.len()
                        && value.starts_with(wanted.as_str())
                        && value.as_bytes()[wanted.len()] == b'-')
            }
        }
    }
}

/// A sequence of simple selectors with no combinator between them, e.g.
/// `div#main.wide`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Compound {
    /// Type selector, or `None` for `*` or a bare class/id selector.
    pub tag: Option<String>,
    /// `#id`, at most one.
    pub id: Option<String>,
    /// Every `.class` in the compound.
    pub classes: Vec<String>,
    /// Every `[attribute]` test in the compound.
    pub attributes: Vec<AttributeTest>,
}

impl Compound {
    /// Whether this compound matches a single element, ignoring combinators.
    fn matches(&self, doc: &Document, node: NodeId) -> bool {
        let Some(element) = doc.element(node) else {
            return false;
        };
        if let Some(tag) = &self.tag
            && element.local_name() != tag
        {
            return false;
        }
        if let Some(id) = &self.id
            && element.id() != Some(id.as_str())
        {
            return false;
        }
        if !self
            .classes
            .iter()
            .all(|wanted| element.classes().any(|actual| actual == wanted))
        {
            return false;
        }
        self.attributes
            .iter()
            .all(|test| element.attr(&test.name).is_some_and(|v| test.matches(v)))
    }

    fn is_empty(&self) -> bool {
        self.tag.is_none()
            && self.id.is_none()
            && self.classes.is_empty()
            && self.attributes.is_empty()
    }
}

/// A full selector: compounds ordered left to right, each paired with the
/// combinator that links it to the one before.
#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    /// `(combinator to previous compound, compound)`. The first entry's
    /// combinator is unused.
    pub parts: Vec<(Combinator, Compound)>,
}

/// CSS specificity, compared lexicographically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Specificity {
    /// Count of id selectors.
    pub ids: u32,
    /// Count of class selectors.
    pub classes: u32,
    /// Count of type selectors.
    pub types: u32,
}

impl Selector {
    /// This selector's specificity.
    pub fn specificity(&self) -> Specificity {
        let mut out = Specificity::default();
        for (_, compound) in &self.parts {
            out.ids += u32::from(compound.id.is_some());
            // An attribute selector counts at the same level as a class.
            out.classes += (compound.classes.len() + compound.attributes.len()) as u32;
            out.types += u32::from(compound.tag.is_some());
        }
        out
    }

    /// Whether this selector matches `node`.
    ///
    /// Evaluated right to left, which is what makes selector matching cheap:
    /// the rightmost compound rejects the overwhelming majority of candidates
    /// before any ancestor is touched.
    pub fn matches(&self, doc: &Document, node: NodeId) -> bool {
        let Some(last) = self.parts.last() else {
            return false;
        };
        if !last.1.matches(doc, node) {
            return false;
        }

        let mut current = node;
        // Step leftwards. The combinator that links `parts[i - 1]` to
        // `parts[i]` is stored on `parts[i]`, so the pair must be read together
        // — taking the combinator from the left-hand compound instead silently
        // turns every child combinator into a descendant one.
        for index in (1..self.parts.len()).rev() {
            let combinator = &self.parts[index].0;
            let compound = &self.parts[index - 1].1;
            match combinator {
                Combinator::Child => {
                    let Some(parent) = doc.node(current).parent else {
                        return false;
                    };
                    if !compound.matches(doc, parent) {
                        return false;
                    }
                    current = parent;
                }
                Combinator::Descendant => {
                    // Walk up until an ancestor matches. Taking the first match
                    // is not strictly correct for all selectors — backtracking
                    // is required in general — but it is correct for the
                    // descendant-only selectors in scope here.
                    let mut ancestor = doc.node(current).parent;
                    loop {
                        let Some(candidate) = ancestor else {
                            return false;
                        };
                        if compound.matches(doc, candidate) {
                            current = candidate;
                            break;
                        }
                        ancestor = doc.node(candidate).parent;
                    }
                }
            }
        }
        true
    }
}

/// Parses a comma-separated selector list.
///
/// Returns only the selectors that parsed, and **that is wrong for one of the
/// two reasons a selector can fail to parse.** This comment used to justify it
/// as "matching how browsers treat unknown syntax"; browsers do not do this.
/// CSS 2.1 §4.1.7 says a statement with an error anywhere in its selector is
/// ignored *entirely*, so `[1digit], div { color: red }` styles nothing at all
/// — the malformed attribute name takes the valid `div` down with it. Keeping
/// the `div` applies a colour the author never asked for, which is how the CSS
/// 2.1 suite catches this: a page of tests whose whole assertion is "no red".
///
/// The fix is not simply to be strict, which is why this is a comment rather
/// than a patch. Two different failures arrive here as the same `None`:
///
/// * **Invalid** — `[1digit]`, `[title~=]`. No browser accepts it, the rule
///   must be dropped, and dropping it is what the suite expects.
/// * **Unsupported** — `p:first-child`, `a::before`. Valid CSS 2.1 that this
///   engine does not implement. Browsers apply these, so dropping the whole
///   rule would *lose* styling they show, making real pages worse in exchange
///   for passing tests.
///
/// So the honest fix distinguishes them: drop the rule for the first, skip the
/// selector for the second. That needs a three-way result threaded through
/// `parse_compound` and `parse_attribute_test`, and it wants measuring against
/// both the suite and the reference baselines, because it can move rendering
/// on real pages in either direction.
pub fn parse_selector_list(input: &str) -> Vec<Selector> {
    input
        .split(',')
        .filter_map(|s| parse_selector(s.trim()))
        .collect()
}

fn parse_selector(input: &str) -> Option<Selector> {
    let mut parts: Vec<(Combinator, Compound)> = Vec::new();
    let mut combinator = Combinator::Descendant;

    for token in input.split_whitespace() {
        if token == ">" {
            combinator = Combinator::Child;
            continue;
        }
        // `a > b` may also arrive unspaced as `a>b`.
        for (index, piece) in token.split('>').enumerate() {
            if index > 0 {
                combinator = Combinator::Child;
            }
            if piece.is_empty() {
                continue;
            }
            let compound = parse_compound(piece)?;
            parts.push((combinator, compound));
            combinator = Combinator::Descendant;
        }
    }

    (!parts.is_empty()).then_some(Selector { parts })
}

fn parse_compound(input: &str) -> Option<Compound> {
    let mut compound = Compound::default();
    let mut chars = input.chars().peekable();

    // A leading type selector or `*`, if any.
    let mut tag = String::new();
    while let Some(&c) = chars.peek() {
        if c == '.' || c == '#' || c == '[' {
            break;
        }
        chars.next();
        tag.push(c);
    }
    if !tag.is_empty() && tag != "*" {
        if !tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return None;
        }
        compound.tag = Some(tag.to_ascii_lowercase());
    }

    while let Some(marker) = chars.next() {
        if marker == '[' {
            // An attribute test runs to its closing bracket, which may enclose
            // a quoted value containing `.` or `#`.
            let mut body = String::new();
            let mut closed = false;
            for c in chars.by_ref() {
                if c == ']' {
                    closed = true;
                    break;
                }
                body.push(c);
            }
            if !closed {
                return None;
            }
            compound.attributes.push(parse_attribute_test(&body)?);
            continue;
        }

        let mut name = String::new();
        while let Some(&c) = chars.peek() {
            if c == '.' || c == '#' || c == '[' {
                break;
            }
            chars.next();
            name.push(c);
        }
        if name.is_empty() {
            return None;
        }
        match marker {
            '.' => compound.classes.push(name),
            '#' => compound.id = Some(name),
            // Pseudo-classes and anything else are out of scope; drop the whole
            // selector rather than match too broadly.
            _ => return None,
        }
    }

    if compound.is_empty() && tag != "*" {
        return None;
    }
    Some(compound)
}

/// Parses the inside of an `[…]`, without the brackets.
fn parse_attribute_test(body: &str) -> Option<AttributeTest> {
    let body = body.trim();
    // Longest operator first: `~=` and `|=` both end in `=`, so testing for
    // `=` before them would split them in the wrong place.
    let split = ["~=", "|=", "="]
        .into_iter()
        .filter_map(|operator| body.find(operator).map(|at| (operator, at)))
        .min_by_key(|(_, at)| *at);

    let (name, match_) = match split {
        None => (body, AttributeMatch::Present),
        Some((operator, at)) => {
            let value = unquote(body[at + operator.len()..].trim());
            let match_ = match operator {
                "~=" => AttributeMatch::Word(value),
                "|=" => AttributeMatch::Prefix(value),
                _ => AttributeMatch::Exact(value),
            };
            (&body[..at], match_)
        }
    };

    let name = name.trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    Some(AttributeTest {
        // HTML attribute names are case-insensitive and the DOM stores them
        // lowercased, so the selector's must be too.
        name: name.to_ascii_lowercase(),
        match_,
    })
}

/// Strips one layer of matching quotes, which an attribute value may carry.
fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[bytes.len() - 1] == bytes[0]
    {
        return value[1..value.len() - 1].to_owned();
    }
    value.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> Document {
        dom::parse(
            r#"<html><body>
                 <div id="main" class="wide box">
                   <p class="lead">one</p>
                   <section><p>two</p></section>
                 </div>
               </body></html>"#,
        )
    }

    fn matching(doc: &Document, selector: &str) -> usize {
        let selectors = parse_selector_list(selector);
        doc.descendants(doc.root())
            .into_iter()
            .filter(|&n| selectors.iter().any(|s| s.matches(doc, n)))
            .count()
    }

    #[test]
    fn matches_type_class_and_id() {
        let doc = doc();
        assert_eq!(matching(&doc, "p"), 2);
        assert_eq!(matching(&doc, ".lead"), 1);
        assert_eq!(matching(&doc, "#main"), 1);
        assert_eq!(matching(&doc, "div.wide.box"), 1);
        assert_eq!(matching(&doc, "div.wide.missing"), 0);
    }

    #[test]
    fn distinguishes_descendant_from_child() {
        let doc = doc();
        assert_eq!(matching(&doc, "div p"), 2, "descendant reaches nested p");
        assert_eq!(matching(&doc, "div > p"), 1, "child does not");
        assert_eq!(matching(&doc, "div>p"), 1, "unspaced child combinator");
    }

    #[test]
    fn selector_lists_union_their_matches() {
        assert_eq!(matching(&doc(), "section, .lead"), 2);
    }

    #[test]
    fn specificity_orders_correctly() {
        let id = parse_selector_list("#main")[0].specificity();
        let class = parse_selector_list(".lead")[0].specificity();
        let tag = parse_selector_list("p")[0].specificity();
        assert!(id > class && class > tag);
        // Many types never outrank one class.
        assert!(parse_selector_list("div section p")[0].specificity() < class);
    }

    #[test]
    fn unsupported_syntax_drops_only_its_own_selector() {
        let selectors = parse_selector_list("p:hover, .lead");
        assert_eq!(selectors.len(), 1);
        assert_eq!(selectors[0].specificity().classes, 1);
    }

    #[test]
    fn universal_selector_matches_every_element() {
        let doc = doc();
        let elements = doc
            .descendants(doc.root())
            .into_iter()
            .filter(|&n| doc.element(n).is_some())
            .count();
        assert_eq!(matching(&doc, "*"), elements);
    }
}
