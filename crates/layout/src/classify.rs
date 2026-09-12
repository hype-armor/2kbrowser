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
    /// The page's *frame* is built on layout we do not implement, even though
    /// its prose is not inside it.
    ///
    /// The share above cannot see this case and no proportional measure can:
    /// on a page like a Wikipedia article the reading matter really is
    /// ordinary flow, and by volume the page is overwhelmingly correct. What
    /// is wrong is the structure around it — the columns, the header, the
    /// rows of navigation — and that is a handful of containers holding
    /// almost no text at all.
    DocumentFrame {
        /// How many containers would have laid their children out in a row.
        containers: usize,
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
            RenderMode::DocumentFrame { containers } => Some(format!(
                "Rendered as a document: this page's layout is built out of {containers} \
                 rows this browser cannot arrange, even though its text is ordinary."
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

/// How many row-forming containers make a page's *frame* unsupported.
///
/// Unlike the share above, this is a count, and that is not an oversight: the
/// share answers "is the reading matter broken", and a page can answer no to
/// that while its structure is still gone. A Wikipedia article is the case —
/// its prose is ordinary flow, so every proportional measure reports the page
/// as almost entirely fine, and the columns and navigation rows around it are
/// still laid out wrongly.
///
/// Measured rather than guessed, which is what ADR-0009 asked for and did not
/// have. Row-forming containers, at a 1000px viewport:
///
/// | page                          | rows | flex containers |
/// |-------------------------------|------|-----------------|
/// | `example.com`                 |    0 |               0 |
/// | `info.cern.ch` (the first)    |    0 |               0 |
/// | `news.ycombinator.com`        |    0 |               0 |
/// | `motherfuckingwebsite.com`    |    0 |               0 |
/// | `gnu.org`                     |    0 |               8 |
/// | this repository's `era-page`  |    0 |               0 |
/// | **Wikipedia, an article**     | **16** |         **484** |
///
/// Every page that should keep its author's layout scores zero, `gnu.org`
/// included — it has eight flex containers and not one of them arranges a
/// row of blocks. Four is the threshold because it is well clear of zero and
/// well under sixteen; a corpus of six is small, and this will want revisiting
/// with a larger one, which is the same caveat the share above carries.
pub const UNSUPPORTED_FRAME_THRESHOLD: usize = 4;

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
        return RenderMode::Document {
            unsupported_share: share,
        };
    }

    // The share is asked first because it is the stronger signal: text inside
    // a container we cannot lay out is text in the wrong place, whatever the
    // page looks like around it. The frame is asked second, for the pages the
    // share cannot see.
    let containers = row_containers(doc, styles, body);
    if containers >= UNSUPPORTED_FRAME_THRESHOLD {
        return RenderMode::DocumentFrame { containers };
    }

    RenderMode::Authored
}

/// How many unsupported containers would have laid their children out in a row.
///
/// Not every flex container changes anything when it is ignored. One holding a
/// single line of text lays that text out the same way either way, and a page
/// can carry dozens of those without a pixel moving — a footer of them is the
/// case this deliberately does not count, and `gnu.org` is the real page that
/// proves it, with eight flex containers and no rows.
///
/// A container with *two or more block-level children* is different. Those
/// children are meant to sit side by side and will instead stack down the
/// page, which is a visible change to the page's shape rather than to its
/// contents. Counting those is what separates a page that uses flex from a
/// page built out of it.
fn row_containers(doc: &Document, styles: &StyleMap, node: NodeId) -> usize {
    if is_display_none(styles, node) {
        return 0;
    }
    let own = usize::from(
        styles
            .get(node)
            .is_some_and(|style| !style.display.is_supported_layout())
            && block_children(doc, styles, node) >= 2,
    );
    own + doc
        .children(node)
        .iter()
        .map(|&child| row_containers(doc, styles, child))
        .sum::<usize>()
}

/// How many of a node's children are block-level boxes that would lay out.
fn block_children(doc: &Document, styles: &StyleMap, node: NodeId) -> usize {
    doc.children(node)
        .iter()
        .filter(|&&child| {
            styles.get(child).is_some_and(|style| {
                style.display != css::style::Display::None && !style.display.is_inline()
            })
        })
        .count()
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
    fn a_page_whose_frame_is_rows_of_blocks_falls_back() {
        // The case no share can see: the prose is ordinary flow and by volume
        // the page is almost entirely fine, while the structure around it is
        // laid out in rows this engine cannot arrange. A Wikipedia article
        // scores sixteen of these; every page in the corpus that should keep
        // its author's layout scores zero.
        let frame = (0..5)
            .map(|i| format!("<div class=\"row\"><div>a{i}</div><div>b{i}</div></div>"))
            .collect::<String>();
        let html = format!("<body>{frame}<main>{}</main></body>", paragraphs(20, "x"));
        let mode = classify_html(&html, ".row { display: flex }");
        assert!(
            matches!(mode, RenderMode::DocumentFrame { containers: 5 }),
            "got {mode:?}"
        );
    }

    #[test]
    fn a_container_holding_one_thing_is_not_a_row() {
        // Ignoring a flex container that holds a single block changes nothing
        // about where that block goes, so it must not count. This is the
        // footer-of-flex-boxes case, and `gnu.org` is the real page it stands
        // for: eight flex containers, not one of them a row.
        let frame = (0..20)
            .map(|i| format!("<div class=\"row\"><div>only{i}</div></div>"))
            .collect::<String>();
        let html = format!("<body>{frame}<main>{}</main></body>", paragraphs(20, "x"));
        assert_eq!(
            classify_html(&html, ".row { display: flex }"),
            RenderMode::Authored
        );
    }

    #[test]
    fn a_row_of_inline_children_is_not_a_row_either() {
        // Inline children already sit side by side without flex, so ignoring
        // the container leaves them where they were.
        let frame = (0..10)
            .map(|i| format!("<div class=\"row\"><span>a{i}</span><span>b{i}</span></div>"))
            .collect::<String>();
        let html = format!("<body>{frame}<main>{}</main></body>", paragraphs(20, "x"));
        assert_eq!(
            classify_html(&html, ".row { display: flex }"),
            RenderMode::Authored
        );
    }

    #[test]
    fn a_handful_of_rows_is_below_the_threshold() {
        // A page with one flex row in its header is not a page built out of
        // flex, and its article should keep the author's layout.
        let html = format!(
            "<body><div class=\"row\"><div>a</div><div>b</div></div><main>{}</main></body>",
            paragraphs(20, "x")
        );
        assert_eq!(
            classify_html(&html, ".row { display: flex }"),
            RenderMode::Authored
        );
    }

    #[test]
    fn rows_inside_a_hidden_subtree_do_not_count() {
        // A page carrying a hidden menu built out of flex rows is not a page
        // whose frame is broken: none of it is drawn. The walk has to stop at
        // the hidden ancestor rather than only at the rows themselves.
        let frame = (0..8)
            .map(|i| format!("<div class=\"row\"><div>a{i}</div><div>b{i}</div></div>"))
            .collect::<String>();
        let html = format!(
            "<body><div class=\"hidden\">{frame}</div><main>{}</main></body>",
            paragraphs(20, "x")
        );
        assert_eq!(
            classify_html(&html, ".row { display: flex } .hidden { display: none }"),
            RenderMode::Authored
        );
    }

    #[test]
    fn the_frame_explanation_does_not_quote_a_share() {
        // There is no share to quote here, and a small one would read as
        // "mostly fine" — which is exactly the impression that made this case
        // invisible in the first place.
        let text = RenderMode::DocumentFrame { containers: 14 }
            .explanation()
            .expect("an explanation");
        assert!(text.contains("14"), "{text}");
        assert!(!text.contains('%'), "{text}");
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
    fn a_page_laid_out_with_inline_blocks_keeps_the_authors_layout() {
        // This used to be the reverse assertion, and the reversal is the
        // point: inline-block was the quietest of the unimplemented layouts —
        // laid out as a plain inline, it still showed its text and merely lost
        // its box — so a whole page built on one fell back rather than come out
        // subtly wrong with nothing said. It is laid out now, so it does not.
        let html = format!(
            r#"<body><div id="app">{}</div><nav>{}</nav></body>"#,
            paragraphs(10, "x"),
            (0..8)
                .map(|i| format!(r##"<a class="nav" href="#">Link {i}</a>"##))
                .collect::<String>(),
        );
        let mode = classify_html(&html, "#app, .nav { display: inline-block }");
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
