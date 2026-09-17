//! Fetching, and the policy that governs it.
//!
//! TLS comes from `rustls` via `ureq` — never written here (ADR-0007). The
//! interesting part of this crate is [`policy`], which is where "without the
//! slop" stops being a slogan and becomes a rule.

pub mod encoding;
pub mod policy;
pub mod site;
pub mod tls;

pub use policy::{
    Exception, LOCAL_SITE, Origin, Policy, Refusal, RequestKind, Scheme, file_url, is_drive_path,
    parse_url, resolve,
};
pub use site::site;

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

/// Count of third-party subresource requests the policy refused.
///
/// The other side of the pair. On its own, `THIRD_PARTY_REQUESTS` reads the
/// same at zero whether the policy is working or the page asked for nothing:
/// "no third-party request left this process" and "no third-party request was
/// ever made" are different facts, and only one of them is evidence. Counting
/// refusals separates them, and it is what lets the chrome say how much of a
/// page was withheld rather than only that none of it escaped (issue #118).
///
/// Only [`Refusal::ThirdParty`] is counted. A malformed URL and an unsupported
/// scheme are the page being wrong about itself rather than the policy holding
/// a line, and folding them in here would inflate the number the reader is
/// being shown with things nobody could allow even if they wanted to.
static THIRD_PARTY_REFUSALS: AtomicUsize = AtomicUsize::new(0);

/// How many third-party subresource requests have been issued this process.
pub fn third_party_request_count() -> usize {
    THIRD_PARTY_REQUESTS.load(Ordering::Relaxed)
}

/// Resets the count. For tests and the budget harness.
pub fn reset_third_party_request_count() {
    THIRD_PARTY_REQUESTS.store(0, Ordering::Relaxed);
}

/// How many third-party subresource requests have been refused this process.
pub fn third_party_refusal_count() -> usize {
    THIRD_PARTY_REFUSALS.load(Ordering::Relaxed)
}

/// Resets the count. For tests and the budget harness.
pub fn reset_third_party_refusal_count() {
    THIRD_PARTY_REFUSALS.store(0, Ordering::Relaxed);
}

/// Records a refusal, if it was the third-party rule that did it.
///
/// Public because the fetch this crate would have counted does not always
/// happen here: the sandbox parent checks a whole batch of URLs against the
/// policy before it fetches any of them, so a refused subresource is dropped
/// one layer up and never reaches [`Fetcher::fetch_raw`]. That path has to
/// count for itself, and the two never see the same URL — anything refused in
/// the batch pre-pass is excluded from what is fetched.
pub fn count_refusal(refusal: &Refusal) {
    if matches!(refusal, Refusal::ThirdParty { .. }) {
        THIRD_PARTY_REFUSALS.fetch_add(1, Ordering::Relaxed);
    }
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
    /// The redirects went on past [`MAX_HOPS`].
    ///
    /// Its own variant rather than a transport error because it is a refusal:
    /// a chain that long is a loop or a server arguing with itself, and either
    /// way stopping is the right answer rather than a failure to report.
    TooManyRedirects,
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
            FetchError::TooManyRedirects => write!(f, "too many redirects"),
        }
    }
}

impl std::error::Error for FetchError {}

/// Largest form body this browser will send.
///
/// Not a limit any form needs: the era's are a few hundred bytes and a long
/// comment is a few thousand. It is a bound on what a *compromised renderer*
/// can push out of this machine in one request — the one direction the
/// boundary could not otherwise measure, since a body, unlike a URL, has no
/// length anything agrees on.
pub const MAX_FORM_BYTES: u64 = 1024 * 1024;

/// Largest response we will read into memory.
///
/// A browser must not let a hostile server exhaust its memory, and 32 MiB is
/// far beyond any document this engine renders.
pub const MAX_BODY_BYTES: u64 = 32 * 1024 * 1024;

