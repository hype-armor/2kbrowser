//! What to show when a server answers with a status and nothing else (#203).
//!
//! A browser used to have two answers here and only one of them was any good.
//! The good one is the server's: a site's own "not found", a proxy's block
//! notice, the sentence somebody wrote to explain a 503. That body now reaches
//! the reader (see `net`, where a status is a page for a navigation and a
//! failure for a subresource), and nothing in this file touches it.
//!
//! This is the other case. A great many servers answer a 401 or a 403 with a
//! status line, a couple of headers and no body at all, and the browser was
//! putting `server returned 401` in the chrome and leaving the window blank.
//! That is the code, unexplained, in the one place a reader looks for the page
//! — and it says nothing about what to do next, which for a 401 in *this*
//! browser is genuinely worth saying: there is no HTTP authentication here, so
//! that page cannot be reached at all.
//!
//! The wording rule is the one the rest of the chrome follows. Say what
//! happened, say whose end it happened at, and do not pretend a refusal is a
//! failure. A 403 is the server working correctly and declining; a 502 is the
//! server admitting it could not reach something else. Those are different
//! sentences and a reader can act on the difference.

/// Whether a status needs a page of its own.
///
/// Only when there is nothing else to show. A server that sent a body wrote
/// something, and whatever it wrote is closer to the truth than anything this
/// file could invent — even when it is unhelpful, because "the page you are
/// looking at is what the server sent" is a thing a reader can reason about.
pub fn needs_a_page(status: u16, body: &[u8]) -> bool {
    status >= 400 && body.iter().all(u8::is_ascii_whitespace)
}

/// The body to show for a response, and the type to read it as.
///
/// Whatever the server sent, unless it sent a status with nothing behind it —
/// then a page of the browser's own. One function rather than the same
/// condition written at each call site, because there are two of them: the
/// window, and the command line, which renders without one.
pub fn substitute(
    status: u16,
    body: Vec<u8>,
    content_type: Option<String>,
    url: &str,
) -> (Vec<u8>, Option<String>) {
    if needs_a_page(status, &body) {
        return (
            page(status, url).into_bytes(),
            Some("text/html; charset=utf-8".to_owned()),
        );
    }
    (body, content_type)
}

/// The page for a status.
pub fn page(status: u16, url: &str) -> String {
    let (name, explanation) = words(status);
    format!(
        "<!doctype html>\n<meta charset=\"utf-8\">\n\
         <title>{status} {name}</title>\n\
         <body style=\"font-family: sans-serif; margin: 3em auto; max-width: 34em\">\n\
         <h1 style=\"font-size: 1.4em\">{status} — {name}</h1>\n\
         <p>{explanation}</p>\n\
         <p><small>{url}</small></p>\n\
         <p><small>The server sent this status and no page to go with it, so \
         this one is the browser's.</small></p>\n",
        status = status,
        name = escape(name),
        explanation = escape(explanation),
        url = escape(url),
    )
}

