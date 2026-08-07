//! Request policy: what the browser is willing to fetch (ADR-0006).
//!
//! The policy is deliberately structural rather than list-based. Advertising
//! and tracking require contacting a host other than the one in the address
//! bar, so refusing third-party requests removes the category without knowing a
//! single ad domain's name — no filter lists, no subscription, no arms race.

/// How a resource is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    /// `http:` — unauthenticated and tamperable.
    Http,
    /// `https:`.
    Https,
    /// `file:`.
    File,
}

impl Scheme {
    /// Whether traffic over this scheme is authenticated.
    ///
    /// Drives the chrome's security indicator. ADR-0006 allows plain HTTP
    /// because much of the old web needs it, on condition that it is never
    /// presented as secure.
    pub fn is_authenticated(self) -> bool {
        matches!(self, Scheme::Https)
    }
}

/// The origin of a URL: scheme, host, and port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// URL scheme.
    pub scheme: Scheme,
    /// Lowercased host, empty for `file:`.
    pub host: String,
    /// Port, defaulted from the scheme when absent.
    pub port: u16,
}

impl Origin {
    /// Whether two origins are the same for policy purposes.
    ///
    /// Host equality only — not scheme or port. A page on `example.com` loading
    /// an image from `example.com` is first-party whether or not the protocols
    /// match, and treating a port change as third-party would break ordinary
    /// sites while blocking nothing a tracker does.
    pub fn is_same_site(&self, other: &Origin) -> bool {
        !self.host.is_empty() && self.host == other.host
    }
}

/// Why a request was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// A subresource on a host other than the document's.
    ThirdParty {
        /// The host that was asked for.
        host: String,
    },
    /// A scheme we do not implement.
    UnsupportedScheme {
        /// The scheme as written.
        scheme: String,
    },
    /// A page from the network asked to read a local file.
    LocalFile,
    /// The URL could not be parsed.
    Malformed,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::ThirdParty { host } => {
                write!(f, "blocked third-party request to {host} (ADR-0006)")
            }
            Refusal::LocalFile => {
                write!(
                    f,
                    "blocked a network page from reading a local file (ADR-0006)"
                )
            }
            Refusal::UnsupportedScheme { scheme } => write!(f, "unsupported scheme `{scheme}:`"),
            Refusal::Malformed => write!(f, "malformed URL"),
        }
    }
}

/// What a request is for. Navigation is not subject to the third-party rule —
/// following a link to another site is the point of a browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    /// A top-level navigation.
    Navigation,
    /// A stylesheet, image, or other resource loaded by a document.
    Subresource,
}

/// The network policy.
///
/// The default is the whole point: no third-party requests at all. Anything
/// less restrictive has to be asked for explicitly.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    /// Hosts the user has explicitly allowed as third parties.
    ///
    /// Empty by default. ADR-0006 makes the rule a default, not a prohibition;
    /// the per-site override is M3 chrome work, and this is what it will drive.
    pub allowed_third_parties: Vec<String>,
}

impl Policy {
    /// Decides whether a request may proceed.
    pub fn check(
        &self,
        document: Option<&Origin>,
        target: &Origin,
        kind: RequestKind,
    ) -> Result<(), Refusal> {
        if kind == RequestKind::Navigation {
            return Ok(());
        }
        let Some(document) = document else {
            return Ok(());
        };

        match (document.scheme, target.scheme) {
            // A file: document has no host, so a file: subresource beside it
            // is first-party by construction.
            (Scheme::File, Scheme::File) => return Ok(()),
            // A page from the network must never read the disk. There is no
            // origin such a request could be first-party to, and allowing it
            // turns every page into a local file reader.
            (Scheme::Http | Scheme::Https, Scheme::File) => return Err(Refusal::LocalFile),
            // A local document reaching out to the network is third-party by
            // the same argument: it has no host, so nothing it asks the
            // network for can be first-party. Falls through to the allow-list
            // below, so an explicitly permitted host still works.
            (Scheme::File, _) => {}
            _ if document.is_same_site(target) => return Ok(()),
            _ => {}
        }
        if self
            .allowed_third_parties
            .iter()
            .any(|host| host == &target.host)
        {
            return Ok(());
        }
        Err(Refusal::ThirdParty {
            host: target.host.clone(),
        })
    }
}