/// How many redirects one request may follow.
///
/// Redirects are followed here rather than by `ureq`, which would happily do it
/// and is configured not to. Two things have to happen at every hop and only
/// this side can do either: the policy has to be asked again, because a
/// same-site URL that redirects to a tracker is a third-party request the rule
/// would otherwise never see; and where the chain *stopped* has to travel back,
/// because that — not what was typed — is what a relative link on the page
/// resolves against.
pub const MAX_HOPS: u32 = 8;

/// What one request came back with.
enum Landed {
    /// A redirect, carrying its `Location` exactly as the server wrote it.
    Elsewhere(String),
    /// A response with a body.
    Here {
        /// What the server answered with.
        status: u16,
        /// The bytes.
        bytes: Vec<u8>,
        /// Its `Content-Type`, when there was one.
        content_type: Option<String>,
        /// Whether it may outlive the page that asked (ADR-0018).
        storable: bool,
    },
}

/// Where a request ended up, and what was there.
///
/// The origin and path are the ones the chain *landed* on. A page served after
/// a redirect belongs to where it came from, not to where it was asked for:
/// `hackernews.com` redirects to `news.ycombinator.com`, and a browser that
/// kept the first resolves every relative link on the page against a host that
/// only knows how to redirect again — which is how `item?id=49737849` arrives
/// as `/item` and Hacker News answers "No such item."
struct Arrived {
    origin: Origin,
    path: String,
    status: u16,
    bytes: Vec<u8>,
    content_type: Option<String>,
    trust: Trust,
    storable: bool,
}

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
    /// What the server answered with.
    ///
    /// 200 for anything that had no status to have — a local file, a form's
    /// answer. Carried because a navigation now *keeps* the body of a 4xx or a
    /// 5xx (#203), and the reader has to be able to tell a site's own "not
    /// found" from the page they asked for.
    pub status: u16,
    /// Whether the response allows this to be kept once the page that asked
    /// for it is gone (ADR-0018).
    ///
    /// `false` where the server said `Cache-Control: no-store` or `Pragma:
    /// no-cache`. The second matters more here than it would in a modern
    /// browser, because this browser is aimed at the servers of the HTTP/1.0
    /// era and `Pragma` is what they actually send.
    ///
    /// Nothing else about caching is parsed. `max-age`, `Expires` and
    /// revalidation are a freshness model, and a cache that lives for one run
    /// does not need one — it needs to know what it is not allowed to keep.
    pub storable: bool,
}

/// Whether a response's headers allow it to be stored (ADR-0018).
///
/// Both header values are comma-separated lists of directives, and a directive
/// may carry an argument this does not read — so each is split rather than
/// matched whole, or `Cache-Control: max-age=0, no-store` would look like
/// nothing at all.
fn storable(cache_control: Option<&str>, pragma: Option<&str>) -> bool {
    let says = |value: Option<&str>, directive: &str| {
        value.is_some_and(|value| {
            value.split(',').any(|part| {
                part.split('=')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .eq_ignore_ascii_case(directive)
            })
        })
    };
    !says(cache_control, "no-store") && !says(pragma, "no-cache")
}

/// Fetches resources subject to a [`Policy`].
#[derive(Debug, Default, Clone)]
pub struct Fetcher {
    /// The policy applied to every request.
    pub policy: Policy,
}

