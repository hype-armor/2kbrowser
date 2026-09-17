//! Where the reader has been, kept between runs (#197, ADR-0021).
//!
//! The third piece of state this browser keeps on disk, after bookmarks and the
//! site exceptions, and much the most sensitive of the three: a bookmark is a
//! page somebody chose to keep and this is every page they happened to open.
//! ADR-0018 named the decision and declined to make it; ADR-0021 makes it, and
//! says what it costs.
//!
//! What keeps that honest is the shape. A tab-separated file beside the other
//! two, which anybody can read, edit or delete with the tools they already
//! have; a bound, so it cannot quietly become a life story; and one entry per
//! address rather than one per visit, so the list stays something a person can
//! actually scan. Nothing is sent anywhere — there is nothing here that could
//! send it.

use std::path::{Path, PathBuf};

/// How many addresses are kept.
///
/// A bound rather than a policy about time, because a bound is the thing a
/// reader can check: the file cannot grow past this, whatever happens. Five
/// hundred is a few weeks of ordinary reading and about thirty kilobytes.
pub const MAX_ENTRIES: usize = 500;

/// One page the reader has been to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visit {
    /// When it was last opened, as `YYYY-MM-DD HH:MM`.
    pub when: String,
    /// Where it was.
    pub url: String,
    /// What it called itself, when it had a title.
    pub title: String,
}

/// The list, oldest first.
#[derive(Debug, Clone, Default)]
pub struct Visits {
    entries: Vec<Visit>,
}

