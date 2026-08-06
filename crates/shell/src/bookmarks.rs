//! Saved pages.
//!
//! A tab-separated file, one bookmark per line, in the user's config
//! directory. Not a database and not a format anyone needs a tool to read: the
//! whole list is a few kilobytes, and someone who wants to edit it in a text
//! editor or grep it should be able to.
//!
//! The file is the only state this browser keeps between runs. That is worth
//! saying plainly, because "no account, no sync, no profile" is part of the
//! product (§1) and a browser that quietly accumulated more would be walking
//! it back.

use std::path::{Path, PathBuf};

/// One saved page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    /// Where it points.
    pub url: String,
    /// What to call it. The page's title when it had one.
    pub title: String,
}

/// The saved list.
#[derive(Debug, Clone, Default)]
pub struct Bookmarks {
    entries: Vec<Bookmark>,
}

impl Bookmarks {
    /// Every bookmark, in the order they were added.
    pub fn iter(&self) -> impl Iterator<Item = &Bookmark> {
        self.entries.iter()
    }

    /// How many there are.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether `url` is saved.
    pub fn contains(&self, url: &str) -> bool {
        self.entries.iter().any(|entry| entry.url == url)
    }

    /// Saves a page, or updates its title if it is already saved.
    ///
    /// Updating rather than duplicating: bookmarking a page twice is a thing
    /// people do by accident, and two entries for one URL is never what they
    /// meant.
    pub fn add(&mut self, url: impl Into<String>, title: impl Into<String>) {
        let url = url.into();
        let title = clean(&title.into());
        match self.entries.iter_mut().find(|entry| entry.url == url) {
            Some(entry) => entry.title = title,
            None => self.entries.push(Bookmark { url, title }),
        }
    }

    /// Forgets a page. Returns whether it was there.
    pub fn remove(&mut self, url: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.url != url);
        self.entries.len() != before
    }

    /// Saves the page if it is not saved, forgets it if it is.
    ///
    /// Returns whether it is saved afterwards, which is what the caller needs
    /// to say so.
    pub fn toggle(&mut self, url: &str, title: &str) -> bool {
        if self.remove(url) {
            false
        } else {
            self.add(url, title);
            true
        }
    }

    /// Parses the file format.
    ///
    /// Malformed lines are skipped rather than failing the load: a corrupt
    /// line should cost one bookmark, not all of them.
    pub fn parse(text: &str) -> Self {
        let entries = text
            .lines()
            .filter_map(|line| {
                if line.trim().is_empty() || line.trim_start().starts_with('#') {
                    return None;
                }
                // Split before trimming: a line *starting* with a tab has an
                // empty URL field, and trimming first would promote its title
                // into the URL.
                let (url, title) = match line.split_once('\t') {
                    Some((url, title)) => (url.trim(), title.trim()),
                    None => (line.trim(), ""),
                };
                // A URL has no whitespace in it. Anything that does is a
                // mangled line rather than a bookmark.
                let usable = !url.is_empty() && !url.contains(char::is_whitespace);
                usable.then(|| Bookmark {
                    url: url.to_owned(),
                    title: title.to_owned(),
                })
            })
            .collect();
        Self { entries }
    }

    /// Renders the file format.
    pub fn to_text(&self) -> String {
        let mut out = String::from(
            "# 2kbrowser bookmarks: one per line, URL and title separated by a tab.\n",
        );
        for entry in &self.entries {
            out.push_str(&entry.url);
            if !entry.title.is_empty() {
                out.push('\t');
                out.push_str(&entry.title);
            }
            out.push('\n');
        }
        out
    }

    /// Reads the list from `path`. A missing file is an empty list, not an
    /// error: not having bookmarked anything yet is the normal state.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    /// Writes the list to `path`, creating the directory if needed.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.to_text())
    }
}

