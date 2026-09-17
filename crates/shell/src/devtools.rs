//! What this browser can tell you about the page it is showing (#198).
//!
//! Four things were asked for — console, inspector, network, storage — and this
//! is all four, as one generated page rather than four panels. A page for the
//! reason the saved list and the history are pages: the engine already knows
//! how to show a document, and a devtools *window* would be a second piece of
//! interface with its own scrolling, its own hit-testing and its own bugs.
//!
//! Two of the four mean something different here than they do elsewhere, and
//! saying so is most of the job:
//!
//! **The console has nothing to listen to.** ADR-0003 means no script runs, so
//! nothing on the page can log anything. What this shows instead is the
//! *browser's* own account of the page — why it was laid out the way it was,
//! what it refused, what did not arrive. Those are the messages somebody
//! actually opens a console to find.
//!
//! **Storage is empty by construction.** There is no `localStorage` without
//! scripts, cookies are not implemented, and ADR-0018's cache lives in memory
//! for one run. The three files this browser keeps are not per-site state at
//! all, so the section names them and says what they are rather than drawing an
//! empty table that looks like a missing feature.
//!
//! The inspector is built from the accessibility tree, which ADR-0019 already
//! sends across the process boundary as data. That is not a substitute for a
//! DOM inspector — it is a *better* answer to the question usually being asked,
//! "what did this page actually turn into?", because it is the structure the
//! page produced rather than the markup it was written in. The markup is one
//! menu entry away, unchanged, in [`source_page`].

use std::path::Path;

use sandbox::access::{Role, Tree};

/// Everything the page below is drawn from.
///
/// A plain description rather than a handle on the browser: this module renders
/// and nothing else, so it can be tested without a window, a renderer or a
/// network.
pub struct Report<'a> {
    /// The address, as the bar shows it.
    pub url: &'a str,
    /// What the document was served as, when anything said.
    pub content_type: Option<&'a str>,
    /// How many bytes of document arrived.
    pub bytes: usize,
    /// What the server answered with (#203).
    pub status: u16,
    /// Whether the certificate chain needed this computer's own roots
    /// (ADR-0015).
    pub local_root: bool,
    /// How the page was laid out (ADR-0009).
    pub mode: layout::RenderMode,
    /// Why, when the classification had something to say.
    pub explanation: Option<String>,
    /// What went wrong with the navigation, if anything.
    pub error: Option<&'a str>,
    /// How many images arrived.
    pub images_loaded: u32,
    /// The subresources the policy refused (ADR-0006).
    pub withheld: Vec<String>,
    /// The hosts those were on.
    pub withheld_hosts: Vec<String>,
    /// What the page turned into.
    pub tree: Tree,
    /// The files this browser keeps, and how much is in each.
    pub stores: Vec<Store>,
}

/// One of the files that outlive a run.
pub struct Store {
    /// What it holds.
    pub what: &'static str,
    /// Where it is.
    pub path: std::path::PathBuf,
    /// How many entries, as its own module counts them.
    pub entries: usize,
    /// Which ADR decided it should exist.
    pub decided_by: &'static str,
}

impl Store {
    /// Reads one, counting whatever its own module counts.
    pub fn of(
        what: &'static str,
        path: std::path::PathBuf,
        entries: usize,
        adr: &'static str,
    ) -> Self {
        Self {
            what,
            path,
            entries,
            decided_by: adr,
        }
    }
}

/// The page itself.
pub fn page(report: &Report<'_>) -> String {
    let mut html = String::from(
        // The charset is not decoration: this is written to disk and loaded
        // like any other file, and a document that declares nothing is
        // windows-1252 — so a title with an em dash in it would come back as
        // mojibake through the browser's own front door.
        "<!doctype html>\n<meta charset=\"utf-8\">\n<title>Page information</title>\n\
         <body style=\"font-family: sans-serif; margin: 2em; max-width: 52em\">\n",
    );
    html.push_str("<h1>Page information</h1>\n<p><small>");
    html.push_str(&escape(report.url));
    html.push_str("</small></p>\n");

    console(&mut html, report);
    network(&mut html, report);
    inspector(&mut html, report);
    storage(&mut html, report);
    html
}

