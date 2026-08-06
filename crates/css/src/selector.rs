//! Selector parsing and matching.
//!
//! Scope is the CSS 2.1 subset M1 needs: type, class, id, and universal
//! selectors, combined into compounds and joined by descendant or child
//! combinators. Attribute, pseudo-class, sibling, and pseudo-element selectors
//! arrive with the rest of the cascade in M2.

use dom::{Document, NodeId};

/// How two compounds in a selector relate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combinator {
    /// `a b` — matches at any depth.
    Descendant,
    /// `a > b` — matches only the immediate parent.
    Child,
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
        self.classes
            .iter()
            .all(|wanted| element.classes().any(|actual| actual == wanted))
    }

    fn is_empty(&self) -> bool {
        self.tag.is_none() && self.id.is_none() && self.classes.is_empty()
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
            out.classes += compound.classes.len() as u32;
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
/// Returns only the selectors that parsed. A selector list containing anything
/// unsupported drops just that selector, matching how browsers treat unknown
/// syntax: ignore what you cannot understand, keep what you can.
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
        if c == '.' || c == '#' {
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
        let mut name = String::new();
        while let Some(&c) = chars.peek() {
            if c == '.' || c == '#' {
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
            // Pseudo-classes, attribute selectors, and anything else are out of
            // scope; drop the whole selector rather than match too broadly.
            _ => return None,
        }
    }

    if compound.is_empty() && tag != "*" {
        return None;
    }
    Some(compound)
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
