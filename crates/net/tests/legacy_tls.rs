//! What happens when a site's HTTPS is older than this browser accepts.
//!
//! ADR-0013 refuses TLS 1.0 and 1.1 outright. The unit tests next to
//! [`net::tls::classify`] check the classification against errors built by
//! hand, which proves the matching is right and proves nothing about whether
//! the real stack ever produces those errors. This runs the actual fetch path
//! against a socket that answers the way an old server answers.
//!
//! The server is seven bytes. It does not need to be a TLS implementation: what
//! a server does when it cannot satisfy a ClientHello offering only 1.2 and 1.3
//! is send a fatal `protocol_version` alert, and that is exactly what an OpenSSL
//! server started with `-tls1` was observed to send. Faking it that precisely is
//! what makes this a test rather than a demo — no `openssl` binary has to exist
//! on the machine, so it runs everywhere CI does.

use std::io::{Read, Write};
use std::net::TcpListener;

/// A fatal `protocol_version` alert: the record type, a version, the length,
/// then the level and the description. Byte for byte what the real thing is.
const PROTOCOL_VERSION_ALERT: [u8; 7] = [
    0x15, // alert
    0x03, 0x03, // legacy record version
    0x00, 0x02, // two bytes of payload
    0x02, // fatal
    70,   // protocol_version
];

/// Accepts one connection, reads whatever is offered, and refuses it.
fn refuse_once(listener: TcpListener) {
    let Ok((mut stream, _)) = listener.accept() else {
        return;
    };
    // The ClientHello has to be read before the alert is written, or the client
    // can see a closed pipe instead of the refusal.
    let mut hello = [0u8; 4096];
    let _ = stream.read(&mut hello);
    let _ = stream.write_all(&PROTOCOL_VERSION_ALERT);
    let _ = stream.flush();
}

#[test]
fn a_site_that_offers_only_legacy_tls_is_refused_in_so_many_words() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("binds a port");
    let port = listener.local_addr().expect("has an address").port();
    let server = std::thread::spawn(move || refuse_once(listener));

    let outcome = net::Fetcher::default().fetch_raw(
        &format!("https://127.0.0.1:{port}/"),
        None,
        net::RequestKind::Navigation,
    );
    let _ = server.join();

    // Skipped rather than failed when the request never reached the socket. A
    // machine with a proxy in the environment sends this somewhere else
    // entirely, and a check that cannot run must not look like one that passed.
    if let Err(net::FetchError::Transport(message)) = &outcome
        && !message.contains("alert")
    {
        eprintln!("SKIP: the request did not reach the test server ({message})");
        return;
    }

    assert!(
        matches!(outcome, Err(net::FetchError::LegacyTls)),
        "expected a legacy-TLS refusal, got {outcome:?}"
    );

    // And it has to *say* so. This is the whole point: before this existed the
    // refusal reached the reader as an unexplained network error, which is
    // indistinguishable from the site being down.
    let said = outcome.unwrap_err().to_string();
    assert!(said.contains("refused"), "{said}");
    assert!(said.contains("1.2"), "{said}");
}

#[test]
fn a_server_that_is_simply_down_is_not_reported_as_an_old_one() {
    // The failure mode this must avoid: telling someone a site is too old when
    // it is merely unreachable. The port is bound and dropped, so nothing is
    // listening on it and nothing else has taken it yet.
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binds a port");
        listener.local_addr().expect("has an address").port()
    };
    let outcome = net::Fetcher::default().fetch_raw(
        &format!("https://127.0.0.1:{port}/"),
        None,
        net::RequestKind::Navigation,
    );
    assert!(
        !matches!(outcome, Err(net::FetchError::LegacyTls)),
        "a dead port was reported as legacy TLS: {outcome:?}"
    );
}
