//! What counts as "the same site", and the small list that decides it.
//!
//! ADR-0006 refuses third-party subresources, and until now "third party" meant
//! *a different host*. That is the strictest reading and it is wrong about
//! ordinary sites: a page at `www.example.com` whose images sit on
//! `images.example.com` is one publisher serving one site from two names, which
//! was as true in 1999 as it is now. Refusing it removed nothing a tracker does
//! and cost the reader the pictures.
//!
//! ADR-0020 moves the boundary to the **registrable domain** — the public
//! suffix plus the one label in front of it, which is the unit somebody
//! actually buys. `images.example.com` and `www.example.com` share
//! `example.com`, so they are one site. `tracker.example.net` does not, so it
//! is still refused.
//!
//! # Why there is a list at all
//!
//! "The last two labels" is the obvious rule and it is wrong twice. It makes
//! `alice.co.uk` and `bob.co.uk` one site, because both end in `co.uk` — two
//! strangers handed each other's resources by a naming convention. And it makes
//! every `*.github.io` page one site, which is thousands of unrelated authors
//! under one name.
//!
//! Getting this exactly right means the Public Suffix List: several thousand
//! entries, maintained elsewhere, stale the day it is vendored. That is a
//! filter-list subscription with a different name, and ADR-0006 exists
//! precisely to avoid one. So this is a small list plus a rule, and it is
//! deliberately incomplete.
//!
//! # Which direction it is wrong in
//!
//! What makes a small list defensible is that its failures are not symmetric.
//! Guessing a suffix is **longer** than it is splits one site into two, and the
//! cost is a refused subresource the reader can allow from the padlock.
//! Guessing it **shorter** joins two sites into one, and the cost is a request
//! this browser exists to not make. Every entry here, and the rule below, only
//! ever lengthens a suffix — so a missing entry costs a permission prompt and
//! never a leak, and adding one can never open anything up.
//!
//! The rule is paired rather than free-standing for the same reason. "A
//! two-letter TLD after `co`, `com`, `ac`… is a suffix" would be right about
//! `bbc.co.uk` and wrong about `web.de` and `id.me`, which are one company
//! each — so the country codes that use second-level names are listed too, and
//! a generic label only counts under one of those.

/// Public suffixes of more than one label that the country-code rule below does
/// not catch.
///
/// Two kinds of entry, one reason. The era's free hosts and today's put an
/// author on a subdomain, so `someone.tripod.com` and `someoneelse.tripod.com`
/// are two people's pages and not one site — exactly the case the third-party
/// rule is for, wearing a first-party name.
///
/// Not the Public Suffix List and not trying to be: the test for an entry is
/// that a subdomain of it belongs to a *stranger*, and that a page this browser
/// is plausibly pointed at would otherwise be joined to one.
const MULTI_LABEL_SUFFIXES: &[&str] = &[
    // Where the era's personal pages lived.
    "fortunecity.com",
    "sourceforge.net",
    "tripod.com",
    // And where they live now.
    "amazonaws.com",
    "appspot.com",
    "azurewebsites.net",
    "blogspot.com",
    "cloudfront.net",
    "firebaseapp.com",
    "github.io",
    "gitlab.io",
    "glitch.me",
    "herokuapp.com",
    "neocities.org",
    "netlify.app",
    "pages.dev",
    "surge.sh",
    "tumblr.com",
    "vercel.app",
    "web.app",
    "wordpress.com",
    "workers.dev",
];

/// Country codes that register under second-level names rather than directly.
///
/// Paired with [`GENERIC_SECOND_LEVELS`]: a generic label counts as part of the
/// suffix only under one of these. Without the pairing, `web.de` and `id.me`
/// read as suffixes and two real companies get split from their own subdomains.
const SECOND_LEVEL_COUNTRIES: &[&str] = &[
    "ar", "at", "au", "bd", "br", "cn", "co", "cy", "eg", "fj", "gh", "hk", "id", "il", "in", "jp",
    "ke", "kr", "lb", "lk", "mx", "my", "ng", "nz", "pe", "pg", "ph", "pk", "pl", "py", "sa", "sg",
    "th", "tr", "tw", "ua", "uk", "uy", "ve", "vn", "za", "zw",
];