/// The browser's own account of this page.
fn console(html: &mut String, report: &Report<'_>) {
    html.push_str("<h2>Console</h2>\n");
    html.push_str(
        "<p><small>No script runs here (ADR-0003), so nothing on the page can \
         log anything. These are the browser's own messages about it.</small></p>\n<ul>\n",
    );
    let mut said = false;
    let line = |html: &mut String, text: String| {
        html.push_str("<li>");
        html.push_str(&text);
        html.push_str("</li>\n");
    };
    if let Some(error) = report.error {
        line(html, format!("<b>Failed:</b> {}", escape(error)));
        said = true;
    }
    if let Some(explanation) = &report.explanation {
        line(html, escape(explanation));
        said = true;
    }
    if report.local_root {
        line(
            html,
            "This page's certificate verified only against this computer's own \
             roots, not against Mozilla's (ADR-0015)."
                .to_owned(),
        );
        said = true;
    }
    if !report.withheld.is_empty() {
        line(
            html,
            format!(
                "{} subresource(s) refused as third party, from {} other site(s) \
                 (ADR-0006). The padlock in the bar is where they are allowed.",
                report.withheld.len(),
                report.withheld_hosts.len()
            ),
        );
        said = true;
    }
    if !said {
        line(
            html,
            "Nothing to report: the page loaded, nothing was refused, and \
             nothing about its layout needed deciding."
                .to_owned(),
        );
    }
    html.push_str("</ul>\n");
}

/// What this page asked the network for.
fn network(html: &mut String, report: &Report<'_>) {
    html.push_str("<h2>Network</h2>\n<table border=\"1\" cellpadding=\"4\" cellspacing=\"0\">\n");
    row(html, "Document", &escape(report.url));
    row(
        html,
        "Served as",
        &escape(report.content_type.unwrap_or("nothing said")),
    );
    row(html, "Status", &report.status.to_string());
    row(html, "Bytes", &report.bytes.to_string());
    row(
        html,
        "Laid out as",
        match report.mode {
            layout::RenderMode::Authored => "the author wrote it",
            // Both fallbacks, because the difference between them is *why*,
            // and the why is the explanation in the console above.
            _ => "a document, rather than as written (ADR-0009)",
        },
    );
    row(html, "Images loaded", &report.images_loaded.to_string());
    row(
        html,
        "Certificate",
        if report.local_root {
            "verified against this computer's roots (ADR-0015)"
        } else {
            "verified, or not encrypted — the bar says which"
        },
    );
    html.push_str("</table>\n");

    if report.withheld.is_empty() {
        html.push_str("<p>Nothing was refused.</p>\n");
        return;
    }
    html.push_str("<h3>Refused as third party (ADR-0006)</h3>\n<ul>\n");
    for url in &report.withheld {
        html.push_str("<li><small>");
        html.push_str(&escape(url));
        html.push_str("</small></li>\n");
    }
    html.push_str("</ul>\n");
}

/// What the page turned into.
fn inspector(html: &mut String, report: &Report<'_>) {
    html.push_str("<h2>Inspector</h2>\n");
    html.push_str(
        "<p><small>The structure the page produced, from the tree this browser \
         hands to a screen reader (ADR-0019) — what it <i>became</i>, rather \
         than the markup it was written in. The markup is under \
         <i>View page source</i>.</small></p>\n",
    );
    if report.tree.nodes.is_empty() {
        html.push_str("<p>Nothing to show: this page produced no boxes.</p>\n");
        return;
    }
    html.push_str("<pre style=\"font-size: 0.9em\">\n");
    // Pre-order with a child count per node, which is how the tree crosses the
    // boundary. Depth comes back by keeping a stack of how many children are
    // still owed at each level.
    let mut owed: Vec<u32> = Vec::new();
    for node in &report.tree.nodes {
        let depth = owed.len();
        html.push_str(&"  ".repeat(depth));
        html.push_str(&escape(describe(&node.role)));
        if node.level > 0 {
            html.push_str(&format!(" {}", node.level));
        }
        if !node.name.is_empty() {
            html.push_str(&format!("  “{}”", escape(&elide(&node.name))));
        }
        if let Some(value) = &node.value {
            html.push_str(&format!("  → {}", escape(&elide(value))));
        }
        html.push_str(&format!(
            "  <small>{:.0}×{:.0} at {:.0},{:.0}</small>\n",
            node.rect.width, node.rect.height, node.rect.x, node.rect.y
        ));

        // A node with children opens a level; a leaf fills one slot of its
        // parent and closes however many ancestors that finishes off. Doing
        // these the other way round closes a parent *before* its children are
        // printed, and the whole tree comes out flat one level down.
        if node.children > 0 {
            owed.push(node.children);
            continue;
        }
        while let Some(left) = owed.last_mut() {
            *left -= 1;
            if *left > 0 {
                break;
            }
            owed.pop();
        }
    }
    html.push_str("</pre>\n");
}