impl Visits {
    /// Every visit, oldest first.
    ///
    /// Double-ended, because the file reads best oldest-first and the page
    /// reads best newest-first, and neither of those should mean a second copy
    /// of the list.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Visit> {
        self.entries.iter()
    }

    /// How many addresses are in it.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Records a visit, at `when`.
    ///
    /// One entry per address, moved to the end rather than added again. A
    /// browser that kept every visit separately would fill this list with
    /// thirty copies of the page somebody keeps going back to, which is the
    /// opposite of a list anybody can read — and the question it is here to
    /// answer is "where was that page?", which needs the address once.
    ///
    /// An empty title does not overwrite one already recorded. The title
    /// arrives from the renderer after the navigation, so the first thing this
    /// is told about a page is usually that it has no title yet.
    pub fn record(&mut self, url: impl Into<String>, title: impl Into<String>, when: String) {
        let url = url.into();
        let title = clean(&title.into());
        if url.is_empty() {
            return;
        }
        let existing = self
            .entries
            .iter()
            .position(|entry| entry.url == url)
            .map(|at| self.entries.remove(at));
        let title = match (title.is_empty(), existing) {
            (true, Some(was)) => was.title,
            _ => title,
        };
        self.entries.push(Visit { when, url, title });
        // From the front, because the front is the oldest.
        if self.entries.len() > MAX_ENTRIES {
            let over = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..over);
        }
    }

    /// Forgets everything.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Parses the file format.
    ///
    /// Malformed lines are skipped rather than failing the load, the way
    /// bookmarks and the site exceptions do: a corrupt line should cost one
    /// entry rather than the file.
    pub fn parse(text: &str) -> Self {
        let entries = text
            .lines()
            .filter_map(|line| {
                if line.trim().is_empty() || line.trim_start().starts_with('#') {
                    return None;
                }
                // Split before trimming, for the reason the other two files
                // give: a line starting with a tab has an empty first field,
                // and trimming first would promote the next one into it.
                let mut fields = line.splitn(3, '\t');
                let when = fields.next()?.trim();
                let url = fields.next().unwrap_or_default().trim();
                let title = fields.next().unwrap_or_default().trim();
                // An address has no whitespace in it. Anything that does is a
                // mangled line rather than a visit.
                let usable = !url.is_empty() && !url.contains(char::is_whitespace);
                usable.then(|| Visit {
                    when: when.to_owned(),
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
            "# 2kbrowser history: when, the address, and its title, separated by\n\
             # tabs. Delete a line to forget it; delete the file to forget all of\n\
             # it. Nothing here is sent anywhere.\n",
        );
        for entry in &self.entries {
            out.push_str(&entry.when);
            out.push('\t');
            out.push_str(&entry.url);
            out.push('\t');
            out.push_str(&entry.title);
            out.push('\n');
        }
        out
    }

    /// Reads the list from `path`. A missing file is an empty list.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    /// Writes it to `path`, creating the directory if needed.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.to_text())
    }
}

/// Renders the list as a page, newest first.
///
/// A page rather than a panel, for the reason the saved list gives: this
/// browser already knows how to show a document with links in it, and a history
/// *window* would be a second piece of interface with its own scrolling, its
/// own hit-testing and its own bugs.
pub fn page(visits: &Visits) -> String {
    let mut html = String::from(
        // The charset is not decoration: this is written to disk and then
        // loaded like any other file, and a document that declares nothing is
        // windows-1252, so a title with an em dash would come back as mojibake.
        "<!doctype html>\n<meta charset=\"utf-8\">\n<title>History</title>\n\
         <body style=\"font-family: sans-serif; margin: 2em; max-width: 44em\">\n\
         <h1>History</h1>\n",
    );
    if visits.is_empty() {
        html.push_str("<p>Nothing here yet.</p>\n");
        return html;
    }
    html.push_str(&format!(
        "<p><small>The most recent {} addresses. This list is a tab-separated \
         file you can edit or delete; Ctrl+Shift+H forgets all of it.</small></p>\n<ul>\n",
        visits.len()
    ));
    for entry in visits.iter().rev() {
        let url = escape(&entry.url);
        let when = escape(&entry.when);
        html.push_str("<li><a href=\"");
        html.push_str(&url);
        html.push_str("\">");
        if entry.title.is_empty() {
            html.push_str(&url);
            html.push_str("</a>");
        } else {
            html.push_str(&escape(&entry.title));
            html.push_str("</a><br><small>");
            html.push_str(&url);
            html.push_str("</small>");
        }
        html.push_str(&format!("<br><small>{when}</small></li>\n"));
    }
    html.push_str("</ul>\n");
    html
}

/// Where the rendered list is written.
///
/// Beside the file it describes, and regenerated every time it is opened: a
/// view of the list rather than a second copy of it.
pub fn page_path() -> PathBuf {
    default_path().with_file_name("history.html")
}

/// Where the history file lives: beside the bookmarks, by the same rule.
pub fn default_path() -> PathBuf {
    crate::bookmarks::default_path().with_file_name("history.tsv")
}

/// `YYYY-MM-DD HH:MM` for a moment, in UTC.
///
/// UTC rather than local time, and by hand rather than through a crate. A date
/// library would be a dependency for a dozen lines of arithmetic (ADR-0007),
/// and *local* time needs the zone database, which is a much larger thing to
/// carry — and would put the reader's timezone in a file that is otherwise
/// about nothing but addresses.
pub fn stamp(at: std::time::SystemTime) -> String {
    let seconds = at
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0);
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let (year, month, day) = civil_from_days(days as i64);
    let (hour, minute) = (rest / 3_600, (rest % 3_600) / 60);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

/// The civil date `days` after 1970-01-01, by Howard Hinnant's algorithm.
///
/// Written out because it is short, exact and testable, and because the
/// alternative is a dependency. The shifted era it works in — years starting in
/// March so that the leap day is last — is what makes it branchless.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    (year + i64::from(month <= 2), month, day)
}

/// Escapes text for HTML.
///
/// A title is whatever an author wrote and an address can contain an ampersand.
/// Neither is trusted enough to interpolate raw: a title containing `</a>`
/// would otherwise rewrite the page around it.
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