/// The second-level names those countries use.
///
/// Kinds of organisation rather than names of one, which is what makes the
/// pairing safe: nobody is the registrant of `co.uk` or `ne.jp`, so lengthening
/// the suffix to include one of these cannot split a real owner in two.
const GENERIC_SECOND_LEVELS: &[&str] = &[
    "ac", "asn", "co", "com", "edu", "go", "gov", "govt", "ltd", "mil", "ne", "net", "nom", "or",
    "org", "plc", "res", "sch",
];

/// The site a host belongs to: its registrable domain.
///
/// Returns the host unchanged when there is no shorter answer — an address
/// literal, a single label like `localhost`, or a host that *is* a public
/// suffix. Each of those then compares by equality, which is where this rule
/// started and the right place for it to land when the question has no answer.
///
/// The result is a suffix of the input — a trailing dot aside — so it borrows
/// rather than allocating: this runs on every subresource of every page.
///
/// Expects a lowercased host, which is what [`crate::parse_url`] produces. A
/// mixed-case one is not wrong so much as unmatched: it compares against the
/// lists below and finds nothing.
pub fn site(host: &str) -> &str {
    // A fully qualified name carries a trailing dot and is the same name
    // without it. Left on, it becomes an empty final label: `example.com.` and
    // `www.example.com.` would come out as two different sites, and neither of
    // them the site the page is actually on.
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty() || is_address_literal(host) {
        return host;
    }
    last_labels(host, suffix_labels(host) + 1)
}

/// How many labels at the end of `host` are the public suffix.
fn suffix_labels(host: &str) -> usize {
    for suffix in MULTI_LABEL_SUFFIXES {
        if ends_with_label_boundary(host, suffix) {
            return suffix.split('.').count();
        }
    }
    let mut labels = host.rsplit('.');
    let country = labels.next().unwrap_or_default();
    let second = labels.next().unwrap_or_default();
    if SECOND_LEVEL_COUNTRIES.contains(&country) && GENERIC_SECOND_LEVELS.contains(&second) {
        2
    } else {
        1
    }
}

/// Whether `host` is `suffix` or ends with it at a label boundary.
///
/// The boundary check is the whole point: `notgithub.io` ends with the text
/// `github.io` and is a different owner's domain entirely.
fn ends_with_label_boundary(host: &str, suffix: &str) -> bool {
    if host == suffix {
        return true;
    }
    host.len() > suffix.len()
        && host.ends_with(suffix)
        && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
}

/// The last `count` labels of `host`, or all of it if it has no more than that.
fn last_labels(host: &str, count: usize) -> &str {
    let mut end = host.len();
    for _ in 0..count {
        match host[..end].rfind('.') {
            Some(dot) => end = dot,
            None => return host,
        }
    }
    &host[end + 1..]
}