/// Renders the saved list as a page.
///
/// A page rather than a panel: this browser already knows how to show a
/// document with links in it, and a bookmarks *window* would be a second piece
/// of interface with its own scrolling, its own hit-testing and its own bugs.
/// The list is HTML, so it is read by the engine the rest of the browser is.
pub fn page(marks: &Bookmarks) -> String {
    // The charset is not decoration. This page is written to disk and then
    // loaded like any other file, and a document that declares nothing is
    // windows-1252 — so a title with an em dash in it would come back as
    // mojibake through the browser's own front door.
    let mut html = String::from(
        "<!doctype html>\n<meta charset=\"utf-8\">\n<title>Saved pages</title>\n\
         <body style=\"font-family: sans-serif; margin: 2em; max-width: 44em\">\n\
         <h1>Saved pages</h1>\n",
    );
    if marks.is_empty() {
        html.push_str("<p>Nothing saved yet. Press Ctrl+D on a page to save it.</p>\n");
        return html;
    }
    html.push_str("<ul>\n");
    for entry in marks.iter() {
        let url = escape(&entry.url);
        html.push_str(&format!("<li><a href=\"{url}\">"));
        if entry.title.is_empty() {
            // Nothing else to call it, and printing the URL twice would say the
            // same thing in two sizes.
            html.push_str(&url);
            html.push_str("</a></li>\n");
        } else {
            html.push_str(&escape(&entry.title));
            html.push_str(&format!("</a><br><small>{url}</small></li>\n"));
        }
    }
    html.push_str("</ul>\n");
    html
}

/// Where the rendered list is written.
///
/// Beside the bookmarks file, and regenerated every time it is opened: it is a
/// view of the list rather than a second copy of it.
pub fn page_path() -> PathBuf {
    default_path().with_file_name("bookmarks.html")
}