impl Fetcher {
    /// Makes a request, following redirects and asking the policy at each one.
    ///
    /// The policy is applied *before* every hop rather than to the URL the
    /// caller named. Applying it once at the front is what a browser does when
    /// it lets `ureq` follow redirects for it, and it leaves the rule with a
    /// door in it: a subresource on the page's own site that answers `302
    /// Location: https://tracker.example.net/pixel.gif` gets the request made
    /// and the rule never sees the host it was made to. ADR-0006's budget says
    /// *no third-party request was ever made*, and that has to be true of the
    /// second request as much as the first.
    ///
    /// Where the chain stopped comes back in [`Arrived`], because that is the
    /// document's real origin — for the policy, for the chrome, and above all
    /// for resolving the links on the page.
    fn follow(
        &self,
        url: &str,
        document: Option<&Origin>,
        kind: RequestKind,
    ) -> Result<Arrived, FetchError> {
        let (mut origin, mut path) = parse_url(url).map_err(FetchError::Refused)?;
        if let Err(refusal) = self.policy.check(document, &origin, kind) {
            count_refusal(&refusal);
            return Err(FetchError::Refused(refusal));
        }
        count_if_third_party(document, &origin, kind);

        // A file has no transport and nothing to redirect with.
        if origin.scheme == Scheme::File {
            let bytes = read_file(&path)?;
            return Ok(Arrived {
                origin,
                path,
                // A file that opened is the file. There is no status to have,
                // and 200 is the honest stand-in: the thing was served.
                status: 200,
                bytes,
                content_type: None,
                trust: Trust::NotEncrypted,
                storable: true,
            });
        }

        let mut url = url.to_owned();
        // Sticky rather than taken from the last hop: if any step of the chain
        // needed this computer's own roots, the reader is behind something
        // intercepting the connection and the chrome has to say so (ADR-0015).
        let mut trust = Trust::Public;
        for _ in 0..=MAX_HOPS {
            let (landed, hop) = fetch_http(&url)?;
            if hop == Trust::LocalRoot {
                trust = Trust::LocalRoot;
            }
            let location = match landed {
                Landed::Here {
                    status,
                    bytes,
                    content_type,
                    storable,
                } => {
                    // A 4xx or a 5xx means opposite things to the two kinds of
                    // request, so this is the one place the kind decides what a
                    // status *is* (#203).
                    //
                    // For a navigation it is a page. The body is the site's own
                    // "not found", or a proxy's block notice, or the sentence a
                    // server wrote to explain itself, and throwing it away to
                    // show `server returned 403` instead tells the reader less
                    // than the server did.
                    //
                    // For a subresource it is a failure, exactly as before. A
                    // 404's HTML body is not a stylesheet, and handing it to the
                    // CSS parser because the status was ignored would apply a
                    // page of garbage rules to the document.
                    if status >= 400 && kind != RequestKind::Navigation {
                        return Err(FetchError::Status { code: status });
                    }
                    return Ok(Arrived {
                        origin,
                        path,
                        status,
                        bytes,
                        content_type,
                        trust,
                        storable,
                    });
                }
                Landed::Elsewhere(location) => location,
            };

            url = resolve(&origin, &path, &location);
            let (next, next_path) = parse_url(&url).map_err(FetchError::Refused)?;
            // Asked before the guard below because a navigation is exempt from
            // the policy entirely, and "follow this redirect to the disk" is
            // not something a navigation may do either. A page cannot be
            // allowed to reach a local file by bouncing off a server.
            if next.scheme == Scheme::File {
                let refusal = Refusal::LocalFile;
                count_refusal(&refusal);
                return Err(FetchError::Refused(refusal));
            }
            if let Err(refusal) = self.policy.check(document, &next, kind) {
                count_refusal(&refusal);
                return Err(FetchError::Refused(refusal));
            }
            count_if_third_party(document, &next, kind);
            (origin, path) = (next, next_path);
        }
        Err(FetchError::TooManyRedirects)
    }

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
        let arrived = self.follow(url, document, kind)?;
        // Not UTF-8 by assumption: most of the surviving old web is not, and
        // guessing wrong turns every accented letter into a replacement
        // character (ADR-0004).
        let (body, encoding, encoding_source) =
            encoding::decode_document(&arrived.bytes, arrived.content_type.as_deref());
        Ok(Resource {
            body,
            bytes: arrived.bytes,
            origin: arrived.origin,
            path: arrived.path,
            encoding: encoding.name(),
            encoding_source,
        })
    }

    /// Sends a form and fetches what comes back (#110).
    ///
    /// The one request this browser makes that carries data *up*. Everything
    /// else here asks a server for something; this hands it something, which is
    /// a different kind of act and is why it is a separate method rather than a
    /// flag on `fetch_raw`: a caller has to mean it.
    ///
    /// Three rules, all enforced here rather than by whoever calls:
    ///
    /// * **Network schemes only.** There is nothing to post to a `file:` URL,
    ///   and a page that asked to would be asking to write to the disk.
    /// * **Bounded.** A body is not a URL and has no length anyone has ever
    ///   agreed on, so [`MAX_FORM_BYTES`] is what leaves this machine at most.
    ///   The cap is about what a *compromised renderer* can push, not about
    ///   what a form needs — the era's forms are a few hundred bytes.
    /// * **Navigation, so the third-party rule does not apply.** Posting to
    ///   another host is what a form to another host means, and refusing it
    ///   would break the sign-in on half the surviving web. ADR-0006 is about
    ///   what a page loads *without being asked*, and this was asked for.
    pub fn post(&self, url: &str, body: &str) -> Result<Fetched, FetchError> {
        let (origin, path) = parse_url(url).map_err(FetchError::Refused)?;
        if origin.scheme == Scheme::File {
            return Err(FetchError::Refused(Refusal::UnsupportedScheme {
                scheme: "file".to_owned(),
            }));
        }
        if body.len() as u64 > MAX_FORM_BYTES {
            return Err(FetchError::TooLarge);
        }
        self.policy
            .check(None, &origin, RequestKind::Navigation)
            .map_err(FetchError::Refused)?;

        let (bytes, content_type, trust, landed_on, status) = post_http(url, body)?;
        // Where the form's answer actually came from. A `post` is usually
        // answered by a redirect to the page to show, and that page's links
        // resolve against where it was served rather than against the address
        // the form was sent to.
        let (origin, path) = parse_url(&landed_on).unwrap_or((origin, path));
        Ok(Fetched {
            body: bytes,
            content_type,
            origin,
            path,
            trust,
            status,
            // A response to a form submission is a page, not a subresource, and
            // never reaches the cache. Saying so here rather than relying on
            // that: the answer to "may this be kept?" for a POST is no.
            storable: false,
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
        // A local file has no headers to say it may not be kept. It is still
        // never served to a second document, because the policy refuses that
        // outright (`Refusal::LocalFile`) — a stronger rule than that flag, and
        // applied before the cache is consulted at all.
        let arrived = self.follow(url, document, kind)?;
        Ok(Fetched {
            body: arrived.bytes,
            content_type: arrived.content_type,
            origin: arrived.origin,
            path: arrived.path,
            trust: arrived.trust,
            status: arrived.status,
            storable: arrived.storable,
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
fn fetch_http(url: &str) -> Result<(Landed, Trust), FetchError> {
    match get(tls::agent(), url) {
        Ok(landed) => Ok((landed, Trust::Public)),
        Err(error) => {
            if !matches!(tls::classify(&error), Some(tls::Handshake::UntrustedRoot)) {
                return Err(into_fetch_error(error));
            }
            let landed = get(tls::platform_agent(), url).map_err(into_fetch_error)?;
            Ok((landed, Trust::LocalRoot))
        }
    }
}

/// One form, through the same two-agent dance `fetch_http` does.
///
/// Also reports the URL the answer was finally served from, which is not the
/// one it was sent to whenever the server answers a `post` with a redirect —
/// which is what a server that does not want the form re-sent on reload does,
/// and therefore what most of them do.
fn post_http(
    url: &str,
    body: &str,
) -> Result<(Vec<u8>, Option<String>, Trust, String, u16), FetchError> {
    match send(tls::agent(), url, body) {
        Ok((bytes, content_type, landed_on, status)) => {
            Ok((bytes, content_type, Trust::Public, landed_on, status))
        }
        Err(error) => {
            if !matches!(tls::classify(&error), Some(tls::Handshake::UntrustedRoot)) {
                return Err(into_fetch_error(error));
            }
            let (bytes, content_type, landed_on, status) =
                send(tls::platform_agent(), url, body).map_err(into_fetch_error)?;
            Ok((bytes, content_type, Trust::LocalRoot, landed_on, status))
        }
    }
}

/// One form sent through a given agent.
fn send(
    agent: &ureq::Agent,
    url: &str,
    body: &str,
) -> Result<(Vec<u8>, Option<String>, String, u16), ureq::Error> {
    use ureq::ResponseExt;

    let response = agent
        .post(url)
        .content_type("application/x-www-form-urlencoded")
        .send(body)?;

    // A form's redirects are left to `ureq`, unlike a plain request's. There is
    // nothing for this side to decide at each hop: the third-party rule does
    // not apply to a submission (ADR-0006), and the method handling a redirect
    // needs — `post` becoming `get` on a 303 and staying a `post` on a 307 — is
    // exactly what the library already does correctly.
    let landed_on = response.get_uri().to_string();

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    // A form answered with a 4xx or a 5xx keeps its body for the same reason a
    // navigation does: "your password was wrong" is a page, and the status
    // alone does not say it (#203).
    let status = response.status().as_u16();
    let bytes = response
        .into_body()
        .with_config()
        .limit(MAX_BODY_BYTES)
        .read_to_vec()?;
    Ok((bytes, content_type, landed_on, status))
}

/// One request through a given agent, and one hop only.
///
/// `max_redirects(0)` is what makes this one hop: `ureq` hands the redirect
/// back as an ordinary response instead of chasing it. The chasing is
/// [`Fetcher::follow`]'s, because a redirect is a decision — the policy has to
/// see the host at the other end, and the caller has to learn where it stopped.
fn get(agent: &ureq::Agent, url: &str) -> Result<Landed, ureq::Error> {
    // No custom User-Agent games: this browser does not run scripts, and
    // pretending otherwise to get the script path served would produce exactly
    // the silent breakage ADR-0003 rejects.
    let response = agent.get(url).config().max_redirects(0).build().call()?;

    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };

    // A 3xx without a `Location` names nowhere to go, so it is whatever it
    // came with — usually nothing, which is a blank page rather than a hang.
    if response.status().is_redirection()
        && let Some(location) = header("location")
    {
        return Ok(Landed::Elsewhere(location));
    }

    let content_type = header("content-type");
    let keep = storable(
        header("cache-control").as_deref(),
        header("pragma").as_deref(),
    );

    let status = response.status().as_u16();
    let bytes = response
        .into_body()
        .with_config()
        .limit(MAX_BODY_BYTES)
        .read_to_vec()?;
    Ok(Landed::Here {
        status,
        bytes,
        content_type,
        storable: keep,
    })
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
    fn no_store_and_pragma_no_cache_stop_a_response_being_kept() {
        assert!(storable(None, None), "nothing said means keep");
        assert!(!storable(Some("no-store"), None));
        assert!(!storable(None, Some("no-cache")));
        // The one this browser is aimed at: an HTTP/1.0 server sends `Pragma`
        // and nothing else.
        assert!(!storable(Some("max-age=0"), Some("no-cache")));
    }

    #[test]
    fn a_directive_in_a_list_still_counts() {
        // Both headers are comma-separated lists, and matching the value whole
        // would read `max-age=0, no-store` as saying nothing at all.
        assert!(!storable(Some("max-age=0, no-store"), None));
        assert!(!storable(Some("public, No-Store, must-revalidate"), None));
        assert!(
            storable(Some("no-cache"), None),
            "`Cache-Control: no-cache` is revalidate-before-use, not do-not-store",
        );
    }

    #[test]
    fn a_directive_that_merely_starts_the_same_does_not_count() {
        assert!(storable(Some("no-store-ish"), None));
        assert!(storable(Some("no-transform"), None));
    }

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
        // A delta rather than a reset: the counter is process-wide and the test
        // binary is not. This is the only test here that refuses a third party,
        // so nothing else can move it underneath us — but resetting it would
        // stamp on whatever else was counting, which is a different bug and a
        // much harder one to see.
        let before = third_party_refusal_count();
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
        assert_eq!(
            third_party_refusal_count(),
            before + 1,
            "the refusal was not counted, so the chrome has nothing to report (#118)"
        );
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