/// A status in words: its name, and what it means for the reader.
///
/// The named ones are the ones somebody actually meets. Everything else falls
/// back to its class, which is still worth saying — 4xx and 5xx put the problem
/// at opposite ends of the wire, and that is the first thing a reader wants to
/// know because it decides whether trying again could possibly help.
fn words(status: u16) -> (&'static str, &'static str) {
    match status {
        400 => (
            "Bad request",
            "The server could not make sense of the request. That is usually a \
             fault in the address rather than in the page it points at.",
        ),
        401 => (
            "Not authorised",
            "The server will not serve this page without a sign-in. This browser \
             does not implement HTTP authentication, so there is no way to \
             offer one — the page cannot be reached from here.",
        ),
        403 => (
            "Forbidden",
            "The server understood the request and refused it. Nothing is \
             broken: this is the server declining, and no amount of reloading \
             will change its mind. A proxy or a filter between here and the \
             site can also answer this way.",
        ),
        404 => (
            "Not found",
            "The server has nothing at this address. Either it never did, or \
             whatever was here has moved and nothing was left to say where.",
        ),
        405 => (
            "Method not allowed",
            "The server does not accept requests of this kind at this address — \
             usually a form sent with `post` to somewhere that only answers a \
             `get`.",
        ),
        408 => (
            "Request timed out",
            "The server gave up waiting for the request to arrive. Trying again \
             is reasonable.",
        ),
        410 => (
            "Gone",
            "The server says this page used to exist and has been removed on \
             purpose. Unlike a 404, that is a deliberate answer, so it is not \
             worth looking for it again later.",
        ),
        429 => (
            "Too many requests",
            "The server is asking for less traffic — from this machine, or from \
             everyone. Waiting is the only thing that helps.",
        ),
        500 => (
            "Server error",
            "Something went wrong at the server's end while it was making this \
             page. Nothing here caused it and nothing here can fix it.",
        ),
        501 => (
            "Not implemented",
            "The server does not support what was asked of it.",
        ),
        502 => (
            "Bad gateway",
            "The server you reached could not get an answer from another one \
             behind it. The problem is between two machines that are not this \
             one.",
        ),
        503 => (
            "Service unavailable",
            "The server is up but not serving — too busy, or deliberately down \
             for a while. It is the sort of thing that fixes itself.",
        ),
        504 => (
            "Gateway timed out",
            "The server you reached waited for another one behind it and gave \
             up. Like a 502, the trouble is further in.",
        ),
        // The classes, for everything unnamed. Which end the problem is at is
        // the first thing worth knowing, because it decides whether trying
        // again could possibly help.
        400..=499 => (
            "Request refused",
            "The server refused the request. A status in this range puts the \
             problem at this end — the address, or what was asked for — rather \
             than at the server's.",
        ),
        500..=599 => (
            "Server error",
            "The server failed while answering. A status in this range is the \
             server's own end, so trying again later is the only thing that \
             might work.",
        ),
        _ => (
            "Unexpected answer",
            "The server answered with a status this browser was not expecting \
             for a page.",
        ),
    }
}

/// Escapes text for the page above.
///
/// The URL is the part that matters: it came from somewhere, it can contain an
/// ampersand or a `<`, and a page the browser writes about an address must not
/// be rewritten by the address.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_that_sent_a_page_keeps_it() {
        // The whole first half of #203: the body is the site's own answer, and
        // it says more than a status line ever will. This is also #208 — a
        // proxy's block notice is a 403 with a body, and it was being thrown
        // away in favour of `server returned 403`.
        assert!(!needs_a_page(404, b"<html>Sorry, no such page</html>"));
        assert!(!needs_a_page(403, b"Blocked by your network administrator"));
    }

    #[test]
    fn a_status_with_no_body_gets_one() {
        assert!(needs_a_page(401, b""));
        assert!(needs_a_page(500, b"   \r\n  "), "whitespace is not a page");
    }

    #[test]
    fn a_page_that_worked_is_never_replaced() {
        // Including an empty one: a 200 with no body is a blank page, which is
        // what the server meant to send.
        assert!(!needs_a_page(200, b""));
        assert!(!needs_a_page(204, b""));
        assert!(!needs_a_page(304, b""));
    }

    #[test]
    fn the_named_statuses_say_something_specific() {
        for (status, expected) in [
            (401, "Not authorised"),
            (403, "Forbidden"),
            (404, "Not found"),
            (500, "Server error"),
            (503, "Service unavailable"),
        ] {
            let html = page(status, "https://example.com/");
            assert!(html.contains(expected), "{status}: {html}");
            assert!(html.contains(&status.to_string()), "{status}: {html}");
        }
    }

    #[test]
    fn a_401_says_this_browser_cannot_sign_in() {
        // The one status where the honest answer is specific to this browser:
        // there is no HTTP authentication here, so "try signing in" would be
        // advice nobody can take.
        let html = page(401, "https://example.com/");
        assert!(
            html.contains("does not implement HTTP authentication"),
            "{html}"
        );
    }

    #[test]
    fn an_unnamed_status_still_says_which_end_it_came_from() {
        // Which end is the first thing worth knowing: it decides whether
        // trying again could possibly help.
        assert!(page(418, "https://example.com/").contains("this end"));
        assert!(page(507, "https://example.com/").contains("server's own end"));
    }

    #[test]
    fn the_address_cannot_rewrite_the_page_about_it() {
        let html = page(404, "https://example.com/?a=1&b=2<script>");
        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&amp;b=2"), "{html}");
    }
}
