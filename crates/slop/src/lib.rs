//! Content filtering, reader mode, and content-quality heuristics.
//!
//! Today that is one thing: finding the article on a page and saying what is
//! wrapped around it. ADR-0009 puts content extraction on the critical path,
//! because the document fallback it describes is what a reader falls back *to*.
//! Discarding the author's layout without also discarding the author's
//! furniture leaves the navigation, the sidebar and the footer stacked one
//! after another above the first sentence — every link on the site, in a
//! column, in front of the thing you came to read.
//!
//! The approach is the one every reader mode uses, Firefox's included: score
//! the containers by how much running prose they hold, take the best one, and
//! drop the rest. It is a heuristic and it is wrong sometimes, which is why
//! [`extract`] refuses to act when it is not confident, and why the reader can
//! always ask for the author's layout back.
//!
//! Nothing here removes nodes. It names them, and the caller takes them out of
//! the flow — so the same parsed document can be rendered either way, and
//! pressing "as authored" does not cost a re-parse.

use std::collections::{HashMap, HashSet};

use dom::{Document, NodeId};

/// What a document's reader rendering should leave out.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Boilerplate {
    /// Nodes to take out of the flow, each the shallowest of its branch — so
    /// hiding them hides their subtrees and nothing else has to be walked.
    pub dropped: Vec<NodeId>,
    /// Whether an article was actually identified.
    ///
    /// `false` means only the unmistakable furniture was removed, because
    /// nothing on the page looked enough like an article to be confident about.
    /// A page can be a list of links and still be the page the reader asked
    /// for.
    pub found_article: bool,
}

/// Elements that are navigation by their own definition, wherever they appear.
const NAVIGATION_ELEMENTS: &[&str] = &["nav"];

/// ARIA landmark roles naming a region as something other than the content.
///
/// `main` and `article` are landmarks too, and are deliberately absent: those
/// say "this *is* the content", which is evidence rather than grounds for
/// removal.
const CHROME_ROLES: &[&str] = &[
    "navigation",
    "banner",
    "complementary",
    "contentinfo",
    "search",
    "menu",
    "menubar",
    "toolbar",
    "dialog",
    "alertdialog",
];

/// Elements that are the page's furniture rather than the article's — unless
/// they sit inside one, where a `<header>` is the piece holding the title and
/// the byline and belongs to what is being read.
const SURROUNDING_ELEMENTS: &[&str] = &["header", "footer", "aside"];

/// Elements that never hold prose worth keeping in a reading view.
const NEVER_CONTENT: &[&str] = &["form", "fieldset", "button", "select", "textarea", "input"];

/// Words in a `class` or `id` suggesting furniture.
///
/// Substrings rather than whole words, because these arrive as `site-header`,
/// `nav__list`, `commentsWrapper` and every other joining convention there has
/// ever been. That looseness is exactly why they only ever *weigh* against a
/// container and never remove one on their own: `heading` contains `head`, and
/// an article's own heading is not furniture.
const FURNITURE_WORDS: &[&str] = &[
    "banner",
    "breadcrumb",
    "comment",
    "cookie",
    "disqus",
    "footer",
    "masthead",
    "menu",
    "modal",
    "nav",
    "newsletter",
    "pagination",
    "paywall",
    "popup",
    "promo",
    "related",
    "share",
    "sidebar",
    "signup",
    "social",
    "sponsor",
    "subscribe",
    "toolbar",
    "widget",
];

/// Words in a `class` or `id` suggesting the article.
const CONTENT_WORDS: &[&str] = &[
    "article", "body", "content", "entry", "main", "post", "story", "text",
];

/// Elements whose text counts as prose when scoring the containers above them.
const PARAGRAPH_ELEMENTS: &[&str] = &["p", "pre", "blockquote", "td", "figcaption"];

/// Elements that may be chosen as the article.
///
/// Not `<li>` and not `<a>`: a list item scoring well means a list of links
/// scored well, and choosing one would keep a menu entry and drop the page.
const CANDIDATE_ELEMENTS: &[&str] = &[
    "div",
    "section",
    "article",
    "main",
    "td",
    "blockquote",
    "body",
];

/// Shortest run of text that counts as a paragraph.
///
/// Below this it is a label, a caption fragment or a menu entry, and counting
/// it would let a column of them outscore a column of prose.
const MIN_PARAGRAPH_CHARS: usize = 25;

/// Most a single paragraph may contribute for its length alone.
///
/// Without a cap, one enormous block — a licence, a transcript, a comment
/// thread — outweighs every other signal on the page.
const MAX_LENGTH_SCORE: f32 = 3.0;

