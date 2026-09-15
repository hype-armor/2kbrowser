//! The exceptions a reader has granted to the third-party rule (#118).
//!
//! ADR-0006 refuses off-origin subresources by default and names the per-site
//! override as the reason that default is allowed to be absolute. This is where
//! the override is kept: a tab-separated file beside `bookmarks.tsv`, one pair
//! per line, in the user's config directory.
//!
//! The same format for the same reason bookmarks use it. The whole list is a
//! few hundred bytes, and a reader who wants to see what their browser has been
//! told to allow — or take something back with a text editor at three in the
//! morning — should not need a tool to do it. A permission list nobody can read
//! is a permission list nobody audits.
//!
//! It is the second piece of state this browser keeps between runs, and the
//! first was bookmarks. Worth saying plainly, because "no account, no sync, no
//! profile" is part of the product (§1) — but a permission that did not survive
//! the window closing would have to be granted again on every visit, and a
//! prompt asked often enough stops being a decision and becomes a reflex.

use std::path::{Path, PathBuf};

use net::{Exception, Policy};

/// Parses the file format into a policy.
///
/// Malformed lines are skipped rather than failing the load. A corrupt line
/// should cost one exception, and the direction that costs is the safe one:
/// the pair is simply not allowed, which is the default this file exists to
/// depart from.
pub fn parse(text: &str) -> Policy {
    let mut policy = Policy::default();
    for line in text.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        // Split before trimming, the way bookmarks does: a line starting with a
        // tab has an empty site field, and trimming first would promote the
        // host into it — turning "allowed nowhere" into "allowed on the host's
        // own name", which is a permission nobody granted.
        let Some((site, host)) = line.split_once('\t') else {
            continue;
        };
        let (site, host) = (site.trim(), host.trim());
        // Neither field may be empty or hold whitespace. A host with a space in
        // it is a mangled line, and an empty site would match no document —
        // both are noise rather than permissions, and keeping them would make
        // the list harder to read for no gain.
        if site.is_empty()
            || host.is_empty()
            || site.contains(char::is_whitespace)
            || host.contains(char::is_whitespace)
        {
            continue;
        }
        policy.allow(site, host);
    }
    policy
}

/// Renders the file format.
pub fn to_text(policy: &Policy) -> String {
    let mut out = String::from(
        "# 2kbrowser site exceptions: one per line, the site and the third-party\n\
         # host it may load from, separated by a tab. Delete a line to revoke it.\n",
    );
    for Exception { site, host } in &policy.exceptions {
        out.push_str(site);
        out.push('\t');
        out.push_str(host);
        out.push('\n');
    }
    out
}

/// Reads the exceptions from `path`.
///
/// A missing file is an empty policy rather than an error: having granted
/// nothing is the normal state, and the one this browser wants people to stay
/// in.
pub fn load(path: &Path) -> Policy {
    std::fs::read_to_string(path)
        .map(|text| parse(&text))
        .unwrap_or_default()
}

/// Writes them to `path`, creating the directory if needed.
pub fn save(policy: &Policy, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, to_text(policy))
}

/// Where the list lives, by the same rule bookmarks follow.
pub fn default_path() -> PathBuf {
    crate::bookmarks::default_path().with_file_name("sites.tsv")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_granted_exception_survives_a_round_trip() {
        let mut policy = Policy::default();
        policy.allow("example.com", "cdn.example.net");
        policy.allow("example.com", "fonts.example.org");
        policy.allow("elsewhere.example", "cdn.example.net");

        let back = parse(&to_text(&policy));
        assert_eq!(
            back.allowed_on("example.com"),
            ["cdn.example.net", "fonts.example.org"]
        );
        assert_eq!(back.allowed_on("elsewhere.example"), ["cdn.example.net"]);
        assert!(
            !back.allows("elsewhere.example", "fonts.example.org"),
            "the round trip invented a pair nobody granted"
        );
    }

    #[test]
    fn a_mangled_line_costs_one_exception_and_not_the_file() {
        let policy = parse(
            "# a comment\n\
             \n\
             example.com\tcdn.example.net\n\
             this line has no tab at all\n\
             \tcdn.example.net\n\
             example.org\t\n\
             two words\tcdn.example.net\n\
             example.net\tcdn.example.net\n",
        );
        assert_eq!(policy.allowed_on("example.com"), ["cdn.example.net"]);
        assert_eq!(policy.allowed_on("example.net"), ["cdn.example.net"]);
        assert_eq!(
            policy.exceptions.len(),
            2,
            "a malformed line was read as a permission: {:?}",
            policy.exceptions
        );
    }

    #[test]
    fn a_line_starting_with_a_tab_does_not_promote_its_host() {
        // The failure the split-before-trim rule exists for. Trimming first
        // turns "\tcdn.example.net" into one field, which the old bookmark
        // parser would have read as a URL and this would read as a site
        // allowed to load from itself — a permission nobody typed.
        let policy = parse("\tcdn.example.net\n");
        assert!(policy.exceptions.is_empty(), "{:?}", policy.exceptions);
    }

    #[test]
    fn a_missing_file_grants_nothing() {
        let policy = load(Path::new("/nonexistent/2kbrowser/sites.tsv"));
        assert!(policy.exceptions.is_empty());
    }

    #[test]
    fn the_list_is_written_where_the_bookmarks_are() {
        let path = default_path();
        assert_eq!(path.file_name().unwrap(), "sites.tsv");
        assert_eq!(
            path.parent(),
            crate::bookmarks::default_path().parent(),
            "the two files should live together or neither is findable"
        );
    }
}