/// Whether the host is an address rather than a name.
///
/// Addresses must never be split into labels. `10.0.0.1` and `20.0.0.1` would
/// otherwise share a "registrable domain" of `0.1` and be read as one site —
/// two unrelated machines handed each other's resources by arithmetic that was
/// never about them.
///
/// A trailing all-digit label is the test for IPv4, and it is exact rather than
/// approximate: a top-level domain may not be all digits, so any host ending in
/// one is not a name.
fn is_address_literal(host: &str) -> bool {
    if host.contains(':') || host.starts_with('[') {
        return true;
    }
    let last = host.rsplit('.').next().unwrap_or_default();
    !last.is_empty() && last.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subdomain_resolves_to_the_domain_that_was_bought() {
        assert_eq!(site("www.example.com"), "example.com");
        assert_eq!(site("images.example.com"), "example.com");
        assert_eq!(site("a.b.c.example.com"), "example.com");
        assert_eq!(site("example.com"), "example.com");
    }

    #[test]
    fn a_country_second_level_is_part_of_the_suffix() {
        // The failure the list exists for. `alice.co.uk` and `bob.co.uk` are
        // two strangers, and "the last two labels" makes them one site.
        assert_eq!(site("www.alice.co.uk"), "alice.co.uk");
        assert_eq!(site("bob.co.uk"), "bob.co.uk");
        assert_ne!(site("alice.co.uk"), site("bob.co.uk"));

        assert_eq!(site("shop.example.com.au"), "example.com.au");
        assert_eq!(site("www.example.ne.jp"), "example.ne.jp");
    }

    #[test]
    fn a_generic_label_only_counts_under_a_country_that_uses_them() {
        // `web.de` and `id.me` are single companies. Reading `web` and `id` as
        // second-level suffixes would cut each of them off from its own
        // subdomains — the safe direction, but still wrong.
        assert_eq!(site("mail.web.de"), "web.de");
        assert_eq!(site("api.id.me"), "id.me");
        assert_eq!(site("www.co.com"), "co.com");
    }

    #[test]
    fn a_host_under_a_shared_hosting_suffix_is_its_own_site() {
        // Thousands of unrelated authors under one name. Joining them is the
        // one failure direction this module refuses to have.
        assert_eq!(site("alice.github.io"), "alice.github.io");
        assert_ne!(site("alice.github.io"), site("bob.github.io"));
        assert_eq!(site("someone.tripod.com"), "someone.tripod.com");
        assert_ne!(site("someone.tripod.com"), site("someoneelse.tripod.com"));
    }

    #[test]
    fn a_suffix_must_end_at_a_label_boundary() {
        // `notgithub.io` merely ends with the text of an entry, and is one
        // ordinary domain belonging to one owner.
        assert_eq!(site("www.notgithub.io"), "notgithub.io");
    }

    #[test]
    fn an_address_is_never_split_into_labels() {
        // `10.0.0.1` and `20.0.0.1` would share a registrable domain of `0.1`,
        // which is two unrelated machines joined by arithmetic.
        assert_eq!(site("10.0.0.1"), "10.0.0.1");
        assert_ne!(site("10.0.0.1"), site("20.0.0.1"));
        assert_eq!(site("127.0.0.1"), "127.0.0.1");
        assert_eq!(site("[::1]"), "[::1]");
    }

    #[test]
    fn a_fully_qualified_name_is_the_same_name() {
        assert_eq!(site("www.example.com."), "example.com");
        assert_eq!(site("www.example.com."), site("www.example.com"));
        assert_eq!(site("."), "");
    }

    #[test]
    fn a_host_with_no_shorter_answer_is_itself() {
        assert_eq!(site("localhost"), "localhost");
        assert_eq!(site(""), "");
        // A bare public suffix has no label in front of it to register.
        assert_eq!(site("co.uk"), "co.uk");
        assert_eq!(site("github.io"), "github.io");
    }

    #[test]
    fn the_list_only_ever_lengthens_a_suffix() {
        // The property the whole design rests on: every entry makes the site
        // *narrower* than the two-label default, never wider. A missing entry
        // therefore costs a permission prompt and never a leaked request, and
        // no entry can open anything that was closed.
        for host in [
            "a.b.example.com",
            "www.alice.co.uk",
            "alice.github.io",
            "mail.web.de",
            "deep.sub.example.org",
        ] {
            let listed = site(host);
            let two_labels = last_labels(host, 2);
            assert!(
                listed.len() >= two_labels.len(),
                "{host}: {listed} is shorter than the {two_labels} default, \
                 so the list widened a site instead of narrowing one"
            );
            assert!(host.ends_with(listed), "{host}: {listed} is not a suffix");
        }
    }
}
