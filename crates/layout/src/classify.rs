//! Deciding whether a document can be laid out, or must be re-rendered as a
//! document (ADR-0009).

use css::cascade::StyleMap;
use dom::{Document, NodeId};

/// How a document should be rendered.
#[derive(Debug, Clone, PartialEq)]
pub enum RenderMode {
    /// Layout uses only features we implement. Render it as authored.
    Authored,
    /// The page has content, but a significant share of it sits under layout we
    /// do not implement. Render it as a document instead of producing a layout
    /// we already know to be wrong.
    Document {
        /// Fraction of text content under unsupported layout, 0.0–1.0.
        unsupported_share: f32,
    },
    /// The page has essentially no content without scripting. Reader mode
    /// cannot help; there is nothing to extract.
    RequiresScripting,
}

impl RenderMode {
    /// A short explanation for the chrome.
    ///
    /// ADR-0009 forbids switching rendering mode silently, so every non-default
    /// mode has to be able to say why it was chosen.
    pub fn explanation(&self) -> Option<String> {
        match self {
            RenderMode::Authored => None,
            RenderMode::Document { unsupported_share } => Some(format!(
                "Rendered as a document: {}% of this page's content uses layout \
                 this browser does not implement.",
                (unsupported_share * 100.0).round() as u32
            )),
            RenderMode::RequiresScripting => Some(
                "This page has no content without JavaScript, which this browser \
                 does not run."
                    .to_owned(),
            ),
        }
    }
}

/// Share of text content under unsupported layout beyond which we stop trying
/// to reproduce the author's layout.
///
/// A first guess, and explicitly flagged as such in ADR-0009: it wants a corpus
/// behind it. Pages near the threshold will flip between modes, which is why
/// the user needs the override.
pub const UNSUPPORTED_SHARE_THRESHOLD: f32 = 0.40;

/// Below this many characters, a document with scripts is treated as an empty
/// shell rather than a short page.
const MIN_CONTENT_CHARS: usize = 200;

/// Classifies a styled document.
pub fn classify(doc: &Document, styles: &StyleMap) -> RenderMode {
    let body = doc.find_element("body").unwrap_or_else(|| doc.root());
    let total_text = visible_text_len(doc, styles, body);

    if total_text < MIN_CONTENT_CHARS && script_count(doc) > 0 {
        return RenderMode::RequiresScripting;
    }
    if total_text == 0 {
        return RenderMode::Authored;
    }

    let unsupported = unsupported_text_len(doc, styles, body, false);
    let share = unsupported as f32 / total_text as f32;

    if share >= UNSUPPORTED_SHARE_THRESHOLD {
        RenderMode::Document {
            unsupported_share: share,
        }
    } else {
        RenderMode::Authored
    }
}

fn script_count(doc: &Document) -> usize {
    doc.descendants(doc.root())
        .into_iter()
        .filter(|&n| doc.element(n).is_some_and(|e| e.local_name() == "script"))
        .count()
}

/// Length of text that would actually be painted, skipping `display: none`
/// subtrees so that hidden boilerplate does not count toward the total.
fn visible_text_len(doc: &Document, styles: &StyleMap, node: NodeId) -> usize {
    if is_display_none(styles, node) {
        return 0;
    }
    if let Some(text) = doc.text(node) {
        return text.trim().chars().count();
    }
    doc.children(node)
        .iter()
        .map(|&c| visible_text_len(doc, styles, c))
        .sum()
}

/// Length of text sitting under at least one unsupported container.
///
/// Weighted by *text*, not by element count, and deliberately: one flex
/// container wrapping the whole page should dominate, and fifty flex containers
/// in a footer should not.
fn unsupported_text_len(
    doc: &Document,
    styles: &StyleMap,
    node: NodeId,
    inside_unsupported: bool,
) -> usize {
    if is_display_none(styles, node) {
        return 0;
    }
    if let Some(text) = doc.text(node) {
        return if inside_unsupported {
            text.trim().chars().count()
        } else {
            0
        };
    }
    let unsupported = inside_unsupported
        || styles
            .get(node)
            .is_some_and(|s| !s.display.is_supported_layout());
    doc.children(node)
        .iter()
        .map(|&c| unsupported_text_len(doc, styles, c, unsupported))
        .sum()
}

