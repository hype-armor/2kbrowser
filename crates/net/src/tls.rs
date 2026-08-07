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
//! Nothing in this file relaxes anything. If a future change wants to, it has
//! to edit these tests, which is a reviewable diff rather than a quiet default.

use ureq::Agent;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

/// The agent every request goes through.
///
/// Built explicitly rather than using `ureq::get`, which picks up whatever the
/// library's defaults happen to be at the version we are pinned to.
pub fn agent() -> Agent {
    Agent::config_builder()
        .tls_config(tls_config())
        // Redirects are followed, but a redirect chain is also a way to make a
        // browser walk somewhere it was never pointed at. A handful is every
        // legitimate use.
        .max_redirects(8)
        .build()
        .into()
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_roots_are_mozillas_rather_than_the_platforms() {
        // Same reasoning as ADR-0005 applies to rendering: the same input
        // should behave the same on all three platforms. It also means a
        // corporate root installed in the system store cannot quietly
        // intercept this browser.
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