/// Whether a URL reference names its own scheme, and so is already absolute.
///
/// Not a test for `://`: `mailto:` and `tel:` have no authority, and treating
/// them as relative paths glues them onto the document's directory and produces
/// nonsense like `file:///pages/mailto:someone@example.com`.
///
/// RFC 3986's rule, which means a relative path whose first segment contains a
/// colon really is read as a scheme — that is why such a path has to be written
/// `./odd:name.html`, and browsers agree.
pub fn has_scheme(reference: &str) -> bool {
    let Some(colon) = reference.find(':') else {
        return false;
    };
    let scheme = &reference[..colon];
    !scheme.is_empty()
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Converts a URL path into a filesystem path.
///
/// The canonical `file:` URL for a Windows path carries a leading slash before
/// the drive letter — `file:///D:/site/x.html` — and `D:/site/x.html` is what
/// actually opens. Forward slashes are left alone: Windows accepts them.
pub fn to_file_path(url_path: &str) -> &str {
    let bytes = url_path.as_bytes();
    let looks_like_a_drive = bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && (bytes[2] == b':' || (bytes.len() >= 4 && bytes[2] == b'|'));
    if looks_like_a_drive {
        &url_path[1..]
    } else {
        url_path
    }
}

/// Whether this is a Windows path beginning with a drive letter.
///
/// A drive letter is not a scheme, however exactly `C:` fits the definition of
/// one — [`has_scheme`] says yes to it, correctly by RFC 3986 and uselessly in
/// practice. Anything deciding "is this a URL or a path?" has to ask this
/// first, or a Windows path becomes a URL with a one-letter scheme and fails
/// somewhere further on with nothing to explain it.
///
/// One letter, deliberately: `http:` is not a drive and never was.
pub fn is_drive_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

/// The canonical `file:` URL for a filesystem path.
///
/// `format!("file://{}", path.display())` is the obvious thing to write and is
/// wrong on Windows twice over: the separators are backslashes, and the drive
/// letter lands where the authority goes, so the URL has two slashes where the
/// canonical form has three. Both are invisible on Unix, where the leading `/`
/// of an absolute path supplies the third slash and there are no backslashes to
/// convert — which is exactly why this kept being written by hand and kept
/// being wrong on one platform.
///
/// The result round-trips: [`parse_url`] then [`to_file_path`] gives back a
/// path that opens.
///
/// Plain UNC paths (`\\server\share`) are not handled. Nothing here produces
/// one — `canonicalize` spells a network path with the verbatim prefix below —
/// and on Unix a leading `//` is a legitimate way to write a root-relative
/// path, so there is no way to tell the two apart from the string alone.
pub fn file_url(path: &std::path::Path) -> String {
    let separated = path.display().to_string().replace('\\', "/");
    // `\\?\D:\dir` is what Windows `canonicalize` hands back. The prefix is a
    // Win32 API detail rather than part of the path, and a URL carrying it
    // resolves against nothing: `logo.png` beside `\\?\D:\x\page.html` comes
    // out as `/?/D:/x/logo.png`, which is not a file anywhere.
    let separated = separated.strip_prefix("//?/").unwrap_or(&separated);
    // Its network spelling, `\\?\UNC\server\share`, names a host — and a host
    // belongs where a URL keeps one rather than buried in the path.
    if let Some(rest) = separated.strip_prefix("UNC/") {
        return format!("file://{rest}");
    }
    format!("file:///{}", separated.trim_start_matches('/'))
}

/// Parses a URL into an origin plus its path.
///
/// A deliberately small parser covering the schemes we support, rather than a
/// URL crate: the surface actually used here is `scheme://host[:port]/path`,
/// and full RFC 3986 handling is M2 work alongside relative-URL resolution.
pub fn parse_url(url: &str) -> Result<(Origin, String), Refusal> {
    let (scheme_text, rest) = url.split_once("://").ok_or(Refusal::Malformed)?;
    let scheme = match scheme_text.to_ascii_lowercase().as_str() {
        "http" => Scheme::Http,
        "https" => Scheme::Https,
        "file" => Scheme::File,
        other => {
            return Err(Refusal::UnsupportedScheme {
                scheme: other.to_owned(),
            });
        }
    };

    if scheme == Scheme::File {
        // A URL's separator is `/`, whatever the filesystem underneath uses.
        // Windows hands out `D:\\a\\page.html`, and leaving the backslashes in
        // means relative resolution finds no directory at all and every
        // subresource resolves to the root — so images and frames silently
        // fail to load on one platform and not the others. Windows accepts
        // forward slashes when opening the file, so this needs no undoing.
        let path = if rest.is_empty() {
            "/".to_owned()
        } else {
            rest.replace('\\', "/")
        };
        return Ok((
            Origin {
                scheme,
                host: String::new(),
                port: 0,
            },
            path,
        ));
    }

    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    // Strip credentials; they are not supported and must not be mistaken for a
    // host, which would let `evil.com` masquerade as `user@bank.com`.
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    if authority.is_empty() {
        return Err(Refusal::Malformed);
    }

    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port_text)) => {
            let port = port_text.parse().map_err(|_| Refusal::Malformed)?;
            (host, port)
        }
        None => (authority, if scheme == Scheme::Https { 443 } else { 80 }),
    };
    if host.is_empty() {
        return Err(Refusal::Malformed);
    }

    Ok((
        Origin {
            scheme,
            host: host.to_ascii_lowercase(),
            port,
        },
        path.to_owned(),
    ))
}