/// What this browser keeps, which is not what a storage panel usually shows.
fn storage(html: &mut String, report: &Report<'_>) {
    html.push_str("<h2>Storage</h2>\n");
    html.push_str(
        "<p><small>This page has none, and neither has any other. There is no \
         <code>localStorage</code> without scripts (ADR-0003), cookies are not \
         implemented, and the cache lives in memory for one run (ADR-0018). \
         Nothing on the web can leave anything on this machine.</small></p>\n",
    );
    html.push_str(
        "<p>What the browser itself keeps, all of it plain text you can read, \
         edit or delete:</p>\n<table border=\"1\" cellpadding=\"4\" cellspacing=\"0\">\n",
    );
    for store in &report.stores {
        html.push_str(&format!(
            "<tr><td>{}</td><td align=\"right\">{}</td><td><small>{}</small></td>\
             <td><small>{}</small></td></tr>\n",
            escape(store.what),
            store.entries,
            escape(&store.path.display().to_string()),
            escape(store.decided_by),
        ));
    }
    html.push_str("</table>\n");
}

/// The page's own markup, as a page (#198).
///
/// The bytes the parent already holds, decoded the way the document itself was
/// decoded, so what is shown is what was parsed rather than a second guess at
/// the encoding. Escaped into a `<pre>`, which is the whole of "view source"
/// and is why it needs no engine work at all.
pub fn source_page(url: &str, bytes: &[u8], content_type: Option<&str>) -> String {
    let (text, encoding, _) = net::encoding::decode_document(bytes, content_type);
    format!(
        "<!doctype html>\n<meta charset=\"utf-8\">\n<title>Source of {name}</title>\n\
         <body style=\"margin: 2em\">\n\
         <p style=\"font-family: sans-serif\"><small>{name}<br>\
         {count} bytes, read as {encoding}</small></p>\n\
         <pre style=\"white-space: pre-wrap\">{source}</pre>\n",
        name = escape(url),
        count = bytes.len(),
        encoding = escape(encoding.name()),
        source = escape(&text),
    )
}

/// One label-and-value row.
fn row(html: &mut String, label: &str, value: &str) {
    html.push_str(&format!(
        "<tr><td>{}</td><td>{}</td></tr>\n",
        escape(label),
        value
    ));
}

/// A role in words.
fn describe(role: &Role) -> &'static str {
    match role {
        Role::Document => "document",
        Role::Heading => "heading",
        Role::Paragraph => "paragraph",
        Role::Link => "link",
        Role::Image => "image",
        Role::List => "list",
        Role::ListItem => "list item",
        Role::Table => "table",
        Role::Row => "row",
        Role::Cell => "cell",
        Role::HeaderCell => "header cell",
        Role::TextField => "text field",
        Role::Button => "button",
        Role::CheckBox => "checkbox",
        Role::RadioButton => "radio",
        Role::ComboBox => "dropdown",
        Role::Separator => "separator",
        Role::Group => "group",
        Role::Text => "text",
    }
}

/// Shortens a long name to something a tree can hold on one line.
fn elide(text: &str) -> String {
    const MOST: usize = 60;
    let flattened: String = text
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    if flattened.chars().count() <= MOST {
        return flattened;
    }
    let kept: String = flattened.chars().take(MOST).collect();
    format!("{kept}…")
}

/// Escapes text for HTML.
///
/// Everything on this page came from somewhere untrusted — a title, a URL, an
/// error message quoting a server. A name containing `</pre>` would otherwise
/// rewrite the page around it.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
    out
}

/// Where the generated page is written, beside the browser's other views.
pub fn page_path() -> std::path::PathBuf {
    crate::bookmarks::default_path().with_file_name("page-info.html")
}

/// And the source view, likewise.
pub fn source_path() -> std::path::PathBuf {
    crate::bookmarks::default_path().with_file_name("source.html")
}

