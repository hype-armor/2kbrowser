//! Fetching, and the policy that governs it.
//!
//! TLS comes from `rustls` via `ureq` — never written here (ADR-0007). The
//! interesting part of this crate is [`policy`], which is where "without the
//! slop" stops being a slogan and becomes a rule.

pub mod encoding;
pub mod policy;
pub mod tls;

pub use policy::{
    Origin, Policy, Refusal, RequestKind, Scheme, file_url, is_drive_path, parse_url, resolve,
};

use std::sync::atomic::{AtomicUsize, Ordering};

/// Count of third-party subresource requests that reached the network.
///
/// ADR-0006's rule is that there are none unless the user has allowed a host,
/// and the budget harness asserts exactly that. Counted here, at the point a
/// request is actually issued, rather than inside `Policy::check`: a bug that
/// stopped the policy refusing would still be counted, which is the only
/// version of this number worth having.
///
/// A request that is issued and then *fails* still counts. That distinction is
/// the whole reason this exists — a page full of unreachable ad hosts loads no
/// images whether the policy works or not, so success counts prove nothing.
static THIRD_PARTY_REQUESTS: AtomicUsize = AtomicUsize::new(0);

/// How many third-party subresource requests have been issued this process.
pub fn third_party_request_count() -> usize {
    THIRD_PARTY_REQUESTS.load(Ordering::Relaxed)
}

/// Resets the count. For tests and the budget harness.
pub fn reset_third_party_request_count() {
    THIRD_PARTY_REQUESTS.store(0, Ordering::Relaxed);
}

