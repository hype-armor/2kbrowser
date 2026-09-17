//! What a redirect means to the policy, and to the page that comes back.
//!
//! Every question here needs a real `Location` header off a real socket. A
//! redirect is the one thing about a fetch that cannot be posed to a file on
//! disk, and it is where this browser was wrong in a way nothing on disk could
//! have caught: it resolved a page's links against the URL that was *asked
//! for*. `hackernews.com` redirects to `news.ycombinator.com`, so every link on
//! the front page was resolved against a host that only knows how to redirect
//! again — and that host's redirect drops the query string, so `item?id=…`
//! arrived as `/item` and Hacker News answered "No such item."

use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use net::{Fetcher, Origin, Refusal, RequestKind};

/// A server that redirects on request, and counts what it was asked for.
///
/// `elsewhere` is pasted into the one route that sends the reader off this
/// server entirely, so a test can point it at a second one and then ask whether
/// that second one was ever touched. "Was the request refused?" and "was the
/// request never made?" are different questions, and only the second one is
/// what ADR-0006 promises.
fn serve(elsewhere: &str) -> (u16, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("binds a port");
    let port = listener.local_addr().expect("has an address").port();
    let asked = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&asked);
    let elsewhere = elsewhere.to_owned();
    // Detached, and it never stops: the test binary exiting is what ends it.
    std::thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            let counter = Arc::clone(&counter);
            let elsewhere = elsewhere.clone();
            std::thread::spawn(move || {
                let mut buffer = [0u8; 2048];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                let wanted = request
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                counter.fetch_add(1, Ordering::SeqCst);

                let redirect = |location: String| {
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: {location}\r\n\
                         Content-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                };
                let page = |body: &str| {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                };

                // The shape Hacker News's front door has: a redirect that keeps
                // the path and throws the query away.
                let response = match wanted.split('?').next().unwrap_or_default() {
                    "/start" => redirect("/deep/landed?id=42".to_owned()),
                    "/deep/landed" => page("<a href=\"item?id=7\">comments</a>"),
                    "/away" => redirect(elsewhere.clone()),
                    "/disk" => redirect("file:///etc/hostname".to_owned()),
                    "/circles" => redirect("/circles".to_owned()),
                    _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\
                          Connection: close\r\n\r\n"
                        .to_owned(),
                };
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            });
        }
    });
    (port, asked)
}

fn origin_of(url: &str) -> Origin {
    net::parse_url(url).expect("parses").0
}

#[test]
fn a_page_belongs_to_where_the_redirect_left_it() {
    // The bug, exactly. The links on the page that comes back have to resolve
    // against where it was served from, not against what was typed — and the
    // query has to survive, because `item?id=7` without the id is a different
    // request that a server is entitled to refuse.
    let (port, _) = serve("");
    let fetched = Fetcher::default()
        .fetch_raw(
            &format!("http://127.0.0.1:{port}/start"),
            None,
            RequestKind::Navigation,
        )
        .expect("fetches");

    assert_eq!(
        fetched.path, "/deep/landed?id=42",
        "kept the asked-for path"
    );
    assert_eq!(
        net::resolve(&fetched.origin, &fetched.path, "item?id=7"),
        format!("http://127.0.0.1:{port}/deep/item?id=7"),
        "a relative link resolved against the wrong page"
    );
}

#[test]
fn a_redirect_off_the_site_is_refused_before_it_is_followed() {
    // ADR-0006's budget says *no third-party request was ever made*. A
    // subresource on the page's own site that answers `302 Location:
    // https://tracker…` is the way that sentence stops being true if the hops
    // are left to the HTTP library: the request gets made and the rule never
    // sees the host it was made to.
    let (tracker_port, tracker_asked) = serve("");
    let (port, _) = serve(&format!("http://127.0.0.1:{tracker_port}/pixel.gif"));

    // `localhost` and `127.0.0.1` are different hosts to the policy and both
    // arrive at this machine, which is what makes a two-server test possible
    // at all without leaving it.
    let document = origin_of(&format!("http://localhost:{port}/"));
    let refusal = Fetcher::default().fetch_raw(
        &format!("http://localhost:{port}/away"),
        Some(&document),
        RequestKind::Subresource,
    );

    assert!(
        matches!(
            refusal,
            Err(net::FetchError::Refused(Refusal::ThirdParty { .. }))
        ),
        "got {:?}",
        refusal.map(|fetched| fetched.path)
    );
    assert_eq!(
        tracker_asked.load(Ordering::SeqCst),
        0,
        "the third party was asked for something, which is the whole thing the rule forbids"
    );
}

#[test]
fn a_redirect_to_the_disk_is_refused_even_on_a_navigation() {
    // A navigation is exempt from the third-party rule, and this is not that
    // rule. There is no origin a local file could be first-party to, and a
    // page that could reach one by bouncing off a server would turn every
    // site on the web into a local file reader.
    let (port, _) = serve("");
    let refusal = Fetcher::default().fetch_raw(
        &format!("http://127.0.0.1:{port}/disk"),
        None,
        RequestKind::Navigation,
    );
    assert!(
        matches!(refusal, Err(net::FetchError::Refused(Refusal::LocalFile))),
        "got {:?}",
        refusal.map(|fetched| fetched.path)
    );
}

#[test]
fn a_redirect_that_goes_in_circles_stops() {
    let (port, asked) = serve("");
    let refusal = Fetcher::default().fetch_raw(
        &format!("http://127.0.0.1:{port}/circles"),
        None,
        RequestKind::Navigation,
    );
    assert!(
        matches!(refusal, Err(net::FetchError::TooManyRedirects)),
        "got {:?}",
        refusal.map(|fetched| fetched.path)
    );
    assert!(
        asked.load(Ordering::SeqCst) <= net::MAX_HOPS as usize + 1,
        "followed {} hops, past the bound",
        asked.load(Ordering::SeqCst)
    );
}
