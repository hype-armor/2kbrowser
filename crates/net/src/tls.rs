//! The TLS posture, stated rather than inherited.
//!
//! The engine is of its era. The security is not, and this is the file that
//! says so: 2kbrowser renders the web of 2000 using the tools of now.
//!
//! # What is refused, and why there is no opt-in
//!
//! TLS 1.0 and 1.1 are refused. Not marked, not per-site, not behind a
//! confirmation — refused. Old sites that support nothing newer do not load,
//! and that is the intended outcome rather than a limitation to work around.
//!
//! This overrules the earlier answer on
//! [issue #3](https://github.com/hype-armor/2kbrowser/issues/3), which had
//! chosen a marked opt-in downgrade by analogy with how ADR-0006 treats plain
//! HTTP. The analogy does not hold. Plain HTTP is *visibly* unauthenticated and
//! the reader can price that in; a downgraded TLS session looks exactly like a
//! working one, and any marking has to compete with a padlock-shaped intuition
//! that took twenty years to build. Refusing is the only version of this that
//! cannot be misread.
//!
//! There is also nothing to opt into. `rustls` has no TLS 1.0 or 1.1 code in it
//! at all — they were removed upstream by policy, not hidden behind a feature —
//! so allowing them would mean adding a second TLS stack. That would be taking
//! on a dependency in order to be less safe.
//!
//! # Why this file exists at all, given rustls already refuses
//!
//! Because "it happens to be true" and "it is guaranteed" are different
//! properties, and only one of them survives a dependency bump. `ureq` can be
//! built against `native-tls` instead, whose accepted versions come from
//! whatever the platform decides — and on an older platform that includes TLS
//! 1.0. It can also be told to skip certificate verification entirely. Both are
//! one line away by default, so both are asserted here.
//!
//! # Whose roots
//!
//! Mozilla's, first and by default. Then — and only when nothing in that store
//! signed the chain — this computer's own, with the result marked in the chrome
//! for as long as the page is on screen (ADR-0015). That second attempt exists
//! because refusing every chain a proxy signed did not make anyone safer: it
//! made the browser unusable on a corporate network, and an unusable browser is
//! one people close.
//!
//! Nothing in this file relaxes anything. If a future change wants to, it has
//! to edit these tests, which is a reviewable diff rather than a quiet default.

use std::sync::OnceLock;

use ureq::Agent;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

/// The agent every request goes through.
///
/// One for the process, not one per request. An `Agent` owns the connection
/// pool, so building a fresh one each time threw the pool away before anything
/// could use it twice: every subresource paid its own TCP connection and its
/// own TLS handshake to a host the previous one had just finished talking to.
/// A page with twenty images meant twenty handshakes.
///
/// Shared across threads deliberately. Each session's conversation runs on a
/// thread of its own and they all fetch through here, which is the arrangement
/// a connection pool exists for.
///
/// Built explicitly rather than using `ureq::get`, which picks up whatever the
/// library's defaults happen to be at the version we are pinned to.
pub fn agent() -> &'static Agent {
    static AGENT: OnceLock<Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        Agent::config_builder()
            .tls_config(tls_config())
            // Redirects are followed, but a redirect chain is also a way to
            // make a browser walk somewhere it was never pointed at. A handful
            // is every legitimate use.
            .max_redirects(8)
            .build()
            .into()
    })
}

/// An agent that will also accept this computer's own trust store.
///
/// Never used first. A request goes out against Mozilla's roots, and only a
/// refusal of the specific shape "nothing I trust signed this" is retried here
/// — see [`crate::FetchError::LocalRoot`] and ADR-0015. Everything else about
/// the posture is identical, including that certificates are still *verified*:
/// this widens who may sign, not whether anyone need bother.
///
/// Kept for the process like [`agent`], and separate from it for the same
/// reason it always was: these are two different answers to "whose signature
/// counts", and a page retried against this one is marked in the chrome for as
/// long as it is up.
pub fn platform_agent() -> &'static Agent {
    static PLATFORM_AGENT: OnceLock<Agent> = OnceLock::new();
    PLATFORM_AGENT.get_or_init(|| {
        Agent::config_builder()
            .tls_config(platform_tls_config())
            .max_redirects(8)
            .build()
            .into()
    })
}

/// The TLS configuration, spelled out.
pub fn tls_config() -> TlsConfig {
    TlsConfig::builder()
        // rustls, never native-tls. This is the choice that actually refuses
        // TLS 1.0 and 1.1: rustls has no code for them, whereas native-tls
        // accepts whatever the platform is configured to accept.
        .provider(TlsProvider::Rustls)
        // Mozilla's roots rather than the platform's. Two reasons, and the
        // second is the one that matters: it keeps behaviour identical across
        // Linux, macOS, and Windows the way ADR-0005 asks of rendering, and it
        // means a corporate MITM proxy installed in the system store cannot
        // silently read this browser's traffic.
        .root_certs(RootCerts::WebPki)
        // Stated so that its being false is a decision rather than a default.
        // This is the switch that turns HTTPS into theatre.
        .disable_verification(false)
        .build()
}