/// Records a request that the policy let through, if it left the origin.
fn count_if_third_party(document: Option<&Origin>, target: &Origin, kind: RequestKind) {
    if kind != RequestKind::Subresource {
        return;
    }
    let Some(document) = document else { return };
    // A file: subresource never leaves the machine, so it is not what this
    // counts — but a *network* request from a file: document does leave, and
    // has no origin to be first-party to.
    if target.scheme == Scheme::File {
        return;
    }
    if document.scheme == Scheme::File || !document.is_same_site(target) {
        THIRD_PARTY_REQUESTS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Anything that can go wrong fetching a resource.
#[derive(Debug)]
pub enum FetchError {
    /// The policy refused the request.
    Refused(Refusal),
    /// The transport failed.
    Transport(String),
    /// The server offered no TLS version this browser accepts.
    ///
    /// Not a failure so much as ADR-0013 being enforced, and worth its own
    /// variant for exactly that reason: without one it reached the reader as an
    /// unexplained network error, indistinguishable from a server that was
    /// down. A refusal nobody can recognise is indistinguishable from a bug.
    LegacyTls,
    /// The server's certificate did not check out.
    ///
    /// Separate from [`FetchError::LegacyTls`] because the two mean opposite
    /// things: one says the site is too old to talk to, the other says
    /// something is wrong with its identity.
    Certificate(String),
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
            // "refused", not "failed": someone reading this should be able to
            // tell that the browser worked exactly as intended. Short, because
            // it also has to fit in the chrome bar beside a URL, and the words
            // that matter are at the front so truncation costs the least.
            FetchError::LegacyTls => {
                write!(
                    f,
                    "refused: this site's TLS is too old — needs 1.2 or newer"
                )
            }
            FetchError::Certificate(reason) => {
                write!(
                    f,
                    "refused: this site's certificate is not valid ({reason})"
                )
            }
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
    /// Body decoded as text. Empty for resources fetched as bytes.
    pub body: String,
    /// Raw body bytes.
    ///
    /// Images are not text: decoding them through `String` would mangle every
    /// byte that is not valid UTF-8, which is most of a PNG.
    pub bytes: Vec<u8>,
    /// Origin it came from, for the policy to judge subresources against.
    pub origin: Origin,
    /// Path it was fetched from, which relative subresource URLs resolve
    /// against.
    pub path: String,
    /// Name of the encoding the body was decoded from.
    pub encoding: &'static str,
    /// How that encoding was decided.
    pub encoding_source: encoding::EncodingSource,
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
#[derive(Debug, Default, Clone)]
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

        count_if_third_party(document, &origin, kind);

        let (bytes, content_type) = match origin.scheme {
            // A file has no transport, so its encoding comes from the document
            // or the default.
            Scheme::File => (read_file(&path)?, None),
            Scheme::Http | Scheme::Https => fetch_http(url)?,
        };
        // Not UTF-8 by assumption: most of the surviving old web is not, and
        // guessing wrong turns every accented letter into a replacement
        // character (ADR-0004).
        let (body, encoding, encoding_source) =
            encoding::decode_document(&bytes, content_type.as_deref());
        Ok(Resource {
            body,
            bytes,
            origin,
            path,
            encoding: encoding.name(),
            encoding_source,
        })
    }

    /// Fetches a URL without decoding it, keeping the `Content-Type`.
    ///
    /// What a navigation uses now that decoding happens in the renderer child
    /// (ADR-0012): the parent must not turn a stranger's bytes into text, so it
    /// hands over the bytes and the header that says how to read them.
    pub fn fetch_raw(
        &self,
        url: &str,
        document: Option<&Origin>,
        kind: RequestKind,
    ) -> Result<(Vec<u8>, Option<String>, Origin, String), FetchError> {
        let (origin, path) = parse_url(url).map_err(FetchError::Refused)?;
        self.policy
            .check(document, &origin, kind)
            .map_err(FetchError::Refused)?;
        count_if_third_party(document, &origin, kind);

        let (bytes, content_type) = match origin.scheme {
            Scheme::File => (read_file(&path)?, None),
            Scheme::Http | Scheme::Https => fetch_http(url)?,
        };
        Ok((bytes, content_type, origin, path))
    }

    /// Fetches a URL, keeping only the raw bytes.
    pub fn fetch_bytes(
        &self,
        url: &str,
        document: Option<&Origin>,
        kind: RequestKind,
    ) -> Result<Vec<u8>, FetchError> {
        let (origin, path) = parse_url(url).map_err(FetchError::Refused)?;
        self.policy
            .check(document, &origin, kind)
            .map_err(FetchError::Refused)?;
        count_if_third_party(document, &origin, kind);

        match origin.scheme {
            Scheme::File => read_file(&path),
            // The content type is irrelevant here: bytes fetched as bytes are
            // images, and an image's encoding is its own format's business.
            Scheme::Http | Scheme::Https => fetch_http(url).map(|(bytes, _)| bytes),
        }
    }
}

fn read_file(url_path: &str) -> Result<Vec<u8>, FetchError> {
    let path = policy::to_file_path(url_path);
    let metadata = std::fs::metadata(path).map_err(FetchError::Io)?;
    if metadata.len() > MAX_BODY_BYTES {
        return Err(FetchError::TooLarge);
    }
    std::fs::read(path).map_err(FetchError::Io)
}

/// Fetches over HTTP, returning the body and the `Content-Type` it was served
/// with — the header being what decides the encoding when the page itself does
/// not say.
fn fetch_http(url: &str) -> Result<(Vec<u8>, Option<String>), FetchError> {
    // No custom User-Agent games: this browser does not run scripts, and
    // pretending otherwise to get the script path served would produce exactly
    // the silent breakage ADR-0003 rejects.
    // Through our own agent, not `ureq::get`, so the TLS posture is the one
    // `tls::agent` states rather than whatever the library defaults to at the
    // version we happen to be pinned at.
    let response = tls::agent().get(url).call().map_err(|error| match error {
        ureq::Error::StatusCode(code) => FetchError::Status { code },
        // Asked before falling back to a generic transport error, so that the
        // one failure this browser causes on purpose says so.
        other => match tls::classify(&other) {
            Some(tls::Handshake::LegacyVersion) => FetchError::LegacyTls,
            Some(tls::Handshake::Certificate(reason)) => FetchError::Certificate(reason),
            None => FetchError::Transport(other.to_string()),
        },
    })?;

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    let bytes = response
        .into_body()
        .with_config()
        .limit(MAX_BODY_BYTES)
        .read_to_vec()
        .map_err(|error| FetchError::Transport(error.to_string()))?;
    Ok((bytes, content_type))
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
        let url = file_url(&path);
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
