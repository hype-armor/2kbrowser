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
    /// The URL could not be parsed.
    Malformed,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::ThirdParty { host } => {
                write!(f, "blocked third-party request to {host} (ADR-0006)")
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
        // A file: document has no host, so every subresource beside it is
        // first-party by construction.
        let Some(document) = document else {
            return Ok(());
        };
        if document.scheme == Scheme::File || target.scheme == Scheme::File {
            return Ok(());
        }
        if document.is_same_site(target) {
            return Ok(());
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
        let path = if rest.is_empty() { "/" } else { rest };
        return Ok((
            Origin {
                scheme,
                host: String::new(),
                port: 0,
            },
            path.to_owned(),
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
    if relative.contains("://") {
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