/// The same, checked against this computer's trust store instead.
///
/// The *only* difference is where the roots come from. Verification is still
/// on, legacy versions are still absent, and the provider is still rustls — a
/// connection that gets here is not a connection with the checks turned off,
/// it is one checked against a different set of signers.
pub fn platform_tls_config() -> TlsConfig {
    TlsConfig::builder()
        .provider(TlsProvider::Rustls)
        .root_certs(RootCerts::PlatformVerifier)
        .disable_verification(false)
        .build()
}

/// Why a secure connection could not be established.
///
/// ADR-0013 decided that legacy TLS is refused rather than downgraded to. That
/// decision was invisible in use: a site offering nothing newer than TLS 1.1
/// failed with the same shrug as a site that was simply down, so the reader had
/// no way to tell "this browser refused" from "this server is broken". A
/// refusal nobody can recognise is indistinguishable from a bug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handshake {
    /// The server offered no protocol version this browser accepts.
    LegacyVersion,
    /// Nothing in the trust store signed this chain.
    ///
    /// Kept apart from the rest because it is the one certificate failure that
    /// is *routinely* not the site's fault: it is what a network doing TLS
    /// interception looks like, and what a private certificate authority looks
    /// like. It is also the only one worth retrying against this computer's own
    /// trust store — an expired certificate is expired whoever signed it.
    UntrustedRoot,
    /// The certificate did not check out for some other reason.
    Certificate(String),
}

/// Works out whether a transport failure was the TLS handshake, and which way.
///
/// Reads the `rustls` error out of the `io::Error` rather than matching on the
/// message text: the text is a `Display` impl nobody promised us, and it would
/// change silently. `get_ref` gives back the real error, and it downcasts.
pub fn classify(error: &ureq::Error) -> Option<Handshake> {
    let ureq::Error::Io(io) = error else {
        return None;
    };
    let failure = io.get_ref()?.downcast_ref::<rustls::Error>()?;
    match failure {
        // What a correctly-implemented old server actually sends: it cannot
        // satisfy a ClientHello offering only 1.2 and 1.3, so it says so.
        rustls::Error::AlertReceived(rustls::AlertDescription::ProtocolVersion) => {
            Some(Handshake::LegacyVersion)
        }
        // rustls' own conclusion that there is no common ground — a server that
        // never offers a version it can use, rather than one that objects.
        rustls::Error::PeerIncompatible(_) => Some(Handshake::LegacyVersion),
        rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer) => {
            Some(Handshake::UntrustedRoot)
        }
        rustls::Error::InvalidCertificate(reason) => {
            Some(Handshake::Certificate(format!("{reason:?}")))
        }
        // `AlertReceived(HandshakeFailure)` is deliberately not here. It is what
        // an old server sends *and* what a current one sends when only its
        // cipher suites are too weak, and calling the second one "too old" would
        // be a confident sentence that is sometimes false. Under-claiming beats
        // a wrong explanation.
        _ => None,
    }
}