/// How far up the tree a paragraph's score is shared.
const SCORE_REACH: usize = 4;

/// How much more text an ancestor may bring with it and still be preferred.
///
/// The article's headline, byline and captions score nothing — they are not
/// prose — so the tightest container around the paragraphs usually sits a shade
/// *inside* the article, and stopping there loses them. What separates them
/// from a sidebar is not their score but their size: a byline is a line, and a
/// sidebar is a column. Climbing while the text barely grows takes back the
/// first and stops at the second.
///
/// Comparing scores instead does not work, and looks as though it should. A
/// parent is one step further from the paragraphs, so it holds a fraction of
/// their score by construction — the climb would refuse at the first step,
/// every time, whatever the threshold was set to.
const PARENT_TEXT_ALLOWANCE: f32 = 0.25;

/// Share of the top score a sibling must reach to be kept beside it.
const SIBLING_SHARE: f32 = 0.2;

/// Link share above which a block is a list of links rather than prose.
const LINK_DENSITY_LIMIT: f32 = 0.75;

/// Fewest links a block needs before its link density is worth believing.
///
/// One link in a short paragraph is a citation, not a menu.
const MIN_LINKS_FOR_DENSITY: usize = 3;

/// Least text an extraction may leave behind, in characters.
const MIN_ARTICLE_CHARS: usize = 250;

/// Least share of the page's text an extraction may leave behind.
///
/// Together with [`MIN_ARTICLE_CHARS`] this is the whole safety net. Reader
/// mode showing a page's furniture is a poor rendering; reader mode showing one
/// paragraph and silently dropping the rest is a broken browser, and from the
/// reader's side the two look identical.
const MIN_ARTICLE_SHARE: f32 = 0.25;

/// Finds the article and names everything else.
pub fn extract(doc: &Document) -> Boilerplate {
    let Some(body) = doc.find_element("body") else {
        return Boilerplate::default();
    };
    let text = TextMeasure::of(doc);
    let chrome = structural_chrome(doc, body, &text);
    let scores = score(doc, body, &text, &chrome);

    let Some(article) = choose_article(doc, body, &text, &scores, &chrome) else {
        // Nothing on the page looked enough like an article to act on.
        return Boilerplate {
            dropped: shallowest(doc, body, &chrome),
            found_article: false,
        };
    };

    // Everything outside the article, and the furniture that survived inside
    // it: a "share this" bar between two paragraphs is inside the article by
    // every structural measure and is still not part of it.
    let keep = kept_subtrees(doc, article.chosen, article.best, &scores);
    let mut drop = chrome;
    for node in doc.descendants(body) {
        // Kept, inside something kept, or holding something kept. The last of
        // those is the one that is easy to forget and fatal to miss: the
        // wrappers between `<body>` and the article are not themselves the
        // article, and dropping them takes the article with them.
        let wanted = keep
            .iter()
            .any(|&k| k == node || is_under(doc, node, k) || is_under(doc, k, node));
        if node != body && !wanted {
            drop.insert(node);
        }
    }
    drop.remove(&body);

    Boilerplate {
        dropped: shallowest(doc, body, &drop),
        found_article: true,
    }
}

/// Character counts for every node's subtree, and how many of them are links.
///
/// Measured once for the whole document rather than per question. Asking
/// `text_content` for each candidate walks the subtree again and builds a
/// `String` on the way, which over a page is quadratic work for an answer that
/// is a number.
struct TextMeasure {
    /// Non-whitespace characters in each node's subtree.
    chars: Vec<usize>,
    /// How many of those sit inside an `<a href>`.
    link_chars: Vec<usize>,
    /// How many `<a href>` elements the subtree holds.
    links: Vec<usize>,
}

impl TextMeasure {
    fn of(doc: &Document) -> Self {
        let mut measure = Self {
            chars: vec![0; doc.len()],
            link_chars: vec![0; doc.len()],
            links: vec![0; doc.len()],
        };
        measure.walk(doc, doc.root(), false);
        measure
    }

    /// Fills in `node` and hands its totals back, so one pass does the tree.
    fn walk(&mut self, doc: &Document, node: NodeId, in_link: bool) -> (usize, usize, usize) {
        if let Some(text) = doc.text(node) {
            // Whitespace is not content: the indentation of hand-written markup
            // would otherwise make every wrapper look like it held prose.
            let count = text.chars().filter(|c| !c.is_whitespace()).count();
            self.chars[node.0] = count;
            self.link_chars[node.0] = if in_link { count } else { 0 };
            return (count, self.link_chars[node.0], 0);
        }
        let is_link = doc
            .element(node)
            .is_some_and(|e| e.local_name() == "a" && e.attr("href").is_some());
        let (mut chars, mut link_chars, mut links) = (0, 0, usize::from(is_link));
        for &child in doc.children(node) {
            let (c, l, n) = self.walk(doc, child, in_link || is_link);
            chars += c;
            link_chars += l;
            links += n;
        }
        self.chars[node.0] = chars;
        self.link_chars[node.0] = link_chars;
        self.links[node.0] = links;
        (chars, link_chars, links)
    }

