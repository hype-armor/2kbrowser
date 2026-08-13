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

/// A navigation's bytes, undecoded, with what is known about how they arrived.
#[derive(Debug, Clone)]
pub struct Fetched {
    /// The body, exactly as it came off the wire.
    pub body: Vec<u8>,
    /// The `Content-Type` header, when there was one.
    ///
    /// Travels with the bytes because on a page that declares its encoding
    /// nowhere else, this is the only thing that knows.
    pub content_type: Option<String>,
    /// Where it came from.
    pub origin: Origin,
    /// Its path within that origin.
    pub path: String,
    /// How its certificate chain was verified.
    pub trust: Trust,
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

        let (bytes, content_type, _) = match origin.scheme {
            // A file has no transport, so its encoding comes from the document
            // or the default.
            Scheme::File => (read_file(&path)?, None, Trust::NotEncrypted),
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
    ) -> Result<Fetched, FetchError> {
        let (origin, path) = parse_url(url).map_err(FetchError::Refused)?;
        self.policy
            .check(document, &origin, kind)
            .map_err(FetchError::Refused)?;
        count_if_third_party(document, &origin, kind);

        let (body, content_type, trust) = match origin.scheme {
            Scheme::File => (read_file(&path)?, None, Trust::NotEncrypted),
            Scheme::Http | Scheme::Https => fetch_http(url)?,
        };
        Ok(Fetched {
            body,
            content_type,
            origin,
            path,
            trust,
        })
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

/// How the connection's certificate chain was verified.
///
/// Carried rather than discarded, because the difference is the whole reason
/// ADR-0015 allows the second attempt at all: a chain nothing public signed is
/// a fact about who can read the traffic, and this browser marks facts like
/// that rather than assuming the reader will guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// No certificate was involved: a local file, or plain HTTP.
    ///
    /// Not "safe" and not "unsafe" — the scheme already says what it says, and
    /// the chrome marks plain HTTP on its own.
    NotEncrypted,
    /// Verified against Mozilla's roots. The ordinary case.
    Public,
    /// Verified only against a root in this computer's own trust store.
    ///
    /// Which means something on this machine or this network is standing
    /// between the browser and the site and is able to read what passes.
    /// Usually a corporate proxy, sometimes an antivirus, occasionally an
    /// attacker — the browser cannot tell those apart and does not pretend to.
    LocalRoot,
}

/// Fetches over HTTP, returning the body, the `Content-Type` it was served
/// with — the header being what decides the encoding when the page itself does
/// not say — and how its certificate was verified.
///
/// Two attempts at most, and the second only for one specific refusal. See
/// [`Trust`] and ADR-0015: a chain nothing public signed is retried against
/// this computer's own roots so that a machine behind an intercepting proxy has
/// a working browser, and the fact that it took local roots travels back so the
/// chrome can say so. Any other certificate failure — expired, wrong name — is
/// final, because those are wrong whoever signed them.
fn fetch_http(url: &str) -> Result<(Vec<u8>, Option<String>, Trust), FetchError> {
    match get(&tls::agent(), url) {
        Ok((bytes, content_type)) => Ok((bytes, content_type, Trust::Public)),
        Err(error) => {
            if !matches!(tls::classify(&error), Some(tls::Handshake::UntrustedRoot)) {
                return Err(into_fetch_error(error));
            }
            let (bytes, content_type) =
                get(&tls::platform_agent(), url).map_err(into_fetch_error)?;
            Ok((bytes, content_type, Trust::LocalRoot))
        }
    }
}

/// One request through a given agent.
fn get(agent: &ureq::Agent, url: &str) -> Result<(Vec<u8>, Option<String>), ureq::Error> {
    // No custom User-Agent games: this browser does not run scripts, and
    // pretending otherwise to get the script path served would produce exactly
    // the silent breakage ADR-0003 rejects.
    let response = agent.get(url).call()?;

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    let bytes = response
        .into_body()
        .with_config()
        .limit(MAX_BODY_BYTES)
        .read_to_vec()?;
    Ok((bytes, content_type))
}

/// Turns a `ureq` failure into one this browser can explain.
fn into_fetch_error(error: ureq::Error) -> FetchError {
    if let ureq::Error::StatusCode(code) = error {
        return FetchError::Status { code };
    }
    // Asked before falling back to a generic transport error, so that the
    // failures this browser causes on purpose say so.
    match tls::classify(&error) {
        Some(tls::Handshake::LegacyVersion) => FetchError::LegacyTls,
        Some(tls::Handshake::UntrustedRoot) => {
            FetchError::Certificate("nothing this computer trusts signed it".to_owned())
        }
        Some(tls::Handshake::Certificate(reason)) => FetchError::Certificate(reason),
        None => FetchError::Transport(error.to_string()),
    }
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