/// Escapes text for HTML.
///
/// A title is whatever an author wrote, and a URL can contain an ampersand.
/// Neither is trusted enough to interpolate raw — a title containing `</a>`
/// would otherwise rewrite the page around it.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Strips what the file format cannot carry.
///
/// A title with a tab or a newline in it would split its own line, so a
/// bookmark could silently become two — or one with a URL of nonsense.
fn clean(title: &str) -> String {
    title
        .chars()
        .map(|c| {
            if c == '\t' || c == '\n' || c == '\r' {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Where the bookmarks file lives.
///
/// The platform's config directory, resolved by hand rather than by pulling in
/// a crate for four lines of `std::env` (ADR-0007).
pub fn default_path() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    };
    base.unwrap_or_else(|| PathBuf::from("."))
        .join("2kbrowser")
        .join("bookmarks.tsv")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adding_and_removing() {
        let mut marks = Bookmarks::default();
        assert!(marks.is_empty());
        marks.add("https://example.com/", "Example");
        assert!(marks.contains("https://example.com/"));
        assert_eq!(marks.len(), 1);
        assert!(marks.remove("https://example.com/"));
        assert!(!marks.remove("https://example.com/"), "already gone");
        assert!(marks.is_empty());
    }

    #[test]
    fn saving_a_page_twice_updates_it_rather_than_duplicating() {
        // Bookmarking the same page twice is a thing people do by accident,
        // and two entries for one URL is never what they meant.
        let mut marks = Bookmarks::default();
        marks.add("https://example.com/", "Old title");
        marks.add("https://example.com/", "New title");
        assert_eq!(marks.len(), 1);
        assert_eq!(marks.iter().next().expect("one").title, "New title");
    }

    #[test]
    fn toggling_says_where_it_ended_up() {
        let mut marks = Bookmarks::default();
        assert!(marks.toggle("https://example.com/", "Example"));
        assert!(!marks.toggle("https://example.com/", "Example"));
        assert!(marks.is_empty());
    }

    #[test]
    fn the_format_round_trips() {
        let mut marks = Bookmarks::default();
        marks.add("https://example.com/", "Example");
        marks.add("http://old.example.org/index.html", "An Old Page");
        marks.add("file:///home/user/notes.html", "");

        let reloaded = Bookmarks::parse(&marks.to_text());
        assert_eq!(reloaded.len(), 3);
        let entries: Vec<_> = reloaded.iter().cloned().collect();
        assert_eq!(entries, marks.iter().cloned().collect::<Vec<_>>());
    }

    #[test]
    fn a_title_cannot_break_its_own_line() {
        // A tab or newline in a title would split the line, turning one
        // bookmark into two — the second with a URL of nonsense.
        let mut marks = Bookmarks::default();
        marks.add("https://example.com/", "Two\tparts\nand a third");
        let reloaded = Bookmarks::parse(&marks.to_text());
        assert_eq!(reloaded.len(), 1);
        assert_eq!(
            reloaded.iter().next().expect("one").title,
            "Two parts and a third"
        );
    }

    #[test]
    fn a_corrupt_line_costs_one_bookmark_not_all_of_them() {
        let marks = Bookmarks::parse(
            "# a comment\n\
             https://a.example/\tFirst\n\
             \n\
             \tno url at all\n\
             https://b.example/\tSecond\n",
        );
        assert_eq!(marks.len(), 2);
        assert!(marks.contains("https://a.example/"));
        assert!(marks.contains("https://b.example/"));
    }

    #[test]
    fn a_url_with_no_title_is_still_a_bookmark() {
        let marks = Bookmarks::parse("https://example.com/\n");
        assert_eq!(marks.len(), 1);
        assert_eq!(marks.iter().next().expect("one").title, "");
    }

    #[test]
    fn a_missing_file_is_an_empty_list() {
        // Not having bookmarked anything yet is the normal state, not an error.
        let marks = Bookmarks::load(Path::new("/definitely/not/here/bookmarks.tsv"));
        assert!(marks.is_empty());
    }

    #[test]
    fn saving_and_loading_a_real_file() {
        let dir = std::env::temp_dir().join("2kbrowser-bookmark-tests");
        let path = dir.join("nested").join("bookmarks.tsv");
        let _ = std::fs::remove_dir_all(&dir);

        let mut marks = Bookmarks::default();
        marks.add("https://example.com/", "Example");
        marks.save(&path).expect("saves, creating the directory");

        let loaded = Bookmarks::load(&path);
        assert_eq!(loaded.len(), 1);
        assert!(loaded.contains("https://example.com/"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_page_lists_every_bookmark_as_a_link() {
        let mut marks = Bookmarks::default();
        marks.add("https://example.com/", "Example");
        marks.add("http://old.example.org/", "");
        let html = page(&marks);
        assert!(html.contains("href=\"https://example.com/\""), "{html}");
        assert!(html.contains(">Example<"), "{html}");
        // An untitled bookmark is still reachable; its URL stands in.
        assert!(html.contains("href=\"http://old.example.org/\""), "{html}");
    }

    #[test]
    fn the_page_declares_its_encoding() {
        // It is written to disk and then loaded through the browser's own front
        // door, where a document declaring nothing is windows-1252 — so an em
        // dash in a title would come back as mojibake.
        let mut marks = Bookmarks::default();
        marks.add("https://example.com/", "An Old Page — with an em dash");
        let html = page(&marks);
        assert!(html.contains("charset=\"utf-8\""), "{html}");

        let bytes = html.into_bytes();
        let (decoded, ..) = net::encoding::decode_document(&bytes, None);
        assert!(decoded.contains("— with an em dash"), "{decoded}");
    }

    #[test]
    fn an_empty_list_says_so_rather_than_being_a_blank_page() {
        let html = page(&Bookmarks::default());
        assert!(html.contains("Nothing saved"), "{html}");
    }

    #[test]
    fn a_title_cannot_rewrite_the_page_around_it() {
        // A title is whatever an author wrote. Interpolated raw, one containing
        // `</a>` would close the link and everything after it would be loose.
        let mut marks = Bookmarks::default();
        marks.add("https://example.com/?a=1&b=2", "</a><script>x</script>");
        let html = page(&marks);
        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
        assert!(html.contains("?a=1&amp;b=2"), "{html}");
    }

    #[test]
    fn the_page_actually_renders_with_its_links_intact() {
        // The point of a page rather than a panel is that the engine shows it.
        // If it did not lay out, the feature would be a blank window.
        let mut marks = Bookmarks::default();
        marks.add("https://example.com/", "Example");
        let mut fonts = text::FontStore::new();
        // With a base, because link geometry is resolved against one and a page
        // with no base carries none.
        let (origin, at) = net::parse_url("file:///tmp/bookmarks.html").expect("parses");
        let rendered = crate::render::render_with_base(
            &page(&marks),
            800,
            2000,
            &mut fonts,
            Some((&origin, &at)),
        );
        let links = rendered.links();
        assert_eq!(links.len(), 1, "{links:?}");
        assert_eq!(links[0].1, "https://example.com/");
    }

    #[test]
    fn the_rendered_list_sits_beside_the_saved_one() {
        assert_eq!(page_path().parent(), default_path().parent());
        assert!(page_path().ends_with("bookmarks.html"));
    }

    #[test]
    fn the_default_path_is_under_a_config_directory() {
        let path = default_path();
        assert!(path.ends_with("2kbrowser/bookmarks.tsv"), "{path:?}");
    }
}