/// Whether `path` is one of this module's own generated pages.
///
/// Asked so that opening one does not record it in the history, and so that
/// asking for information *about* the information page says something useful.
pub fn is_generated(path: &Path) -> bool {
    path == page_path() || path == source_path()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sandbox::access::Node;

    fn node(role: Role, name: &str, children: u32) -> Node {
        Node {
            role,
            name: name.to_owned(),
            value: None,
            rect: layout::Rect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
            level: 0,
            on: false,
            span: (1, 1),
            children,
        }
    }

    fn report<'a>(withheld: Vec<String>, tree: Tree) -> Report<'a> {
        Report {
            url: "https://example.com/",
            content_type: Some("text/html"),
            bytes: 1234,
            status: 200,
            local_root: false,
            mode: layout::RenderMode::Authored,
            explanation: None,
            error: None,
            images_loaded: 2,
            withheld_hosts: withheld
                .iter()
                .filter_map(|url| net::parse_url(url).ok().map(|(origin, _)| origin.host))
                .collect(),
            withheld,
            tree,
            stores: vec![Store::of("Bookmarks", "/tmp/bookmarks.tsv".into(), 3, "§1")],
        }
    }

    #[test]
    fn all_four_sections_are_there() {
        // The issue named four and they are the contract: a page missing one is
        // a tool that quietly does not exist.
        let html = page(&report(Vec::new(), Tree::default()));
        for heading in ["Console", "Network", "Inspector", "Storage"] {
            assert!(html.contains(&format!("<h2>{heading}</h2>")), "{heading}");
        }
    }

    #[test]
    fn the_console_says_what_was_refused() {
        let html = page(&report(
            vec!["https://tracker.example.net/pixel.gif".to_owned()],
            Tree::default(),
        ));
        assert!(html.contains("1 subresource(s) refused"), "{html}");
        assert!(html.contains("tracker.example.net/pixel.gif"), "{html}");
    }

    #[test]
    fn a_quiet_page_says_so_rather_than_showing_an_empty_list() {
        // An empty console reads as a broken console.
        let html = page(&report(Vec::new(), Tree::default()));
        assert!(html.contains("Nothing to report"), "{html}");
        assert!(html.contains("Nothing was refused"), "{html}");
    }

    #[test]
    fn the_inspector_nests_by_the_child_counts_it_was_given() {
        // The tree crosses as pre-order plus a child count, so depth has to be
        // rebuilt here. Getting it wrong flattens the page or runs off the end.
        let tree = Tree {
            nodes: vec![
                node(Role::Document, "", 2),
                node(Role::Heading, "Title", 0),
                node(Role::List, "", 2),
                node(Role::ListItem, "one", 0),
                node(Role::ListItem, "two", 0),
            ],
            focus: None,
        };
        let html = page(&report(Vec::new(), tree));
        let listed: Vec<&str> = html
            .lines()
            .filter(|line| line.contains("“") || line.starts_with("document"))
            .collect();
        assert!(
            listed.iter().any(|line| line.starts_with("  heading")),
            "a heading one level in: {listed:?}"
        );
        assert!(
            listed.iter().any(|line| line.starts_with("    list item")),
            "an item two levels in: {listed:?}"
        );
    }

    #[test]
    fn storage_says_there_is_none_rather_than_drawing_an_empty_table() {
        let html = page(&report(Vec::new(), Tree::default()));
        assert!(html.contains("This page has none"), "{html}");
        assert!(
            html.contains("Bookmarks"),
            "the browser's own files: {html}"
        );
    }

    #[test]
    fn a_page_cannot_rewrite_the_information_page_about_it() {
        let mut about = report(Vec::new(), Tree::default());
        about.url = "https://example.com/?a=1&b=2";
        about.error = Some("</body><script>alert(1)</script>");
        let html = page(&about);
        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&amp;b=2"), "{html}");
    }

    #[test]
    fn the_source_view_shows_the_markup_it_parsed() {
        let html = source_page(
            "https://example.com/",
            b"<html><body>hello &amp; goodbye</body></html>",
            Some("text/html; charset=utf-8"),
        );
        assert!(html.contains("&lt;html&gt;&lt;body&gt;hello"), "{html}");
        assert!(html.contains("45 bytes"), "{html}");
        assert!(
            html.contains("UTF-8"),
            "the encoding it was read as: {html}"
        );
    }

    #[test]
    fn the_generated_pages_are_recognised() {
        assert!(is_generated(&page_path()));
        assert!(is_generated(&source_path()));
        assert!(!is_generated(Path::new("/home/reader/index.html")));
    }
}