/// Whether a failed attempt is worth retrying against this computer's roots.
///
/// Exactly one shape of failure, and the narrowness is the point (ADR-0015). An
/// expired certificate is expired whoever signed it; a certificate for the
/// wrong name is for the wrong name. Retrying either against a wider set of
/// signers would be looking for someone willing to say yes, which is not what
/// this is for.
pub fn worth_local_retry(error: &ureq::Error) -> bool {
    matches!(classify(error), Some(Handshake::UntrustedRoot))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wraps a `rustls` error the way `ureq` does, so [`classify`] sees what it
    /// would see in the field.
    fn as_ureq(error: rustls::Error) -> ureq::Error {
        ureq::Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    }

    #[test]
    fn a_server_refusing_our_protocol_versions_is_recognised() {
        // Measured, not guessed: an OpenSSL server started with `-tls1` answers
        // a ClientHello offering only 1.2 and 1.3 with exactly this alert.
        assert_eq!(
            classify(&as_ureq(rustls::Error::AlertReceived(
                rustls::AlertDescription::ProtocolVersion
            ))),
            Some(Handshake::LegacyVersion)
        );
    }

    #[test]
    fn only_an_untrusted_root_is_retried_against_this_computer() {
        // The narrowness ADR-0015 depends on. Widening who may sign is
        // defensible for a chain nobody public vouched for; doing it because a
        // certificate expired would be shopping for a signer willing to agree.
        assert!(worth_local_retry(&as_ureq(
            rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer)
        )));

        for settled in [
            rustls::CertificateError::Expired,
            rustls::CertificateError::NotValidForName,
            rustls::CertificateError::Revoked,
        ] {
            assert!(
                !worth_local_retry(&as_ureq(rustls::Error::InvalidCertificate(settled.clone()))),
                "{settled:?} should be final"
            );
        }
        // And a site that is simply too old is not a trust question at all.
        assert!(!worth_local_retry(&as_ureq(rustls::Error::AlertReceived(
            rustls::AlertDescription::ProtocolVersion
        ))));
        assert!(!worth_local_retry(&ureq::Error::ConnectionFailed));
    }

    #[test]
    fn the_local_root_agent_still_verifies_everything_else() {
        // The second attempt widens *who may sign* and nothing else. If it ever
        // came to mean "and skip the checks", this is the test that says so.
        assert!(!platform_tls_config().disable_verification());
        assert_eq!(platform_tls_config().provider(), TlsProvider::Rustls);
        assert!(matches!(
            platform_tls_config().root_certs(),
            RootCerts::PlatformVerifier
        ));
        assert!(!agent().config().tls_config().disable_verification());
    }

    #[test]
    fn a_bad_certificate_is_not_reported_as_an_old_server() {
        // The two failures mean opposite things to a reader — "this site is too
        // old to talk to" versus "something is wrong with this site's identity"
        // — so conflating them would be worse than saying nothing.
        let classified = classify(&as_ureq(rustls::Error::InvalidCertificate(
            rustls::CertificateError::Expired,
        )));
        assert!(
            matches!(classified, Some(Handshake::Certificate(_))),
            "{classified:?}"
        );
    }

    #[test]
    fn an_ordinary_connection_failure_is_not_a_handshake_failure() {
        // A server that is down must not be reported as a server that is old.
        let refused = ureq::Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "connection refused",
        ));
        assert_eq!(classify(&refused), None);
        assert_eq!(classify(&ureq::Error::ConnectionFailed), None);
    }

    #[test]
    fn a_handshake_failure_alert_is_left_unexplained() {
        // Ambiguous between an old server and a current one with weak ciphers.
        // Asserted so that adding it later is a deliberate change rather than a
        // drive-by.
        assert_eq!(
            classify(&as_ureq(rustls::Error::AlertReceived(
                rustls::AlertDescription::HandshakeFailure
            ))),
            None
        );
    }

    #[test]
    fn certificate_verification_is_on() {
        // The one switch that would turn every padlock into a lie. Asserted
        // rather than assumed, because it is one line away.
        assert!(!tls_config().disable_verification());
    }

    #[test]
    fn the_provider_is_rustls_which_is_what_refuses_legacy_tls() {
        // This is the assertion that actually enforces "no TLS 1.0 or 1.1".
        // rustls has no code for those versions; native-tls would take whatever
        // the platform allows, which on an older machine includes them.
        assert_eq!(tls_config().provider(), TlsProvider::Rustls);
    }

    #[test]
    fn the_roots_tried_first_are_mozillas_rather_than_the_platforms() {
        // Still first, and still the default. ADR-0015 added a second attempt
        // against this computer's own roots; it did not make them the starting
        // point. A site that verifies publicly must never take the other path,
        // or the marking would appear on pages nothing is intercepting.
        assert!(matches!(tls_config().root_certs(), RootCerts::WebPki));
    }

    #[test]
    fn the_agent_carries_that_configuration() {
        // The config is only worth asserting if the agent actually uses it.
        let agent = agent();
        let config = agent.config();
        assert!(!config.tls_config().disable_verification());
        assert_eq!(config.tls_config().provider(), TlsProvider::Rustls);
    }

    #[test]
    fn redirects_are_bounded() {
        assert!(agent().config().max_redirects() <= 8);
    }

    #[test]
    fn every_request_goes_through_the_same_agent() {
        // The agent owns the connection pool, so a fresh one per request is a
        // pool that is never used twice — a TCP connection and a TLS handshake
        // per subresource, to a host the last one had just been talking to.
        // Identity is the checkable half of that: whether the pool then gets
        // reused is `ureq`'s business, but throwing it away was ours.
        assert!(std::ptr::eq(agent(), agent()));
        assert!(std::ptr::eq(platform_agent(), platform_agent()));
        // And the two are still distinct, because they answer different
        // questions about whose signature counts (ADR-0015).
        assert!(!std::ptr::eq(agent(), platform_agent()));
    }

    #[test]
    fn rustls_offers_no_protocol_version_older_than_tls_1_2() {
        // The structural fact this all rests on, checked against the library
        // rather than remembered. If a future rustls reintroduced a legacy
        // version — it will not, but this is the assertion that would notice —
        // the count would change and this would fail.
        let versions: Vec<_> = rustls::ALL_VERSIONS
            .iter()
            .map(|version| version.version)
            .collect();
        assert!(
            versions.contains(&rustls::ProtocolVersion::TLSv1_3),
            "{versions:?}"
        );
        for version in &versions {
            assert!(
                matches!(
                    version,
                    rustls::ProtocolVersion::TLSv1_2 | rustls::ProtocolVersion::TLSv1_3
                ),
                "a protocol version older than TLS 1.2 is on offer: {version:?}"
            );
        }
    }
}