fn is_display_none(styles: &StyleMap, node: NodeId) -> bool {
    styles
        .get(node)
        .is_some_and(|s| s.display == css::style::Display::None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use css::Stylesheet;

    fn classify_html(html: &str, css_text: &str) -> RenderMode {
        let doc = dom::parse(html);
        let sheets = [Stylesheet::parse(css_text)];
        let styles = css::cascade::cascade(&doc, &sheets);
        classify(&doc, &styles)
    }

    fn paragraphs(n: usize, class: &str) -> String {
        (0..n)
            .map(|i| format!(r#"<p class="{class}">Sentence number {i} with enough text.</p>"#))
            .collect()
    }

    #[test]
    fn ordinary_pages_render_as_authored() {
        let html = format!("<body><div>{}</div></body>", paragraphs(10, "x"));
        assert_eq!(classify_html(&html, ""), RenderMode::Authored);
    }

    #[test]
    fn a_flex_wrapped_page_falls_back_to_document() {
        let html = format!(
            r#"<body><div id="app">{}</div></body>"#,
            paragraphs(10, "x")
        );
        let mode = classify_html(&html, "#app { display: flex }");
        assert!(
            matches!(mode, RenderMode::Document { .. }),
            "page wrapped in flex should fall back, got {mode:?}"
        );
    }

    #[test]
    fn a_little_flex_does_not_trigger_fallback() {
        // Fifty flex containers in a footer must not outvote the article body.
        let html = format!(
            "<body><main>{}</main><footer class=\"f\">nav</footer></body>",
            paragraphs(20, "x")
        );
        let mode = classify_html(&html, ".f { display: flex }");
        assert_eq!(mode, RenderMode::Authored, "got {mode:?}");
    }

    #[test]
    fn a_page_laid_out_with_inline_blocks_falls_back_too() {
        // The quietest of the unimplemented layouts, and so the one most worth
        // asserting. Flex and grid produce nothing at all if they are ignored,
        // which is obvious; an inline-block laid out as a plain inline still
        // shows its text and merely loses its box, so a page built on them used
        // to come out subtly wrong with nothing said.
        let html = format!(
            r#"<body><div id="app">{}</div></body>"#,
            paragraphs(10, "x")
        );
        let mode = classify_html(&html, "#app { display: inline-block }");
        assert!(
            matches!(mode, RenderMode::Document { .. }),
            "page wrapped in an inline-block should fall back, got {mode:?}"
        );
    }

    #[test]
    fn a_navigation_bar_of_inline_blocks_does_not_move_an_article() {
        // The other half, and the reason this is a share rather than a switch.
        // Inline-block is the commonest of the three in incidental use — a row
        // of navigation links, a set of badges — and a page whose *article* is
        // ordinary must keep the author's layout.
        let html = format!(
            "<body><nav>{}</nav><main>{}</main></body>",
            (0..8)
                .map(|i| format!(r##"<a class="nav" href="#">Link {i}</a>"##))
                .collect::<String>(),
            paragraphs(20, "x"),
        );
        let mode = classify_html(&html, ".nav { display: inline-block }");
        assert_eq!(mode, RenderMode::Authored, "got {mode:?}");
    }

    #[test]
    fn an_empty_spa_shell_reports_that_it_needs_scripting() {
        let html = r#"<body><div id="root"></div><script src="app.js"></script></body>"#;
        assert_eq!(classify_html(html, ""), RenderMode::RequiresScripting);
    }

    #[test]
    fn a_short_page_without_scripts_is_not_an_spa_shell() {
        assert_eq!(
            classify_html("<body><p>Short but real.</p></body>", ""),
            RenderMode::Authored
        );
    }

    #[test]
    fn hidden_content_does_not_count_toward_the_share() {
        // Text inside display:none must not drag the page into fallback.
        let html = format!(
            r#"<body><div class="hide">{}</div>{}</body>"#,
            paragraphs(20, "y"),
            paragraphs(10, "x")
        );
        let mode = classify_html(&html, ".hide { display: none; }");
        assert_eq!(mode, RenderMode::Authored, "got {mode:?}");
    }

    #[test]
    fn every_non_authored_mode_explains_itself() {
        // ADR-0009: the mode is never switched silently.
        assert!(RenderMode::Authored.explanation().is_none());
        assert!(
            RenderMode::Document {
                unsupported_share: 0.8
            }
            .explanation()
            .is_some()
        );
        assert!(RenderMode::RequiresScripting.explanation().is_some());
    }
}