    fn chars(&self, node: NodeId) -> usize {
        self.chars[node.0]
    }

    /// Share of this subtree's text sitting inside a link, 0.0–1.0.
    fn link_density(&self, node: NodeId) -> f32 {
        match self.chars[node.0] {
            0 => 0.0,
            total => self.link_chars[node.0] as f32 / total as f32,
        }
    }

    /// Whether this subtree is a list of links rather than prose.
    fn is_link_list(&self, node: NodeId) -> bool {
        self.links[node.0] >= MIN_LINKS_FOR_DENSITY
            && self.link_density(node) >= LINK_DENSITY_LIMIT
            && self.chars[node.0] > 0
    }
}

/// Furniture identifiable without reading anything: navigation, landmarks, the
/// elements that exist to surround an article rather than to be one, and blocks
/// that turn out to be nothing but links.
fn structural_chrome(doc: &Document, body: NodeId, text: &TextMeasure) -> HashSet<NodeId> {
    let mut out = HashSet::new();
    let whole = text.chars(body);
    for node in doc.descendants(body) {
        if node == body {
            continue;
        }
        let Some(element) = doc.element(node) else {
            continue;
        };
        let name = element.local_name();
        let named = NAVIGATION_ELEMENTS.contains(&name)
            || NEVER_CONTENT.contains(&name)
            || element
                .attr("role")
                .is_some_and(|role| CHROME_ROLES.contains(&role.trim()))
            || (SURROUNDING_ELEMENTS.contains(&name) && !is_inside_article(doc, node));

        // A block of nothing but links is a menu whatever it is called and
        // whatever it is made of — a `<ul>`, a row of `<div>`s, a table cell in
        // a page from 1998. Bounded by how much of the page it is, so that a
        // page which is *entirely* a list of links — an index, a search result,
        // a link blog — is not deleted for being what it is.
        let link_list = text.is_link_list(node) && (whole == 0 || text.chars(node) * 2 < whole);

        if named || link_list {
            out.insert(node);
        }
    }
    out
}

fn is_inside_article(doc: &Document, node: NodeId) -> bool {
    doc.ancestors(node).any(|ancestor| {
        doc.element(ancestor)
            .is_some_and(|e| matches!(e.local_name(), "article" | "main"))
    })
}

fn is_under(doc: &Document, node: NodeId, ancestor: NodeId) -> bool {
    doc.ancestors(node).any(|a| a == ancestor)
}

fn is_chrome(doc: &Document, node: NodeId, chrome: &HashSet<NodeId>) -> bool {
    chrome.contains(&node) || doc.ancestors(node).any(|a| chrome.contains(&a))
}

/// The article a page was found to have.
struct Article {
    /// The container that scored highest: the tightest box around the prose.
    ///
    /// Kept because the siblings that belong with the article are *its*
    /// siblings. Taken from the climbed container instead, they would be the
    /// page's other top-level blocks — which is everything extraction exists to
    /// remove.
    best: NodeId,
    /// What to keep, after climbing to the article's own container.
    chosen: NodeId,
}

/// The best container of running prose, if the page has one worth trusting.
fn choose_article(
    doc: &Document,
    body: NodeId,
    text: &TextMeasure,
    scores: &HashMap<NodeId, f32>,
    chrome: &HashSet<NodeId>,
) -> Option<Article> {
    let (&best, _) = scores
        .iter()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .filter(|entry| *entry.1 > 0.0)?;

    // Climb while an ancestor brings almost nothing extra with it.
    let mut chosen = best;
    while chosen != body {
        let Some(parent) = doc.node(chosen).parent else {
            break;
        };
        let allowed = text.chars(chosen) as f32 * (1.0 + PARENT_TEXT_ALLOWANCE);
        if parent == body || text.chars(parent) as f32 > allowed {
            break;
        }
        chosen = parent;
    }

    // The safety net. A page whose article we cannot find is still a page, and
    // showing a fragment of it is worse than showing all of it.
    let kept: usize = kept_subtrees(doc, chosen, best, scores)
        .iter()
        .map(|&node| text.chars(node))
        .sum();
    // Measured against what would otherwise be *shown*, not against the whole
    // page: the navigation and the footer are going either way, and counting
    // them would refuse to extract from any page whose menus outweigh its
    // article — which is most of the pages this exists for.
    let whole = text.chars(body)
        - shallowest(doc, body, chrome)
            .iter()
            .map(|&node| text.chars(node))
            .sum::<usize>();
    let enough =
        kept >= MIN_ARTICLE_CHARS && whole > 0 && kept as f32 / whole as f32 >= MIN_ARTICLE_SHARE;
    enough.then_some(Article { best, chosen })
}