/// Resolves a possibly-relative URL against the document it appeared in.
///
/// Covers the forms that actually appear in markup: absolute URLs,
/// protocol-relative `//host/path`, root-relative `/path`, and plain relative
/// paths including `../`. Not a full RFC 3986 implementation — that arrives
/// with the rest of URL handling — but wrong resolution silently loads the
/// wrong resource, so the cases it does handle are tested.
pub fn resolve(base: &Origin, base_path: &str, relative: &str) -> String {
    let relative = relative.trim();
    if has_scheme(relative) {
        return relative.to_owned();
    }
    let scheme = match base.scheme {
        Scheme::Http => "http",
        Scheme::Https => "https",
        Scheme::File => "file",
    };
    if let Some(rest) = relative.strip_prefix("//") {
        return format!("{scheme}://{rest}");
    }

    let authority = if base.host.is_empty() {
        String::new()
    } else {
        let default_port = if base.scheme == Scheme::Https {
            443
        } else {
            80
        };
        if base.port == default_port {
            base.host.clone()
        } else {
            format!("{}:{}", base.host, base.port)
        }
    };

    if let Some(rest) = relative.strip_prefix('/') {
        return format!("{scheme}://{authority}/{rest}");
    }

    // Relative to the document's directory, which is everything up to and
    // including the last slash.
    let directory = match base_path.rfind('/') {
        Some(index) => &base_path[..=index],
        None => "/",
    };
    let mut segments: Vec<&str> = Vec::new();
    for segment in directory.split('/').chain(relative.split('/')) {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    // A trailing slash on the input means a directory, not a file.
    let trailing = if relative.ends_with('/') { "/" } else { "" };
    format!("{scheme}://{authority}/{}{trailing}", segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(url: &str) -> Origin {
        parse_url(url).expect("parses").0
    }

    fn resolve_from(base_url: &str, relative: &str) -> String {
        let (origin, path) = parse_url(base_url).expect("parses");
        resolve(&origin, &path, relative)
    }

    #[test]
    fn an_absolute_url_is_returned_unchanged() {
        assert_eq!(
            resolve_from("https://example.com/a/b.html", "https://other.org/x.png"),
            "https://other.org/x.png"
        );
    }

    #[test]
    fn a_protocol_relative_url_takes_the_document_scheme() {
        assert_eq!(
            resolve_from("https://example.com/a.html", "//cdn.example.net/x.png"),
            "https://cdn.example.net/x.png"
        );
    }

    #[test]
    fn a_root_relative_url_keeps_the_host() {
        assert_eq!(
            resolve_from("https://example.com/deep/page.html", "/logo.png"),
            "https://example.com/logo.png"
        );
    }

    #[test]
    fn a_relative_url_resolves_against_the_documents_directory() {
        assert_eq!(
            resolve_from("https://example.com/a/b/page.html", "img/logo.png"),
            "https://example.com/a/b/img/logo.png"
        );
        // A document at the root has no directory to descend from.
        assert_eq!(
            resolve_from("https://example.com/page.html", "logo.png"),
            "https://example.com/logo.png"
        );
    }

    #[test]
    fn dot_segments_are_removed() {
        assert_eq!(
            resolve_from("https://example.com/a/b/page.html", "../logo.png"),
            "https://example.com/a/logo.png"
        );
        assert_eq!(
            resolve_from("https://example.com/a/b/page.html", "./logo.png"),
            "https://example.com/a/b/logo.png"
        );
        assert_eq!(
            resolve_from("https://example.com/a/b/page.html", "../../up/logo.png"),
            "https://example.com/up/logo.png"
        );
    }

    #[test]
    fn a_non_default_port_survives_resolution() {
        assert_eq!(
            resolve_from("http://example.com:8080/a/page.html", "logo.png"),
            "http://example.com:8080/a/logo.png"
        );
        // The default port is not written back out.
        assert_eq!(
            resolve_from("https://example.com:443/page.html", "logo.png"),
            "https://example.com/logo.png"
        );
    }

    #[test]
    fn file_urls_resolve_relative_to_the_document() {
        assert_eq!(
            resolve_from("file:///home/user/site/page.html", "img/logo.png"),
            "file:///home/user/site/img/logo.png"
        );
    }

    #[test]
    fn parses_the_schemes_we_support() {
        assert_eq!(origin("https://example.com/a").scheme, Scheme::Https);
        assert_eq!(origin("http://example.com").scheme, Scheme::Http);
        assert_eq!(origin("file:///tmp/x.html").scheme, Scheme::File);
        assert_eq!(
            parse_url("ftp://example.com"),
            Err(Refusal::UnsupportedScheme {
                scheme: "ftp".to_owned()
            })
        );
        assert_eq!(parse_url("not a url"), Err(Refusal::Malformed));
    }

    #[test]
    fn defaults_ports_by_scheme() {
        assert_eq!(origin("https://example.com/").port, 443);
        assert_eq!(origin("http://example.com/").port, 80);
        assert_eq!(origin("https://example.com:8443/").port, 8443);
    }

    #[test]
    fn splits_path_from_authority() {
        let (origin, path) = parse_url("https://example.com/a/b?c=d").expect("parses");
        assert_eq!(origin.host, "example.com");
        assert_eq!(path, "/a/b?c=d");
        assert_eq!(parse_url("https://example.com").expect("parses").1, "/");
    }

    #[test]
    fn credentials_cannot_disguise_the_host() {
        // `https://bank.com@evil.com/` is evil.com. Reading the host as the
        // part before the @ is a classic phishing vector.
        assert_eq!(origin("https://bank.com@evil.com/").host, "evil.com");
    }

    #[test]
    fn host_is_lowercased() {
        assert_eq!(origin("https://EXAMPLE.com/").host, "example.com");
        assert!(origin("https://EXAMPLE.com/").is_same_site(&origin("https://example.com/")));
    }

    #[test]
    fn third_party_subresources_are_blocked_by_default() {
        let policy = Policy::default();
        let document = origin("https://example.com/");
        let tracker = origin("https://tracker.example.net/pixel.gif");
        assert_eq!(
            policy.check(Some(&document), &tracker, RequestKind::Subresource),
            Err(Refusal::ThirdParty {
                host: "tracker.example.net".to_owned()
            })
        );
    }

    #[test]
    fn first_party_subresources_are_allowed() {
        let policy = Policy::default();
        let document = origin("https://example.com/");
        let image = origin("https://example.com/logo.png");
        assert!(
            policy
                .check(Some(&document), &image, RequestKind::Subresource)
                .is_ok()
        );
    }

    #[test]
    fn a_scheme_change_is_still_first_party() {
        // Mixed content is a separate concern; treating it as third-party would
        // block ordinary sites without stopping anything a tracker does.
        let policy = Policy::default();
        let document = origin("https://example.com/");
        let image = origin("http://example.com/logo.png");
        assert!(
            policy
                .check(Some(&document), &image, RequestKind::Subresource)
                .is_ok()
        );
    }

    #[test]
    fn navigation_is_never_blocked() {
        // Following a link to another site is the point of a browser.
        let policy = Policy::default();
        let document = origin("https://example.com/");
        let elsewhere = origin("https://other.example.org/");
        assert!(
            policy
                .check(Some(&document), &elsewhere, RequestKind::Navigation)
                .is_ok()
        );
    }

    #[test]
    fn an_explicit_allowance_admits_one_host_only() {
        let policy = Policy {
            allowed_third_parties: vec!["cdn.example.net".to_owned()],
        };
        let document = origin("https://example.com/");
        assert!(
            policy
                .check(
                    Some(&document),
                    &origin("https://cdn.example.net/a.css"),
                    RequestKind::Subresource
                )
                .is_ok()
        );
        assert!(
            policy
                .check(
                    Some(&document),
                    &origin("https://other.example.net/a.css"),
                    RequestKind::Subresource
                )
                .is_err()
        );
    }

    #[test]
    fn http_is_not_reported_as_authenticated() {
        assert!(Scheme::Https.is_authenticated());
        assert!(!Scheme::Http.is_authenticated());
        assert!(!Scheme::File.is_authenticated());
    }
}

#[cfg(test)]
mod scheme_boundary_tests {
    use super::*;

    fn origin(url: &str) -> Origin {
        parse_url(url).expect("parses").0
    }

    fn check(document: &str, target: &str) -> Result<(), Refusal> {
        Policy::default().check(
            Some(&origin(document)),
            &origin(target),
            RequestKind::Subresource,
        )
    }

    #[test]
    fn a_local_file_may_load_its_neighbours() {
        assert!(check("file:///pages/index.html", "file:///pages/logo.png").is_ok());
    }

    #[test]
    fn a_local_file_may_not_reach_the_network() {
        // A saved page full of tracking pixels is exactly the case: it has no
        // host, so nothing it asks the network for can be first-party, and
        // treating "no host" as "everything is first-party" turns every local
        // file into an unrestricted beacon.
        let refusal = check(
            "file:///pages/index.html",
            "https://tracker.example.net/p.gif",
        );
        assert!(
            matches!(refusal, Err(Refusal::ThirdParty { .. })),
            "got {refusal:?}"
        );
    }

    #[test]
    fn an_allowed_host_still_works_from_a_local_file() {
        // The rule is a default, not a prohibition (ADR-0006).
        let policy = Policy {
            allowed_third_parties: vec!["cdn.example.net".to_owned()],
        };
        assert!(
            policy
                .check(
                    Some(&origin("file:///pages/index.html")),
                    &origin("https://cdn.example.net/x.png"),
                    RequestKind::Subresource,
                )
                .is_ok()
        );
    }

    #[test]
    fn a_network_page_may_not_read_the_disk() {
        // Not a third-party question — there is no host to allow-list. A page
        // fetched over the network has no business reading local files at all.
        let refusal = check("https://example.com/page.html", "file:///etc/passwd");
        assert!(
            matches!(refusal, Err(Refusal::LocalFile)),
            "got {refusal:?}"
        );
    }

    #[test]
    fn an_allow_list_does_not_open_the_disk() {
        let policy = Policy {
            allowed_third_parties: vec!["etc".to_owned(), String::new(), "localhost".to_owned()],
        };
        assert!(
            policy
                .check(
                    Some(&origin("https://example.com/page.html")),
                    &origin("file:///etc/passwd"),
                    RequestKind::Subresource,
                )
                .is_err(),
            "the allow-list is for third-party hosts, not for local files"
        );
    }

    #[test]
    fn navigation_is_not_subject_to_any_of_this() {
        // Following a link off-site, or opening a local file, is what a
        // browser is for. The rule is about subresources.
        assert!(
            Policy::default()
                .check(
                    Some(&origin("https://example.com/a.html")),
                    &origin("file:///pages/b.html"),
                    RequestKind::Navigation,
                )
                .is_ok()
        );
    }
}

#[cfg(test)]
mod windows_path_tests {
    use super::*;

    /// The form `format!("file://{}", path.display())` produces on Windows.
    const WINDOWS_DOC: &str = r"file://D:\a\2kbrowser\tests\ref\fixtures\images.html";

    #[test]
    fn a_windows_file_url_parses_to_a_slash_separated_path() {
        // A URL's separator is `/` whatever the filesystem uses. Leaving the
        // backslashes in leaves the path with no directory to resolve against.
        let (origin, path) = parse_url(WINDOWS_DOC).expect("parses");
        assert_eq!(origin.scheme, Scheme::File);
        assert_eq!(path, "D:/a/2kbrowser/tests/ref/fixtures/images.html");
    }

    #[test]
    fn a_subresource_beside_a_windows_document_resolves_next_to_it() {
        // This is the bug that made images and frames load on Linux and macOS
        // and silently not on Windows — one platform rendering a different
        // page from the same input, which is the whole thing ADR-0005 exists
        // to prevent.
        let (origin, path) = parse_url(WINDOWS_DOC).expect("parses");
        assert_eq!(
            resolve(&origin, &path, "assets/logo.png"),
            "file:///D:/a/2kbrowser/tests/ref/fixtures/assets/logo.png"
        );
    }

    #[test]
    fn a_canonical_windows_file_url_round_trips_to_an_openable_path() {
        // `file:///D:/x` is the canonical form and is what `resolve` emits;
        // `D:/x` is what actually opens.
        let (_, path) = parse_url("file:///D:/site/images/x.png").expect("parses");
        assert_eq!(to_file_path(&path), "D:/site/images/x.png");
    }

    #[test]
    fn a_unix_path_is_left_alone() {
        // Its leading slash is part of the path, not a URL artefact.
        assert_eq!(to_file_path("/home/user/x.png"), "/home/user/x.png");
        assert_eq!(to_file_path("/a/b"), "/a/b");
    }

    #[test]
    fn a_unix_file_url_is_unaffected() {
        let (origin, path) = parse_url("file:///home/user/pages/index.html").expect("parses");
        assert_eq!(path, "/home/user/pages/index.html");
        assert_eq!(
            resolve(&origin, &path, "assets/logo.png"),
            "file:///home/user/pages/assets/logo.png"
        );
    }

    #[test]
    fn a_drive_letter_is_not_a_scheme() {
        // `has_scheme` says yes to `C:`, correctly by RFC 3986 and uselessly in
        // practice — so anything deciding "URL or path?" has to ask this first.
        assert!(has_scheme(r"C:\Users\reader\a.html"), "the trap");
        assert!(is_drive_path(r"C:\Users\reader\a.html"));
        assert!(is_drive_path("D:/site/a.html"));

        // One letter only: these are schemes, not drives.
        assert!(!is_drive_path("http://example.com/"));
        assert!(!is_drive_path("file:///a.html"));
        // And these are not either.
        assert!(!is_drive_path("/home/user/a.html"));
        assert!(!is_drive_path("example.com"));
        assert!(!is_drive_path("C:"), "a drive with no path is not one");
    }

    #[test]
    fn an_extended_length_path_loses_its_win32_prefix() {
        // `canonicalize` returns this form on Windows, and the budget harness
        // pasted it after `file://` — so a same-origin image beside the
        // document resolved to `/?/D:/...`, which is not a file anywhere. The
        // budget it broke exists to prove a zero means something, and it
        // correctly refused to claim one.
        let verbatim = std::path::Path::new(r"\\?\D:\a\fixtures\page.html");
        assert_eq!(file_url(verbatim), "file:///D:/a/fixtures/page.html");

        let (origin, at) = parse_url(&file_url(verbatim)).expect("parses");
        let image = resolve(&origin, &at, "logo.png");
        assert_eq!(image, "file:///D:/a/fixtures/logo.png");
        assert_eq!(
            to_file_path(&parse_url(&image).expect("parses").1),
            "D:/a/fixtures/logo.png",
            "and that is a path that opens"
        );
    }

    #[test]
    fn a_verbatim_network_path_keeps_its_host_where_a_host_goes() {
        assert_eq!(
            file_url(std::path::Path::new(r"\\?\UNC\server\share\page.html")),
            "file://server/share/page.html"
        );
    }

    #[test]
    fn a_path_becomes_a_url_that_resolves_against_itself() {
        // The check that matters: whatever `file_url` produces has to be
        // something `parse_url` and `resolve` agree with, on both shapes of
        // path. Anything else and a link beside the document lands elsewhere.
        for (path, expected) in [
            (r"C:\site\pages\a.html", "file:///C:/site/pages/a.html"),
            ("/home/user/pages/a.html", "file:///home/user/pages/a.html"),
        ] {
            let url = file_url(std::path::Path::new(path));
            assert_eq!(url, expected);

            let (origin, at) = parse_url(&url).expect("parses");
            let sibling = resolve(&origin, &at, "b.html");
            assert_eq!(sibling, expected.replace("a.html", "b.html"));
            // And what comes back opens: the drive letter loses its URL slash,
            // the Unix path keeps its own.
            assert_eq!(
                to_file_path(&parse_url(&sibling).expect("parses").1),
                path.replace('\\', "/").replace("a.html", "b.html")
            );
        }
    }

    #[test]
    fn a_relative_path_climbing_out_still_works_on_windows() {
        let (origin, path) = parse_url(r"file://D:\site\pages\a.html").expect("parses");
        assert_eq!(
            resolve(&origin, &path, "../images/x.png"),
            "file:///D:/site/images/x.png"
        );
    }
}

#[cfg(test)]
mod scheme_tests {
    use super::*;

    fn resolve_from(base_url: &str, reference: &str) -> String {
        let (origin, path) = parse_url(base_url).expect("parses");
        resolve(&origin, &path, reference)
    }

    #[test]
    fn a_scheme_without_an_authority_is_still_absolute() {
        // `mailto:` has no `//`, so testing for `://` reads it as a relative
        // path and glues it onto the document's directory.
        for reference in [
            "mailto:someone@example.com",
            "tel:+15551234",
            "data:text/plain,hi",
            "javascript:void(0)",
        ] {
            assert_eq!(
                resolve_from("https://example.com/pages/a.html", reference),
                reference,
                "mangled {reference}"
            );
        }
    }

    #[test]
    fn ordinary_relative_references_are_still_relative() {
        assert_eq!(
            resolve_from("https://example.com/pages/a.html", "b.html"),
            "https://example.com/pages/b.html"
        );
        assert_eq!(
            resolve_from("https://example.com/pages/a.html", "/c.html"),
            "https://example.com/c.html"
        );
        assert_eq!(
            resolve_from("https://example.com/pages/a.html", "../d.html"),
            "https://example.com/d.html"
        );
    }

    #[test]
    fn a_scheme_must_look_like_one() {
        assert!(has_scheme("https://example.com"));
        assert!(has_scheme("mailto:x@y"));
        assert!(!has_scheme("no-colon-here.html"));
        // A scheme cannot start with a digit, and this is a port, not a scheme.
        assert!(!has_scheme("8080:something"));
        assert!(!has_scheme(":leading-colon"));
    }
}
