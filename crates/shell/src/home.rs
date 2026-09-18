//! Somewhere to start from when no address was given.
//!
//! `2kbrowser open` used to answer "no input given" and exit, which is correct
//! and useless: a browser opened with no argument is a browser somebody wants
//! to *use*, and the one thing it should not do is refuse. Every other browser
//! has an answer to this, and most of them make it an advertisement.
//!
//! This one is built out of what the browser already knows — the pages you
//! saved and the pages you went to — because that is genuinely the most likely
//! place you want to go, and because it costs no network request to find out.
//! There is nothing to configure and nothing fetched: a home page that phoned
//! somewhere on startup would be at odds with ADR-0006, which refuses
//! third-party requests on a page you *asked* for.
//!
//! A generated document rather than a panel, for the reason the history and
//! bookmark views give: this browser already knows how to show a page with
//! links on it, and a second piece of interface would have its own scrolling,
//! its own hit-testing and its own bugs.

use std::path::PathBuf;

use crate::bookmarks::Bookmarks;
use crate::visits::Visits;

/// How many recent addresses the page offers.
///
/// Enough to find yesterday's work in, short enough to read at a glance. The
/// whole list is a keystroke away (Ctrl+H) and says so.
const RECENT: usize = 12;

/// Where the home page is written.
///
/// Beside the other generated pages, and written rather than held in memory
/// because the renderer child loads a *document from a URL* — that is the one
/// path the engine has, and inventing a second one so this page could skip the
/// disk would be a second path to keep working.
pub fn path() -> PathBuf {
    crate::bookmarks::default_path().with_file_name("home.html")
}

/// The page itself.
pub fn page(bookmarks: &Bookmarks, visits: &Visits) -> String {
    let mut html = String::from(
        // The charset is not decoration: this is written to disk and loaded
        // like any other file, and a document that declares nothing is
        // windows-1252 — so a saved title with an em dash in it would come
        // back as mojibake.
        "<!doctype html>\n<meta charset=\"utf-8\">\n<title>2kbrowser</title>\n\
         <body style=\"font-family: sans-serif; margin: 2em auto; max-width: 44em\">\n\
         <h1 style=\"font-size: 1.6em\">2kbrowser</h1>\n",
    );

    let empty = bookmarks.is_empty() && visits.is_empty();
    if empty {
        // A first run. Saying "nothing here yet" under two empty headings
        // would be three ways of saying the same thing, so this says it once
        // and then says what to do about it.
        html.push_str(
            "<p>Nothing saved and nowhere visited yet. Type an address in the bar \
             above, or open a file.</p>\n",
        );
    }

    if !bookmarks.is_empty() {
        html.push_str("<h2 style=\"font-size: 1.1em\">Saved</h2>\n<ul>\n");
        for mark in bookmarks.iter() {
            list_item(&mut html, &mark.url, &mark.title, None);
        }
        html.push_str("</ul>\n");
    }

    if !visits.is_empty() {
        html.push_str("<h2 style=\"font-size: 1.1em\">Recent</h2>\n<ul>\n");
        // Newest first, which is the order a person looks for something in.
        for visit in visits.iter().rev().take(RECENT) {
            list_item(&mut html, &visit.url, &visit.title, Some(&visit.when));
        }
        html.push_str("</ul>\n");
        if visits.len() > RECENT {
            html.push_str(&format!(
                "<p><small>{} more in the full history (Ctrl+H).</small></p>\n",
                visits.len() - RECENT
            ));
        }
    }

    // Last, and only where the page would otherwise be a wall of links with no
    // hint of what the window can do. A first run gets it because there is
    // nothing else on the page; a reader with a history has already found
    // these and does not need them repeated every time they open the browser.
    if empty {
        html.push_str(
            "<h2 style=\"font-size: 1.1em\">Getting around</h2>\n\
             <ul>\n\
             <li>Ctrl+L types an address, Ctrl+T opens a tab.</li>\n\
             <li>Ctrl+F searches the page, Ctrl+D saves it.</li>\n\
             <li>Ctrl+H is everywhere you have been.</li>\n\
             <li>Third-party requests are refused and JavaScript is never run. \
             The padlock says what a page asked for.</li>\n\
             </ul>\n",
        );
    }
    html
}