/// The chosen article, plus the siblings that belong with it.
///
/// An article split across sibling containers — a lead paragraph in one, the
/// body in the next — is ordinary markup, and keeping only the highest scorer
/// would drop half of it.
fn kept_subtrees(
    doc: &Document,
    chosen: NodeId,
    best: NodeId,
    scores: &HashMap<NodeId, f32>,
) -> Vec<NodeId> {
    let mut keep = vec![chosen];
    let Some(parent) = doc.node(best).parent else {
        return keep;
    };
    let top = scores.get(&best).copied().unwrap_or(0.0);
    for &sibling in doc.children(parent) {
        // Already inside what is being kept, which is the ordinary case once
        // the climb has moved up past them.
        if sibling == chosen || is_under(doc, sibling, chosen) {
            continue;
        }
        if scores.get(&sibling).copied().unwrap_or(0.0) >= top * SIBLING_SHARE {
            keep.push(sibling);
        }
    }
    keep
}

/// How much each candidate container looks like an article.
fn score(
    doc: &Document,
    body: NodeId,
    text: &TextMeasure,
    chrome: &HashSet<NodeId>,
) -> HashMap<NodeId, f32> {
    let mut scores: HashMap<NodeId, f32> = HashMap::new();

    for node in doc.descendants(body) {
        let Some(element) = doc.element(node) else {
            continue;
        };
        if !PARAGRAPH_ELEMENTS.contains(&element.local_name()) || is_chrome(doc, node, chrome) {
            continue;
        }
        let content = doc.text_content(node);
        let length = content.chars().filter(|c| !c.is_whitespace()).count();
        if length < MIN_PARAGRAPH_CHARS {
            continue;
        }
        // Commas stand in for sentence structure, which is what separates prose
        // from a stack of labels. Straight out of Readability, and it earns its
        // place: a menu has none.
        let commas = content.matches(',').count() as f32;
        let base = 1.0 + commas + (length as f32 / 100.0).min(MAX_LENGTH_SCORE);

        // Shared upwards with a falling weight, so a container gets credit for
        // the prose beneath it without a distant wrapper collecting all of it.
        for (depth, ancestor) in doc.ancestors(node).enumerate().take(SCORE_REACH) {
            if is_candidate(doc, ancestor) {
                *scores.entry(ancestor).or_insert(0.0) += base / (depth + 1) as f32;
            }
        }
    }

    for (&node, score) in scores.iter_mut() {
        *score *= 1.0 - text.link_density(node);
        *score *= naming_weight(doc, node);
        if is_chrome(doc, node, chrome) {
            *score = 0.0;
        }
    }
    scores
}

fn is_candidate(doc: &Document, node: NodeId) -> bool {
    doc.element(node)
        .is_some_and(|e| CANDIDATE_ELEMENTS.contains(&e.local_name()))
}

/// What a container's `class` and `id` say about it, as a multiplier.
///
/// Deliberately gentle in both directions. These names are a convention rather
/// than a contract, and the commonest of them appear on containers that are and
/// are not what they sound like.
fn naming_weight(doc: &Document, node: NodeId) -> f32 {
    let Some(element) = doc.element(node) else {
        return 1.0;
    };
    let mut names = String::new();
    if let Some(id) = element.id() {
        names.push_str(id);
        names.push(' ');
    }
    for class in element.classes() {
        names.push_str(class);
        names.push(' ');
    }
    names.make_ascii_lowercase();

    let furniture = FURNITURE_WORDS.iter().any(|word| names.contains(word));
    let content = CONTENT_WORDS.iter().any(|word| names.contains(word));
    match (furniture, content) {
        // Both is common — `main-sidebar`, `content-promo` — and says nothing
        // either way, as does neither.
        (true, true) | (false, false) => 1.0,
        (true, false) => 0.5,
        (false, true) => 1.25,
    }
}

