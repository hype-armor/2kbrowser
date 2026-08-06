//! Fetching, and the policy that governs it.
//!
//! TLS comes from `rustls` via `ureq` — never written here (ADR-0007). The
//! interesting part of this crate is [`policy`], which is where "without the
//! slop" stops being a slogan and becomes a rule.

pub mod policy;

pub use policy::{Origin, Policy, Refusal, RequestKind, Scheme, parse_url};

/// Anything that can go wrong fetching a resource.
#[derive(Debug)]
pub enum FetchError {
    /// The policy refused the request.
    Refused(Refusal),
    /// The transport failed.
    Transport(String),
    /// The server answered with a non-success status.
    Status {
        /// HTTP status code.
        code: u16,
    },
    /// A local file could not be read.
    Io(std::io::Error),
    /// The body was larger than [`MAX_BODY_BYTES`].
    TooLarge,
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Refused(refusal) => write!(f, "{refusal}"),
            FetchError::Transport(message) => write!(f, "transport error: {message}"),
            FetchError::Status { code } => write!(f, "server returned {code}"),
            FetchError::Io(error) => write!(f, "{error}"),
            FetchError::TooLarge => write!(f, "response exceeded the size limit"),
        }
    }
}

impl std::error::Error for FetchError {}

/// Largest response we will read into memory.
///
/// A browser must not let a hostile server exhaust its memory, and 32 MiB is
/// far beyond any document this engine renders.
pub const MAX_BODY_BYTES: u64 = 32 * 1024 * 1024;

/// A fetched resource.
#[derive(Debug, Clone)]
pub struct Resource {
    /// Decoded body.
    pub body: String,
    /// Origin it came from, for the policy to judge subresources against.
    pub origin: Origin,
}

impl Resource {
    /// Whether this was retrieved over an authenticated channel.
    ///
    /// The chrome must surface this rather than assume it (ADR-0006).
    pub fn is_authenticated(&self) -> bool {
        self.origin.scheme.is_authenticated()
    }
}

/// Fetches resources subject to a [`Policy`].
#[derive(Debug, Default)]
pub struct Fetcher {
    /// The policy applied to every request.
    pub policy: Policy,
}

impl Fetcher {
    /// Fetches a URL.
    ///
    /// `document` is the origin of the page making the request, or `None` for a
    /// top-level navigation.
    pub fn fetch(
        &self,
        url: &str,
        document: Option<&Origin>,
        kind: RequestKind,
    ) -> Result<Resource, FetchError> {
        let (origin, path) = parse_url(url).map_err(FetchError::Refused)?;
        self.policy
            .check(document, &origin, kind)
            .map_err(FetchError::Refused)?;

        let body = match origin.scheme {
            Scheme::File => read_file(&path)?,
            Scheme::Http | Scheme::Https => fetch_http(url)?,
        };
        Ok(Resource { body, origin })
    }
}

fn read_file(path: &str) -> Result<String, FetchError> {
    let metadata = std::fs::metadata(path).map_err(FetchError::Io)?;
    if metadata.len() > MAX_BODY_BYTES {
        return Err(FetchError::TooLarge);
    }
    std::fs::read_to_string(path).map_err(FetchError::Io)
}

fn fetch_http(url: &str) -> Result<String, FetchError> {
    // No custom User-Agent games: this browser does not run scripts, and
    // pretending otherwise to get the script path served would produce exactly
    // the silent breakage ADR-0003 rejects.
    let response = ureq::get(url).call().map_err(|error| match error {
        ureq::Error::StatusCode(code) => FetchError::Status { code },
        other => FetchError::Transport(other.to_string()),
    })?;

    response
        .into_body()
        .with_config()
        .limit(MAX_BODY_BYTES)
        .read_to_string()
        .map_err(|error| FetchError::Transport(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_local_file() {
        let dir = std::env::temp_dir().join("2kbrowser-net-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("page.html");
        std::fs::write(&path, "<p>hello</p>").expect("write");

        let fetcher = Fetcher::default();
        let url = format!("file://{}", path.display());
        let resource = fetcher
            .fetch(&url, None, RequestKind::Navigation)
            .expect("fetch succeeds");
        assert_eq!(resource.body, "<p>hello</p>");
        assert!(!resource.is_authenticated(), "file: is not authenticated");
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_panic() {
        let fetcher = Fetcher::default();
        let result = fetcher.fetch(
            "file:///nonexistent/page.html",
            None,
            RequestKind::Navigation,
        );
        assert!(matches!(result, Err(FetchError::Io(_))));
    }

    #[test]
    fn the_policy_runs_before_any_network_access() {
        // The refusal must come from the policy, not from a failed connection:
        // a blocked request should never touch the network at all.
        let fetcher = Fetcher::default();
        let document = parse_url("https://example.com/").expect("parses").0;
        let result = fetcher.fetch(
            "https://tracker.invalid/pixel.gif",
            Some(&document),
            RequestKind::Subresource,
        );
        match result {
            Err(FetchError::Refused(Refusal::ThirdParty { host })) => {
                assert_eq!(host, "tracker.invalid");
            }
            other => panic!("expected a policy refusal, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_schemes_are_refused_before_dispatch() {
        let fetcher = Fetcher::default();
        let result = fetcher.fetch("ftp://example.com/x", None, RequestKind::Navigation);
        assert!(matches!(
            result,
            Err(FetchError::Refused(Refusal::UnsupportedScheme { .. }))
        ));
    }
}