/// One entry: a link, its address under it, and when it was, if known.
///
/// The title is what a reader recognises and the address is what tells them
/// which of three similar titles this is — so both, rather than a choice
/// between them. A saved page with no title has only its address, and printing
/// an empty link would be a row that could not be clicked.
fn list_item(html: &mut String, url: &str, title: &str, when: Option<&str>) {
    html.push_str("<li><a href=\"");
    html.push_str(&escape(url));
    html.push_str("\">");
    if title.trim().is_empty() {
        html.push_str(&escape(url));
        html.push_str("</a>");
    } else {
        html.push_str(&escape(title));
        html.push_str("</a><br><small>");
        html.push_str(&escape(url));
        html.push_str("</small>");
    }
    if let Some(when) = when {
        html.push_str("<br><small>");
        html.push_str(&escape(when));
        html.push_str("</small>");
    }
    html.push_str("</li>\n");
}

/// Escapes text for the page above.
///
/// The addresses and titles here came off disk, and what wrote them there was a
/// page from the open internet: a title is whatever a `<title>` said. A page
/// the browser writes about a stranger's text must not be rewritten by it.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn visits_of(entries: &[(&str, &str)]) -> Visits {
        let mut visits = Visits::default();
        for (url, title) in entries {
            visits.record(*url, *title, "2026-01-01 00:00".to_owned());
        }
        visits
    }

    #[test]
    fn a_first_run_says_what_to_do_rather_than_nothing() {
        let page = page(&Bookmarks::default(), &Visits::default());
        assert!(page.contains("Type an address"), "{page}");
        // And the keys, which are the one thing a reader with no history has no
        // other way to discover.
        assert!(page.contains("Ctrl+L"), "{page}");
    }

    #[test]
    fn saved_pages_and_recent_ones_are_both_offered() {
        let mut marks = Bookmarks::default();
        marks.add("https://example.com/saved", "A saved page");
        let visits = visits_of(&[("https://example.com/went", "Somewhere I went")]);

        let page = page(&marks, &visits);
        assert!(page.contains("https://example.com/saved"), "{page}");
        assert!(page.contains("A saved page"), "{page}");
        assert!(page.contains("https://example.com/went"), "{page}");
        assert!(page.contains("Somewhere I went"), "{page}");
    }

    #[test]
    fn the_keys_are_not_repeated_to_somebody_who_has_used_the_browser() {
        // A list of shortcuts on every startup is a list nobody reads. It is
        // there for the run where there is nothing else on the page.
        let visits = visits_of(&[("https://example.com/", "Example")]);
        let page = page(&Bookmarks::default(), &visits);
        assert!(!page.contains("Getting around"), "{page}");
    }

    #[test]
    fn the_recent_list_is_bounded_and_says_how_much_it_left_out() {
        let entries: Vec<(String, String)> = (0..RECENT + 5)
            .map(|n| (format!("https://example.com/{n}"), format!("Page {n}")))
            .collect();
        let borrowed: Vec<(&str, &str)> = entries
            .iter()
            .map(|(url, title)| (url.as_str(), title.as_str()))
            .collect();
        let page = page(&Bookmarks::default(), &visits_of(&borrowed));

        // The newest are there and the oldest are not.
        assert!(page.contains("Page 16"), "{page}");
        assert!(!page.contains("\">Page 0<"), "{page}");
        assert!(page.contains("5 more in the full history"), "{page}");
    }

    #[test]
    fn a_title_from_a_stranger_cannot_rewrite_the_page_about_it() {
        // A title is whatever a `<title>` element said, and it reached the
        // history file from the open internet.
        let visits = visits_of(&[("https://example.com/", "<script>alert(1)</script>")]);
        let page = page(&Bookmarks::default(), &visits);
        assert!(!page.contains("<script>"), "{page}");
        assert!(page.contains("&lt;script&gt;"), "{page}");
    }

    #[test]
    fn an_address_with_no_title_is_still_a_link() {
        let visits = visits_of(&[("https://example.com/untitled", "")]);
        let page = page(&Bookmarks::default(), &visits);
        assert!(
            page.contains("\"https://example.com/untitled\">https://example.com/untitled</a>"),
            "an untitled page became a link with nothing to click: {page}"
        );
    }
}