/// Strips what the file format cannot carry.
///
/// A title with a tab in it would split its own line, so one visit could
/// silently become two — or one with an address of nonsense.
fn clean(title: &str) -> String {
    title
        .chars()
        .map(|character| match character {
            '\t' | '\n' | '\r' => ' ',
            other => other,
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(when: &str) -> String {
        when.to_owned()
    }

    #[test]
    fn a_page_visited_twice_is_one_entry_at_its_latest_time() {
        // The question this list answers is "where was that page?", and thirty
        // copies of the page somebody keeps going back to is the opposite of a
        // list anybody can read.
        let mut visits = Visits::default();
        visits.record("https://example.com/", "Example", at("2026-09-01 10:00"));
        visits.record("https://other.example/", "Other", at("2026-09-01 10:05"));
        visits.record("https://example.com/", "Example", at("2026-09-01 11:00"));

        assert_eq!(visits.len(), 2);
        let last = visits.iter().next_back().expect("an entry");
        assert_eq!(last.url, "https://example.com/");
        assert_eq!(last.when, "2026-09-01 11:00", "the later visit wins");
    }

    #[test]
    fn a_later_visit_with_no_title_keeps_the_one_it_had() {
        // The title arrives from the renderer *after* the navigation, so the
        // first thing this is told about a page is that it has none.
        let mut visits = Visits::default();
        visits.record("https://example.com/", "Example", at("2026-09-01 10:00"));
        visits.record("https://example.com/", "", at("2026-09-01 11:00"));
        assert_eq!(
            visits.iter().next_back().expect("an entry").title,
            "Example"
        );
    }

    #[test]
    fn the_list_is_bounded() {
        // A bound rather than a policy about time, because a bound is the thing
        // a reader can check.
        let mut visits = Visits::default();
        for index in 0..MAX_ENTRIES + 25 {
            visits.record(format!("https://example.com/{index}"), "", at("now"));
        }
        assert_eq!(visits.len(), MAX_ENTRIES);
        assert_eq!(
            visits.iter().next().expect("an entry").url,
            format!("https://example.com/{}", 25),
            "the oldest should have gone, not the newest"
        );
    }

    #[test]
    fn a_round_trip_keeps_what_was_recorded() {
        let mut visits = Visits::default();
        visits.record("https://example.com/a", "A & B", at("2026-09-01 10:00"));
        visits.record("file:///home/reader/x.html", "", at("2026-09-01 10:01"));

        let back = Visits::parse(&visits.to_text());
        assert_eq!(back.len(), 2);
        let first = back.iter().next().expect("an entry");
        assert_eq!(first.url, "https://example.com/a");
        assert_eq!(first.title, "A & B");
        assert_eq!(first.when, "2026-09-01 10:00");
    }

    #[test]
    fn a_mangled_line_costs_one_entry_and_not_the_file() {
        let visits = Visits::parse(
            "# a comment\n\
             \n\
             2026-09-01 10:00\thttps://example.com/\tFine\n\
             this line has no tabs at all\n\
             2026-09-01 10:01\t\tno address\n\
             2026-09-01 10:02\ttwo words\tmangled\n\
             2026-09-01 10:03\thttps://example.org/\t\n",
        );
        assert_eq!(visits.len(), 2, "{:?}", visits.entries);
    }

    #[test]
    fn a_title_cannot_split_its_own_line() {
        let mut visits = Visits::default();
        visits.record("https://example.com/", "one\ttwo\nthree", at("now"));
        let text = visits.to_text();
        assert_eq!(
            text.lines().filter(|line| !line.starts_with('#')).count(),
            1,
            "{text}"
        );
        assert_eq!(Visits::parse(&text).len(), 1);
    }

    #[test]
    fn a_title_cannot_rewrite_the_page_around_it() {
        let mut visits = Visits::default();
        visits.record("https://example.com/?a=1&b=2", "</a><script>", at("now"));
        let html = page(&visits);
        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&amp;b=2"), "{html}");
    }

    #[test]
    fn an_empty_list_says_so_rather_than_drawing_an_empty_one() {
        assert!(page(&Visits::default()).contains("Nothing here yet"));
    }

    #[test]
    fn the_files_live_together() {
        assert_eq!(default_path().file_name().unwrap(), "history.tsv");
        assert_eq!(
            default_path().parent(),
            crate::bookmarks::default_path().parent(),
            "the three files should live together or none of them is findable"
        );
    }

    #[test]
    fn the_clock_reads_as_a_date() {
        use std::time::{Duration, UNIX_EPOCH};
        // Known points, including the two the arithmetic gets wrong when the
        // shifted era is off by one: a leap day and the first of March after it.
        for (seconds, expected) in [
            (0, "1970-01-01 00:00"),
            (86_399, "1970-01-01 23:59"),
            (951_782_400, "2000-02-29 00:00"),
            (951_868_800, "2000-03-01 00:00"),
            (1_767_225_600, "2026-01-01 00:00"),
            (1_788_000_000, "2026-08-29 10:40"),
        ] {
            assert_eq!(
                stamp(UNIX_EPOCH + Duration::from_secs(seconds)),
                expected,
                "for {seconds}"
            );
        }
    }

    #[test]
    fn clearing_forgets_everything() {
        let mut visits = Visits::default();
        visits.record("https://example.com/", "", at("now"));
        visits.clear();
        assert!(visits.is_empty());
        assert_eq!(
            Visits::parse(&visits.to_text()).len(),
            0,
            "and it stays cleared once written"
        );
    }
}