/// Reduces a set of nodes to the shallowest of each branch.
///
/// Hiding a node hides its subtree, so naming a node and then also naming its
/// children is noise the caller would have to walk the document to apply.
fn shallowest(doc: &Document, body: NodeId, nodes: &HashSet<NodeId>) -> Vec<NodeId> {
    doc.descendants(body)
        .into_iter()
        .filter(|&node| nodes.contains(&node) && !doc.ancestors(node).any(|a| nodes.contains(&a)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text a reader would be left with, in order.
    fn kept(html: &str) -> String {
        let doc = dom::parse(html);
        let out = extract(&doc);
        let body = doc.find_element("body").expect("a body");
        let mut text = String::new();
        collect(&doc, body, &out.dropped, &mut text);
        text.trim().to_owned()
    }

    /// Whether an article was identified, as opposed to only chrome removed.
    fn found_article(html: &str) -> bool {
        extract(&dom::parse(html)).found_article
    }

    fn collect(doc: &Document, node: NodeId, dropped: &[NodeId], out: &mut String) {
        if dropped.contains(&node) {
            return;
        }
        if let Some(text) = doc.text(node) {
            let text = text.trim();
            if !text.is_empty() {
                out.push_str(text);
                out.push(' ');
            }
            return;
        }
        for &child in doc.children(node) {
            collect(doc, child, dropped, out);
        }
    }

    /// Six paragraphs of something that reads like prose: long enough to score,
    /// with the commas that tell a sentence from a label.
    fn prose() -> String {
        (1..=6)
            .map(|i| {
                format!(
                    "<p>Paragraph {i} of the article, which has commas in it, and runs on \
                     for a while so that it reads like prose rather than a label.</p>"
                )
            })
            .collect()
    }

    /// A menu of eight links, as a list.
    fn menu() -> String {
        (1..=8)
            .map(|i| format!(r#"<li><a href="/s{i}">Section {i}</a></li>"#))
            .collect()
    }

    #[test]
    fn the_navigation_and_the_furniture_go_and_the_article_stays() {
        // The whole point, on the shape of page that made it necessary: a
        // masthead, a nav, a sidebar of related links and a footer, all of
        // which land above and below the article once the author's layout is
        // discarded.
        let text = kept(&format!(
            r#"<body>
              <header class="site-header"><a href="/">MySite</a></header>
              <nav class="main-nav"><ul>{menu}</ul></nav>
              <div id="page">
                <main><article><h1>The headline</h1>{prose}</article></main>
                <aside class="sidebar"><h2>Related</h2><ul>{menu}</ul></aside>
              </div>
              <footer class="site-footer"><p>Copyright 2026 MySite, all rights reserved.</p></footer>
            </body>"#,
            menu = menu(),
            prose = prose(),
        ));

        assert!(text.starts_with("The headline"), "{text}");
        assert!(text.contains("Paragraph 6 of the article"), "{text}");
        for gone in ["MySite", "Section 1", "Related", "Copyright"] {
            assert!(!text.contains(gone), "{gone:?} survived in: {text}");
        }
    }

    #[test]
    fn the_wrappers_between_the_body_and_the_article_are_kept() {
        // The easiest thing in the world to get wrong: `#page` and `<main>` are
        // not the article, and dropping them for that reason takes the article
        // with them. This asserts against an extractor that says it found an
        // article and then shows an empty page.
        let text = kept(&format!(
            r#"<body><div id="page"><div class="col"><main><article>{}</article></main></div></div></body>"#,
            prose()
        ));
        assert!(text.contains("Paragraph 1 of the article"), "{text:?}");
    }

    #[test]
    fn a_page_that_is_a_list_of_links_is_left_whole() {
        // An index, a search result, a link blog. There is no article to find,
        // and stripping to whatever scored highest would delete the page the
        // reader asked for. Reader mode showing furniture is a poor rendering;
        // reader mode showing a fragment is a broken browser, and the reader
        // cannot tell the two apart.
        let index: String = (1..=30)
            .map(|i| {
                format!(r#"<li><a href="/p{i}">An interesting post about number {i}</a></li>"#)
            })
            .collect();
        let html = format!("<body><h1>Index</h1><ul>{index}</ul></body>");

        assert!(!found_article(&html), "an index is not an article");
        let text = kept(&html);
        assert!(text.contains("number 1"), "{text}");
        assert!(text.contains("number 30"), "the tail of the index went");
    }

    #[test]
    fn a_short_page_keeps_everything_but_its_navigation() {
        // Too little text to be confident about, so nothing is extracted — but
        // a `<nav>` says what it is, and goes whether or not an article was
        // found.
        let html = "<body><nav><a href=\"/a\">Elsewhere</a></nav><p>Just a short note.</p></body>";
        assert!(!found_article(html));
        assert_eq!(kept(html), "Just a short note.");
    }

    #[test]
    fn the_articles_own_header_survives_and_the_pages_does_not() {
        // `<header>` means two different things depending on where it is: the
        // masthead of a site, or the title block of the piece you are reading.
        let text = kept(&format!(
            r#"<body><header>Site name</header>
               <article><header><h1>Headline</h1><p class="byline">By A. Writer</p></header>{}</article>
               </body>"#,
            prose()
        ));
        assert!(
            text.contains("Headline"),
            "the article's header went: {text}"
        );
        assert!(text.contains("By A. Writer"), "the byline went: {text}");
        assert!(!text.contains("Site name"), "the masthead survived: {text}");
    }

    #[test]
    fn a_share_bar_inside_the_article_still_goes() {
        // Inside the article by every structural measure, and still not part of
        // it. Extraction that only cut at the article's edge would keep this.
        let text = kept(&format!(
            r#"<body><article>{}<div class="share"><a href="/t">Tweet</a><a href="/f">Share</a><a href="/l">Post</a></div></article></body>"#,
            prose()
        ));
        assert!(text.contains("Paragraph 1"), "{text}");
        assert!(!text.contains("Tweet"), "the share bar survived: {text}");
    }

    #[test]
    fn comments_below_the_article_go() {
        let comments: String = (1..=5)
            .map(|i| {
                format!(
                    r#"<div class="comment"><p>Commenter {i} here, saying something about the article above, at length.</p></div>"#
                )
            })
            .collect();
        let text = kept(&format!(
            r#"<body><article>{}</article><div id="comments">{comments}</div></body>"#,
            prose()
        ));
        assert!(text.contains("Paragraph 1"), "{text}");
        assert!(!text.contains("Commenter"), "the comments survived: {text}");
    }

    #[test]
    fn an_unnamed_block_of_nothing_but_links_is_a_menu() {
        // Most of the era's navigation, and a good deal of the modern web's,
        // carries no `<nav>`, no role and no telling class name. What it does
        // carry is links and nothing else.
        let text = kept(&format!(
            r#"<body><div><a href="/1">One</a> <a href="/2">Two</a> <a href="/3">Three</a> <a href="/4">Four</a></div><div>{}</div></body>"#,
            prose()
        ));
        assert!(text.contains("Paragraph 1"), "{text}");
        assert!(!text.contains("One"), "the unnamed menu survived: {text}");
    }

    #[test]
    fn one_link_in_a_paragraph_is_a_citation_not_a_menu() {
        // The other side of the link-density rule. A short paragraph that is
        // mostly a link is how a citation reads, and dropping those would take
        // sentences out of the middle of the article.
        let text = kept(&format!(
            r#"<body><article>{}<p><a href="/source">A rather long link to the source of all this</a>.</p></article></body>"#,
            prose()
        ));
        assert!(text.contains("the source of all this"), "{text}");
    }

    #[test]
    fn an_old_page_laid_out_in_a_table_keeps_the_content_cell() {
        // Table layout is the era this browser is for. The menu is a cell and
        // the article is a cell, and only one of them is worth reading.
        let text = kept(&format!(
            r#"<body><table><tr>
               <td class="menu"><a href="/1">One</a><br><a href="/2">Two</a><br><a href="/3">Three</a></td>
               <td class="content">{}</td>
               </tr></table></body>"#,
            prose()
        ));
        assert!(text.contains("Paragraph 1"), "{text}");
        assert!(!text.contains("One"), "the menu cell survived: {text}");
    }

    #[test]
    fn a_form_is_never_content() {
        let text = kept(&format!(
            r#"<body><form><input name="q"><button>Search</button></form><article>{}</article></body>"#,
            prose()
        ));
        assert!(text.contains("Paragraph 1"), "{text}");
        assert!(!text.contains("Search"), "the search form survived: {text}");
    }

    #[test]
    fn nothing_is_named_twice_or_beneath_something_already_named() {
        // Hiding a node hides its subtree, so a list that also names the
        // children is work the caller does for nothing.
        let doc = dom::parse(&format!(
            r#"<body><nav><ul>{}</ul></nav><article>{}</article></body>"#,
            menu(),
            prose()
        ));
        let out = extract(&doc);
        let mut seen = std::collections::HashSet::new();
        for &node in &out.dropped {
            assert!(seen.insert(node), "{node:?} named twice");
            assert!(
                !doc.ancestors(node).any(|a| out.dropped.contains(&a)),
                "{node:?} is beneath something already named"
            );
        }
    }

    #[test]
    fn a_document_with_no_body_is_left_alone() {
        // A fragment, or something that failed to parse into a page. There is
        // nothing to reason about and nothing to take away.
        let out = extract(&dom::parse(""));
        assert!(out.dropped.is_empty());
        assert!(!out.found_article);
    }

    #[test]
    fn an_article_too_small_a_share_of_its_page_is_not_believed() {
        // The scoring will always name a favourite. This is the page where it
        // should not be acted on: one paragraph of prose among a great deal of
        // other text that is not prose but is still the page — an index of
        // short entries, a table of results, a page of quotations. Extracting
        // here would show the reader a paragraph and throw the rest away.
        let entries: String = (1..=60)
            .map(|i| format!("<li>Entry {i}, briefly</li>"))
            .collect();
        let html = format!(
            "<body><ul>{entries}</ul><div><p>One paragraph of real prose, with commas \
             in it, sitting among a page that is mostly something else.</p></div></body>"
        );
        assert!(
            !found_article(&html),
            "a fragment was taken for the article"
        );
        let text = kept(&html);
        assert!(text.contains("Entry 60"), "the page went: {text}");
        assert!(text.contains("One paragraph of real prose"), "{text}");
    }

    #[test]
    fn a_long_enough_article_can_still_be_too_small_a_share_to_believe() {
        // The two halves of the net catch different pages, and this is the one
        // the length alone lets through: three full paragraphs, comfortably
        // past the character floor, and still a fifth of a page that is mostly
        // something else. Extraction here throws four fifths of the page away.
        let entries: String = (1..=50)
            .map(|i| format!("<li>Directory entry number {i} of the list</li>"))
            .collect();
        let article: String = (1..=3)
            .map(|i| {
                format!(
                    "<p>Paragraph {i}, which is genuine prose, with commas in it, and \
                     which runs on for long enough to be well past any floor on the \
                     length of an article.</p>"
                )
            })
            .collect();
        let html = format!("<body><ul>{entries}</ul><div>{article}</div></body>");

        assert!(!found_article(&html), "a fifth of a page was taken for it");
        let text = kept(&html);
        assert!(text.contains("Directory entry number 50"), "the page went");
        assert!(
            text.contains("Paragraph 1, which is genuine prose"),
            "{text}"
        );
    }

    #[test]
    fn an_article_of_almost_nothing_is_not_believed_either() {
        // The other half of the net, and not the same half: this page is
        // *mostly* its one paragraph, so the share is fine. There is simply too
        // little of it to be sure the paragraph is an article rather than a
        // caption, a cookie notice or an error message.
        let html = "<body><div><p>A single short paragraph, with a comma.</p></div></body>";
        assert!(!found_article(html), "two lines were taken for an article");
        assert_eq!(kept(html), "A single short paragraph, with a comma.");
    }

    #[test]
    fn the_menus_do_not_count_against_the_article_when_deciding_to_extract() {
        // The share is measured against what would otherwise be shown, not
        // against the whole page. A site whose navigation outweighs its article
        // — a long footer, a fat menu — is exactly the page this exists for,
        // and counting the menus would refuse to help on precisely those.
        let menus: String = (1..=40)
            .map(|i| format!(r#"<li><a href="/s{i}">Section number {i} of the site</a></li>"#))
            .collect();
        let html = format!(
            "<body><nav><ul>{menus}</ul></nav><article>{}</article>\
             <footer><ul>{menus}</ul></footer></body>",
            prose()
        );
        assert!(found_article(&html), "the menus outvoted the article");
        let text = kept(&html);
        assert!(text.contains("Paragraph 1 of the article"), "{text}");
        assert!(!text.contains("Section number 1"), "{text}");
    }

    #[test]
    fn a_byline_that_is_one_link_is_not_a_menu() {
        // Link density needs more than one link before it means anything. A
        // byline, a source credit and a "read more" are each a block that is
        // entirely a link, and none of them is navigation.
        let text = kept(&format!(
            r#"<body><article><h1>Headline</h1><div class="by"><a href="/authors/writer">A. Writer</a></div>{}</article></body>"#,
            prose()
        ));
        assert!(text.contains("A. Writer"), "the byline went: {text}");
    }

    #[test]
    fn links_inside_prose_do_not_make_it_navigation() {
        // The count alone is not enough either: an article with three citations
        // in it has three links and is still an article. It is the *share* of
        // the text that is a link that separates a menu from a paragraph.
        let text = kept(&format!(
            r#"<body><div class="story">{}<p>See <a href="/1">one</a>, <a href="/2">two</a> and <a href="/3">three</a> for more on all of this.</p></div></body>"#,
            prose()
        ));
        assert!(text.contains("Paragraph 1 of the article"), "{text}");
        assert!(text.contains("for more on all of this"), "{text}");
    }

    #[test]
    fn a_strip_of_short_captions_does_not_outscore_an_article() {
        // Why a paragraph has a minimum length. A photo essay's thumbnail strip
        // is thirty captions of three words; counted as prose they add up to
        // more than the piece they illustrate, and the reader gets the captions.
        let captions: String = (1..=50)
            .map(|i| format!("<figure><figcaption>Figure {i} of the set.</figcaption></figure>"))
            .collect();
        let text = kept(&format!(
            r#"<body><div class="strip">{captions}</div><div class="story">{}</div></body>"#,
            prose()
        ));
        assert!(
            text.contains("Paragraph 1 of the article"),
            "the captions won: {text}"
        );
        assert!(
            !text.contains("Figure 1 of the set"),
            "the strip survived: {text}"
        );
    }

    #[test]
    fn the_headline_above_the_prose_wrapper_is_taken_back() {
        // The tightest container around the paragraphs is usually a shade
        // inside the article, because the headline and the byline are not
        // prose and score nothing. Stopping there loses them.
        let text = kept(&format!(
            r#"<body><article><h1>Headline</h1><p class="byline">By A. Writer</p><div class="body">{}</div></article></body>"#,
            prose()
        ));
        assert!(text.contains("Headline"), "the headline went: {text}");
        assert!(text.contains("By A. Writer"), "the byline went: {text}");
    }

    #[test]
    fn the_climb_stops_before_a_neighbour_that_brings_the_page_with_it() {
        // The other side of the climb. Going up costs nothing when the parent
        // adds a headline; going up when the parent adds a column of something
        // else undoes the extraction entirely, and there is no signal in the
        // score to tell those apart — only in the size.
        let notes: String = (1..=30)
            .map(|i| format!("<li>Note number {i} in the list</li>"))
            .collect();
        let text = kept(&format!(
            r#"<body><div id="page"><div class="story">{}</div><div class="notes"><ul>{notes}</ul></div></div></body>"#,
            prose()
        ));
        assert!(text.contains("Paragraph 1 of the article"), "{text}");
        assert!(
            !text.contains("Note number 1"),
            "the climb took the neighbour: {text}"
        );
    }

    #[test]
    fn an_article_split_across_two_containers_keeps_both_halves() {
        // A standfirst in one box and the body in the next is ordinary markup,
        // and keeping only whichever scored higher drops half the piece.
        let half: String = (1..=3)
            .map(|i| {
                format!(
                    "<p>Second half, paragraph {i}, which also has commas in it and \
                     runs on for long enough to read as prose.</p>"
                )
            })
            .collect();
        let text = kept(&format!(
            r#"<body><div id="page"><div class="lead">{}</div><div class="rest">{half}</div></div></body>"#,
            prose()
        ));
        assert!(text.contains("Paragraph 1 of the article"), "{text}");
        assert!(text.contains("Second half, paragraph 3"), "{text}");
    }

    #[test]
    fn prose_beats_a_stack_of_labels_of_the_same_length() {
        // The comma term, on its own. It is a tie-breaker rather than a page
        // shape: two containers holding the same quantity of text, one of it
        // written in sentences and the other a column of labels, and the
        // sentences have to win — and the labels are deliberately the *longer*
        // of the two, because otherwise length alone would decide and the test
        // would pass without the term it exists for. Length is the one thing a
        // wall of boilerplate always has.
        let labels: String = (1..=6)
            .map(|i| {
                format!(
                    "<p>Category {i} archive listing page for the whole of this site \
                     including everything ever published anywhere within it</p>"
                )
            })
            .collect();
        let sentences: String = (1..=6)
            .map(|i| {
                format!(
                    "<p>Sentence {i}, which is written in prose, has commas in it, and \
                     reads.</p>"
                )
            })
            .collect();
        let text = kept(&format!(
            "<body><div><div class=\"a\">{labels}</div></div>\
             <div><div class=\"b\">{sentences}</div></div></body>"
        ));
        assert!(text.contains("Sentence 1"), "the prose lost: {text}");
        assert!(!text.contains("Category 1"), "the labels survived: {text}");
    }

    #[test]
    fn a_furniture_name_weighs_against_a_container_without_deciding_it() {
        // The class names are a convention, not a contract. A container called
        // `promo` that is nevertheless the only prose on the page has to win,
        // or a page loses its article to a naming choice.
        let text = kept(&format!(
            r#"<body><div class="promo">{}</div></body>"#,
            prose()
        ));
        assert!(text.contains("Paragraph 1"), "{text}");
    }
}
