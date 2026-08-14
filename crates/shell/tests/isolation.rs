//! The process boundary, exercised for real.
//!
//! The unit tests either side of it use in-memory pipes and a stub renderer,
//! which proves the protocol and proves the rendering, but never proves that
//! the binary can talk to a copy of itself. This spawns the actual executable
//! and reads the actual pixels back.
//!
//! Two things are worth demanding of a boundary like this, and they are both
//! here: the page must look exactly the same as it did without one, and a child
//! that dies must not take the parent with it.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

use sandbox::{ToChild, ToParent, read_frame, write_frame};

/// The binary under test, as a renderer child.
fn child() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_2kbrowser"));
    command
        .arg(sandbox::CHILD_ARGUMENT)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

fn request(html: &str, width: u32) -> ToChild {
    ToChild::Render {
        body: html.as_bytes().to_vec(),
        content_type: None,
        width,
        top: 0,
        height: 2000,
        origin: None,
        path: String::new(),
        force_authored: false,
        force_document: false,
    }
}

/// Sends one render request to a freshly spawned child and reads the reply.
fn render_in_child(html: &str, width: u32) -> ToParent {
    let mut process = child().spawn().expect("the renderer starts");
    {
        let mut stdin = process.stdin.take().expect("stdin");
        write_frame(&mut stdin, &request(html, width).encode()).expect("writes the request");
    }
    let mut stdout = process.stdout.take().expect("stdout");
    let reply = read_frame(&mut stdout).expect("reads a reply");
    let _ = process.wait();
    ToParent::decode(&reply).expect("the reply decodes")
}

const PAGE: &str = "<title>Across</title><body bgcolor=\"#eef\">\
     <h1>Heading</h1><p>Some <b>bold</b> and some <i>italic</i>.</p>\
     <table border=1><tr><td>a</td><td>b</td></tr></table></body>";

#[test]
fn a_page_rendered_across_a_process_is_byte_identical() {
    // The property the boundary rests on. ADR-0005 makes rendering
    // deterministic, so "identical" is a fair thing to demand rather than
    // "close enough" — and anything less would mean the sandbox changed what
    // the reader sees.
    let mut fonts = text::FontStore::new();
    let direct = shell::render::render(PAGE, 300, 2000, &mut fonts);

    let ToParent::Rendered(crossed) = render_in_child(PAGE, 300) else {
        panic!("the child did not render the page");
    };

    assert_eq!(crossed.width, direct.pixmap.width());
    assert_eq!(crossed.height, direct.pixmap.height());
    assert_eq!(
        crossed.pixels,
        direct.pixmap.data(),
        "the same page rendered differently in a child process"
    );
    assert_eq!(crossed.title.as_deref(), Some("Across"));
}

#[test]
fn the_parent_survives_a_child_that_dies_mid_conversation() {
    // The whole point of the boundary: a renderer that falls over is a failed
    // page, not a failed browser. Killed before it can answer.
    let mut process = child().spawn().expect("the renderer starts");
    let mut stdin = process.stdin.take().expect("stdin");
    // A header promising far more than is sent, so the child blocks waiting
    // for a body that never arrives.
    stdin.write_all(&4096u32.to_le_bytes()).expect("writes");
    stdin.write_all(b"partial").expect("writes");
    stdin.flush().expect("flushes");

    process.kill().expect("kills the child");
    let status = process.wait().expect("reaps the child");
    assert!(!status.success());

    // And the parent is still perfectly able to render.
    assert!(matches!(render_in_child(PAGE, 200), ToParent::Rendered(_)));
}

#[test]
fn a_child_given_nonsense_refuses_it_rather_than_crashing() {
    // A frame that is not a render request. The child must answer and exit
    // cleanly, because the alternative — a child that panics on unexpected
    // input — is a denial of service triggered by a bug in the parent.
    let mut process = child().spawn().expect("the renderer starts");
    {
        let mut stdin = process.stdin.take().expect("stdin");
        write_frame(
            &mut stdin,
            &ToChild::Resources {
                resources: Vec::new(),
            }
            .encode(),
        )
        .expect("writes");
    }
    let mut stdout = process.stdout.take().expect("stdout");
    let frame = read_frame(&mut stdout).expect("reads a reply");
    assert!(matches!(
        ToParent::decode(&frame),
        Ok(ToParent::Failed { .. })
    ));

    let status = process.wait().expect("reaps");
    assert!(status.success(), "a refusal is not a crash");
}

#[test]
fn a_child_sent_a_malformed_frame_does_not_hang_the_parent() {
    // Bytes that decode to nothing. The child should exit rather than block,
    // and its stdout should close so the parent's read returns.
    let mut process = child().spawn().expect("the renderer starts");
    {
        let mut stdin = process.stdin.take().expect("stdin");
        write_frame(&mut stdin, &[0xff, 0xff, 0xff]).expect("writes");
    }
    let mut stdout = process.stdout.take().expect("stdout");
    let mut anything = Vec::new();
    // Returns when the child exits and closes the pipe. If the child hung this
    // would block, and the test would time out rather than pass.
    stdout.read_to_end(&mut anything).expect("reads to the end");
    let _ = process.wait();
}

#[test]
fn the_child_never_writes_anything_but_frames_to_stdout() {
    // stdout is the protocol. A stray `println!` anywhere in the render path
    // would be read as a frame header and corrupt everything after it — a
    // failure that would look like a decode bug rather than like a stray print.
    let ToParent::Rendered(page) = render_in_child(PAGE, 300) else {
        panic!("the child did not render the page");
    };
    // Decoding cleanly with nothing trailing is the proof: `ToParent::decode`
    // refuses trailing bytes, and `read_frame` took exactly one frame.
    assert!(page.width > 0 && page.height > 0);
}

/// A live session against the real binary, with a file so subresources resolve.
fn session(html: &str, width: u32) -> (sandbox::Session, sandbox::Rendered) {
    let dir = std::env::temp_dir().join("2kbrowser-session-tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("page.html");
    std::fs::write(&path, html).expect("write");
    let (origin, at) = net::parse_url(&net::file_url(&path)).expect("parses");

    sandbox::Renderer::with_program(std::path::PathBuf::from(env!("CARGO_BIN_EXE_2kbrowser")))
        .open(
            html.as_bytes().to_vec(),
            None,
            width,
            0,
            2000,
            Some(origin),
            at,
            false,
            false,
        )
        .expect("the renderer opens the page")
}

#[test]
fn a_live_child_answers_find_from_the_page_it_is_holding() {
    // The text and the box tree never cross the boundary, so the only thing
    // that can answer a find query is the process holding them. This is that
    // question being asked and answered across a real pipe.
    let (mut session, _) = session(
        "<body><p>the quick brown fox</p><p>and another fox here</p></body>",
        400,
    );

    let matches = session.find("fox").expect("the child answers");
    assert_eq!(matches.len(), 2, "{matches:?}");
    assert!(matches.iter().all(|rect| rect.width > 0.0));

    // A second query on the same child, which is the point of it staying alive.
    let none = session.find("aardvark").expect("the child answers");
    assert!(none.is_empty());
}

#[test]
fn an_empty_query_matches_nothing_rather_than_everything() {
    let (mut session, _) = session("<body><p>text</p></body>", 300);
    assert!(session.find("").expect("answers").is_empty());
    assert!(session.find("   ").expect("answers").is_empty());
}

#[test]
fn the_same_child_re_renders_at_a_new_width() {
    // A resize is not a fresh page. Re-rendering in the child that already has
    // the document parsed is both cheaper and the reason a resize does not
    // re-fetch every image on the page.
    let html = "<body><p>a paragraph long enough that its height depends on how \
                wide the viewport is, which is the whole point of this test</p></body>";
    let (mut session, narrow) = session(html, 200);

    let dir = std::env::temp_dir().join("2kbrowser-session-tests");
    let (origin, at) = net::parse_url(&net::file_url(&dir.join("page.html"))).expect("parses");
    let wide = session
        .render(
            html.as_bytes().to_vec(),
            None,
            600,
            0,
            2000,
            Some(origin),
            at,
            false,
            false,
        )
        .expect("re-renders");

    assert_eq!(narrow.width, 200);
    assert_eq!(wide.width, 600);
    assert!(
        wide.content_height < narrow.content_height,
        "a wider viewport should need fewer lines: {} vs {}",
        wide.content_height,
        narrow.content_height
    );
}

#[test]
fn dropping_a_session_kills_its_child() {
    // The mechanism that keeps "one page per process" true. Without it a tab
    // that navigated away would leave its renderer running, and a page's
    // leftovers would outlive the page.
    let (first, _) = session("<body><p>x</p></body>", 200);
    drop(first);

    for _ in 0..8 {
        let (live, page) = session("<body><p>x</p></body>", 200);
        assert!(page.width > 0);
        drop(live);
    }
}

#[test]
fn a_dropped_session_leaves_no_process_behind() {
    // The version of the check above that actually looks. The one above proves
    // the path is repeatable, which it said in its own comment, and a leaked
    // renderer per navigation would pass it every time.
    //
    // Worth its own test now because the two platforms kill by completely
    // different means. On Unix the parent calls `kill` and reaps. On Windows it
    // does not: the child is in a job object created with
    // `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, and what kills it is the job handle
    // closing when the session drops. Nothing in the shared code path would
    // notice if that stopped working.
    let (live, _) = session("<body><p>x</p></body>", 200);
    let pid = live.child_id();
    assert!(alive(pid), "the renderer was not running to begin with");
    drop(live);

    // A moment for the kernel to finish with it. Windows tears down a job
    // asynchronously, so an immediate check can still see the process.
    for _ in 0..50 {
        if !alive(pid) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("renderer {pid} was still running a second after its session was dropped");
}

/// Whether a process id belongs to something still running.
///
/// Asked of the operating system rather than of our own bookkeeping, which is
/// the whole point: our bookkeeping is what is on trial.
fn alive(pid: u32) -> bool {
    #[cfg(target_os = "windows")]
    {
        // `tasklist` prints a header and a row when it matches, and a single
        // "INFO: No tasks..." line when it does not.
        let output = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .expect("tasklist runs");
        String::from_utf8_lossy(&output.stdout).contains(&pid.to_string())
    }
    #[cfg(not(target_os = "windows"))]
    {
        // Signal 0 checks for existence without delivering anything. A reaped
        // child is gone; a zombie would still answer, which is exactly the leak
        // worth catching, since the parent is supposed to `wait` as well as
        // `kill`.
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .expect("kill runs")
            .status
            .success()
    }
}

/// A `Viewport` over a real child, with the page written to disk so
/// subresources and links resolve.
fn viewport(html: &str, width: u32) -> shell::viewport::Viewport {
    let dir = std::env::temp_dir().join("2kbrowser-viewport-tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("page.html");
    std::fs::write(&path, html).expect("write");
    let (origin, at) = net::parse_url(&net::file_url(&path)).expect("parses");

    let renderer =
        sandbox::Renderer::with_program(std::path::PathBuf::from(env!("CARGO_BIN_EXE_2kbrowser")));
    shell::viewport::Viewport::open(
        &renderer,
        shell::viewport::Document {
            body: html.as_bytes().to_vec(),
            content_type: None,
            origin,
            path: at,
        },
        width,
        2000,
        false,
        false,
    )
    .expect("the page opens")
}

#[test]
fn a_viewport_answers_everything_the_window_asks_of_a_page() {
    let mut page = viewport(
        "<title>Titled</title><body><p>some text to search</p>\
         <p><a href=\"next.html\">a link</a></p></body>",
        400,
    );

    assert_eq!(page.title(), Some("Titled"));
    assert_eq!(page.width(), 400);
    assert!(page.content_height() > 0.0);
    assert_eq!(
        page.pixels().len(),
        (page.width() * page.height() * 4) as usize
    );
    assert!(matches!(page.mode(), layout::RenderMode::Authored));
    assert!(!page.can_toggle_layout());
    assert!(!page.find("search").is_empty());

    let links = page.links();
    assert_eq!(links.len(), 1, "{links:?}");
    assert!(links[0].url.ends_with("/next.html"), "{}", links[0].url);
}

#[test]
fn a_point_on_a_link_finds_it_and_a_point_beside_it_does_not() {
    // Hit testing is answered from the rectangles the child sent, because the
    // box tree it would otherwise test against is on the far side — and because
    // a round trip per pointer move would be absurd.
    let page = viewport(
        "<body><p><a href=\"there.html\">click me</a></p></body>",
        400,
    );
    let links = page.links();
    let rect = links[0].rects[0];

    let hit = page.link_at(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    assert!(
        hit.is_some_and(|url| url.ends_with("/there.html")),
        "{hit:?}"
    );

    assert_eq!(page.link_at(rect.x + rect.width + 80.0, rect.y + 2.0), None);
    assert_eq!(page.link_at(rect.x, rect.y + rect.height + 200.0), None);
}

#[test]
fn resizing_re_lays_out_without_a_new_page() {
    let mut page = viewport(
        "<body><p>a paragraph long enough that how many lines it needs depends \
         entirely on how wide the viewport happens to be right now</p></body>",
        200,
    );
    let narrow = page.content_height();
    page.resize(700, 2000).expect("re-renders");
    assert_eq!(page.width(), 700);
    assert!(
        page.content_height() < narrow,
        "wider should need fewer lines: {} vs {narrow}",
        page.content_height()
    );
}

#[test]
fn overruling_the_document_fallback_changes_the_mode_it_reports() {
    // ADR-0009's override, across the boundary. The reader must be able to see
    // what the author wrote, and the chrome must be told which it is looking at.
    let mut page = viewport(
        "<body><div style=\"display: flex\"><div style=\"display: grid\">a</div></div></body>",
        300,
    );
    assert!(
        matches!(page.mode(), layout::RenderMode::Document { .. }),
        "expected the fallback, got {:?}",
        page.mode()
    );
    assert!(page.can_toggle_layout());
    assert!(!page.forcing_authored());

    page.set_forcing_authored(true, 300, 2000)
        .expect("re-renders");
    assert!(matches!(page.mode(), layout::RenderMode::Authored));
    assert!(page.forcing_authored());
    assert!(
        page.can_toggle_layout(),
        "once overruling, there has to be a way back"
    );
}

/// An HTTP server answering one path for as long as the test binary runs,
/// counting how many times it was actually asked.
///
/// Written out rather than pulled in. The questions below need a real header
/// off a real socket, and none of them can be asked of anything on disk: a
/// `file:` URL carries no `Content-Type`, and reading a file again is not the
/// cost that fetching one again is.
fn serve(
    path: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
) -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("binds a port");
    let port = listener.local_addr().expect("has an address").port();
    let served = Arc::new(AtomicUsize::new(0));
    let counter = served.clone();
    // Detached, and it never stops: the test binary exiting is what ends it.
    // Joining would mean deciding in advance how many requests to expect, which
    // is the thing being measured.
    std::thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            let mut buffer = [0u8; 2048];
            let read = stream.read(&mut buffer).unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
            let wanted = request
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_owned();
            let response = if wanted == path {
                counter.fetch_add(1, Ordering::SeqCst);
                let mut out = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .into_bytes();
                out.extend_from_slice(&body);
                out
            } else {
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
            };
            let _ = stream.write_all(&response);
            let _ = stream.flush();
        }
    });
    (port, served)
}

/// Opens a page served from `port` in a real child, at 200px.
fn over_http(port: u16, html: &str) -> shell::viewport::Viewport {
    let (origin, at) = net::parse_url(&format!("http://127.0.0.1:{port}/p.html")).expect("parses");
    let renderer =
        sandbox::Renderer::with_program(std::path::PathBuf::from(env!("CARGO_BIN_EXE_2kbrowser")));
    shell::viewport::Viewport::open(
        &renderer,
        shell::viewport::Document {
            body: html.as_bytes().to_vec(),
            content_type: Some("text/html; charset=utf-8".to_owned()),
            origin,
            path: at,
        },
        200,
        200,
        false,
        false,
    )
    .expect("the page opens")
}

/// How much green some pixels hold, which is how these tests tell a stylesheet
/// arrived and was applied rather than merely asked for.
fn green_in(pixels: &[u8]) -> usize {
    pixels
        .chunks_exact(4)
        .filter(|pixel| pixel[0] < 80 && pixel[1] > 150 && pixel[2] < 80)
        .count()
}

fn green_pixels(page: &shell::viewport::Viewport) -> usize {
    green_in(page.pixels())
}

#[test]
fn a_subresource_remembered_for_one_page_is_not_served_to_another() {
    // The cache sits behind the policy, never in front of it. ADR-0006's rule
    // is about who is asking as much as about what for, and a live child
    // outlives the document it was started for — `Session::render` can be
    // handed a new one. A cache consulted before the check would let a page ask
    // once from somewhere it was allowed and be answered for ever after, from
    // anywhere, which is the whole third-party rule undone by an optimisation.
    let (port, served) = serve("/s.css", "text/css", GREEN_SHEET.to_vec());
    let html = format!(
        "<html><head><link rel=stylesheet href=\"http://127.0.0.1:{port}/s.css\">\
         </head><body><p>green</p></body></html>"
    );
    let renderer =
        sandbox::Renderer::with_program(std::path::PathBuf::from(env!("CARGO_BIN_EXE_2kbrowser")));

    // First: the document and the sheet share a host, so it is first-party.
    let (origin, at) = net::parse_url(&format!("http://127.0.0.1:{port}/p.html")).expect("parses");
    let (mut session, first) = renderer
        .open(
            html.as_bytes().to_vec(),
            Some("text/html".to_owned()),
            200,
            0,
            200,
            Some(origin),
            at,
            false,
            false,
        )
        .expect("the renderer opens the page");

    if served.load(std::sync::atomic::Ordering::SeqCst) == 0 {
        eprintln!("SKIP: the stylesheet request never reached the test server");
        return;
    }
    assert!(
        green_in(&first.pixels) > 0,
        "a first-party sheet should have applied"
    );

    // Then the same live child renders a document from a different host.
    // `localhost` and `127.0.0.1` are different hosts by `is_same_site`, so the
    // very same stylesheet URL is now third-party and has to be refused.
    let (other, other_at) =
        net::parse_url(&format!("http://localhost:{port}/p.html")).expect("parses");
    let second = session
        .render(
            html.as_bytes().to_vec(),
            Some("text/html".to_owned()),
            200,
            0,
            200,
            Some(other),
            other_at,
            false,
            false,
        )
        .expect("renders");

    assert_eq!(
        green_in(&second.pixels),
        0,
        "a sheet remembered for one document was handed to another that may not have it"
    );
}

/// The stylesheet these tests serve, and the colour they look for.
const GREEN_SHEET: &[u8] = b"p { background-color: #00ff00 }";

/// A server that answers any path slowly, one connection per thread, recording
/// how many requests were in flight at once.
///
/// The delay is the instrument. Whether fetches overlap cannot be seen from
/// their results — the page looks the same either way — so it has to be seen
/// from the server's side, by asking how many were being served at the same
/// moment.
fn serve_slowly(
    body: Vec<u8>,
    delay: std::time::Duration,
) -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("binds a port");
    let port = listener.local_addr().expect("has an address").port();
    let live = Arc::new(AtomicUsize::new(0));
    let most = Arc::new(AtomicUsize::new(0));
    let (live_here, most_here) = (live.clone(), most.clone());
    std::thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            let (body, live, most) = (body.clone(), live_here.clone(), most_here.clone());
            std::thread::spawn(move || {
                let mut buffer = [0u8; 2048];
                let _ = stream.read(&mut buffer);
                let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(delay);
                live.fetch_sub(1, Ordering::SeqCst);
                let mut out = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .into_bytes();
                out.extend_from_slice(&body);
                let _ = stream.write_all(&out);
                let _ = stream.flush();
            });
        }
    });
    (port, most)
}

/// A solid PNG of one colour, as bytes.
fn solid_png(width: u32, height: u32, rgb: (u8, u8, u8)) -> Vec<u8> {
    let mut pixmap = paint::Pixmap::new(width, height).expect("a pixmap");
    pixmap.fill(paint::RasterColor::from_rgba8(rgb.0, rgb.1, rgb.2, 0xff));
    let file = std::env::temp_dir().join(format!("2kbrowser-solid-{width}x{height}-{rgb:?}.png"));
    pixmap.save_png(&file).expect("writes a png");
    std::fs::read(&file).expect("reads it back")
}

/// A server that answers each path with its own body.
fn serve_each(
    routes: Vec<(&'static str, Vec<u8>)>,
) -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("binds a port");
    let port = listener.local_addr().expect("has an address").port();
    let served = Arc::new(AtomicUsize::new(0));
    let counter = served.clone();
    std::thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            let (routes, counter) = (routes.clone(), counter.clone());
            std::thread::spawn(move || {
                let mut buffer = [0u8; 2048];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                let wanted = request
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                let response = match routes.iter().find(|(path, _)| *path == wanted) {
                    Some((_, body)) => {
                        counter.fetch_add(1, Ordering::SeqCst);
                        let mut out = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .into_bytes();
                        out.extend_from_slice(body);
                        out
                    }
                    None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
                };
                let _ = stream.write_all(&response);
                let _ = stream.flush();
            });
        }
    });
    (port, served)
}

#[test]
fn one_image_used_all_over_a_page_is_fetched_once() {
    // The era's markup leans on a single spacer or bullet repeated everywhere,
    // and now that images are asked for as a batch, the collapsing has to
    // happen inside one — the cache is only written once the batch is done, so
    // duplicates within it would all miss and all go to the network together.
    let (port, served) = serve_each(vec![("/dot.png", solid_png(8, 8, (0, 0xff, 0)))]);
    let images: String = std::iter::repeat_n("<img src=\"/dot.png\">", 8).collect();
    let page = over_http(port, &format!("<html><body>{images}</body></html>"));

    let count = served.load(std::sync::atomic::Ordering::SeqCst);
    if count == 0 {
        eprintln!("SKIP: the image requests never reached the test server");
        return;
    }
    assert_eq!(
        count, 1,
        "one image, used eight times, fetched {count} times"
    );
    assert_eq!(
        page.images_loaded(),
        8,
        "every use of it should still have got the image"
    );
}

#[test]
fn a_page_asking_for_more_resources_than_the_ceiling_is_refused() {
    // The ceiling exists because the conversation is driven by the child: a
    // compromised renderer that kept asking would have the parent fetching for
    // ever, which is a denial of service against whatever it is pointed at.
    //
    // Batching is exactly how that bound could have been lost. Counting one per
    // message rather than one per URL would let a page ask for any number of
    // things in a single breath, so this asks for more than the ceiling in as
    // few messages as possible.
    let dir = std::env::temp_dir().join("2kbrowser-ceiling");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let (origin, at) = net::parse_url(&net::file_url(&dir.join("page.html"))).expect("parses");
    let images: String = (0..sandbox::MAX_RESOURCES + 1)
        .map(|n| format!("<img src=\"missing{n}.png\">"))
        .collect();
    let html = format!("<html><body>{images}</body></html>");

    let renderer =
        sandbox::Renderer::with_program(std::path::PathBuf::from(env!("CARGO_BIN_EXE_2kbrowser")));
    let outcome = shell::viewport::Viewport::open(
        &renderer,
        shell::viewport::Document {
            body: html.as_bytes().to_vec(),
            content_type: None,
            origin,
            path: at,
        },
        200,
        200,
        false,
        false,
    );

    match outcome {
        Err(error) => assert!(
            error.to_string().contains("more than"),
            "refused for the wrong reason: {error}"
        ),
        Ok(_) => panic!("a page asked for more than the ceiling and was allowed to"),
    }
}

#[test]
fn a_batch_of_images_lands_on_the_elements_that_asked_for_them() {
    // Nothing in a batch carries an id: answers are matched to requests by
    // position, all the way from the parent's fetch through the pipe to the
    // element the image belongs to. Concurrency is exactly what makes that
    // fragile — the fetches finish in whatever order the network decides — so
    // this asks whether the *page* came out right rather than whether the
    // requests did.
    //
    // Red above green in the markup, so red must be above green on the canvas.
    // Swap any two answers anywhere along that path and this inverts.
    let (port, served) = serve_each(vec![
        ("/red.png", solid_png(40, 40, (0xff, 0, 0))),
        ("/green.png", solid_png(40, 40, (0, 0xff, 0))),
    ]);
    let page = over_http(
        port,
        "<html><body><p><img src=\"/red.png\"></p><p><img src=\"/green.png\"></p></body></html>",
    );

    if served.load(std::sync::atomic::Ordering::SeqCst) == 0 {
        eprintln!("SKIP: the image requests never reached the test server");
        return;
    }
    assert_eq!(page.images_loaded(), 2, "both images should have arrived");

    // The topmost row holding each colour. Both must be found, and red's must
    // come first.
    let row_of = |wanted: (u8, u8)| -> Option<u32> {
        let width = page.width() as usize;
        page.pixels()
            .chunks_exact(4)
            .enumerate()
            .find(|(_, pixel)| {
                pixel[0] as i32 - i32::from(wanted.0) == 0 && pixel[1] == wanted.1 && pixel[2] == 0
            })
            .map(|(at, _)| (at / width) as u32)
    };
    let red = row_of((0xff, 0)).expect("the red image was never painted");
    let green = row_of((0, 0xff)).expect("the green image was never painted");
    assert!(
        red < green,
        "green was painted above red, so the answers landed on the wrong elements \
         (red at row {red}, green at row {green})"
    );
}

#[test]
fn a_pages_images_are_fetched_at_the_same_time_rather_than_one_after_another() {
    // The reason the protocol asks for several at once. Every subresource used
    // to be its own request/response over the pipe, so a page waited for the
    // sum of their latencies instead of the longest — twenty images meant
    // twenty round trips one after another.
    //
    // Overlap is invisible in the result, so it is measured where it happens:
    // the server counts how many requests it was serving at the same moment.
    let tile = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/ref/fixtures/assets/tile.png"),
    )
    .expect("the reference fixture tile");
    let (port, most_at_once) = serve_slowly(tile, std::time::Duration::from_millis(300));

    // Distinct URLs, because identical ones are answered once by the cache and
    // would prove nothing about concurrency.
    let images: String = (0..6)
        .map(|n| format!("<img src=\"/tile{n}.png\">"))
        .collect();
    let page = over_http(port, &format!("<html><body>{images}</body></html>"));

    let most = most_at_once.load(std::sync::atomic::Ordering::SeqCst);
    if most == 0 {
        eprintln!("SKIP: the image requests never reached the test server");
        return;
    }
    assert!(
        most > 1,
        "every image was fetched on its own, one after another: {most} at once"
    );
    assert_eq!(
        page.images_loaded(),
        6,
        "the images did not all arrive and decode"
    );
}

#[test]
fn re_rendering_a_page_does_not_fetch_its_subresources_again() {
    // What a resize costs. Laying a page out again re-runs the whole pipeline
    // in the child, subresource requests included, and the parent had no cache
    // of any kind — so every stylesheet and every image went back to the
    // network on every re-render, at whatever rate the window was resizing.
    let (port, served) = serve("/s.css", "text/css", GREEN_SHEET.to_vec());
    let mut page = over_http(
        port,
        "<html><head><link rel=stylesheet href=\"s.css\"></head>\
         <body><p>green</p></body></html>",
    );

    // Skipped rather than failed when the request never reached the server: a
    // machine with a proxy in its environment sends it somewhere else entirely,
    // and a check that could not run must not look like one that passed.
    let after_first = served.load(std::sync::atomic::Ordering::SeqCst);
    if after_first == 0 {
        eprintln!("SKIP: the stylesheet request never reached the test server");
        return;
    }
    assert_eq!(after_first, 1, "the first render fetches it once");

    for width in [220, 240, 260] {
        page.resize(width, 200).expect("re-renders");
    }
    assert_eq!(
        served.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a re-render went back to the network for a stylesheet it already had"
    );
    assert!(
        green_pixels(&page) > 0,
        "the remembered stylesheet stopped reaching the paint"
    );
}

#[test]
fn the_same_subresource_twice_on_one_page_is_fetched_once() {
    // The half that helps a page's *first* load rather than its second. The
    // era's markup leans on one spacer image used all over a layout, and every
    // use of it was its own trip to the network.
    let (port, served) = serve("/s.css", "text/css", GREEN_SHEET.to_vec());
    let page = over_http(
        port,
        "<html><head>\
         <link rel=stylesheet href=\"s.css\"><link rel=stylesheet href=\"s.css\">\
         <link rel=stylesheet href=\"s.css\"><link rel=stylesheet href=\"s.css\">\
         </head><body><p>green</p></body></html>",
    );

    let count = served.load(std::sync::atomic::Ordering::SeqCst);
    if count == 0 {
        eprintln!("SKIP: the stylesheet request never reached the test server");
        return;
    }
    assert_eq!(count, 1, "one sheet, asked for four times, fetched {count}");
    assert!(green_pixels(&page) > 0, "the sheet did not reach the paint");
}

#[test]
fn a_subresource_keeps_the_charset_its_own_header_declared() {
    // The sibling of `a_charset_that_only_the_header_knows_still_reaches_the_
    // renderer`, which asks this of the *document*. That path kept its header
    // and this one did not, which is exactly the sort of inconsistency nobody
    // would think to look for — the same bug, one boundary further in.
    //
    // The parent used to build every answer with `content_type: None`, throwing
    // away what the transport said — while `ToChild::Resource` documented the
    // field as existing precisely so "a stylesheet's character set can come
    // from the header". A sheet declared only in its header decoded as
    // something else, silently, which on the old web is most of them.
    //
    // The selector is the instrument. `.café` is written in UTF-8 and the
    // header says so; read as windows-1252 — the fallback when nothing says
    // otherwise — those two bytes are `Ã©` and the rule matches nothing at all.
    let css = ".caf\u{e9} { background-color: #00ff00 }"
        .as_bytes()
        .to_vec();
    let (port, served) = serve("/s.css", "text/css; charset=utf-8", css);

    let html = "<html><head><link rel=stylesheet href=\"s.css\"></head>\
                <body><p class=\"caf\u{e9}\">green if the header arrived</p></body></html>";
    let (origin, at) = net::parse_url(&format!("http://127.0.0.1:{port}/p.html")).expect("parses");

    let renderer =
        sandbox::Renderer::with_program(std::path::PathBuf::from(env!("CARGO_BIN_EXE_2kbrowser")));
    let page = shell::viewport::Viewport::open(
        &renderer,
        shell::viewport::Document {
            body: html.as_bytes().to_vec(),
            content_type: Some("text/html; charset=utf-8".to_owned()),
            origin,
            path: at,
        },
        200,
        200,
        false,
        false,
    )
    .expect("the page opens");

    // Skipped rather than failed when the request never reached the server. A
    // machine with a proxy in the environment sends this somewhere else
    // entirely, and a check that could not run must not look like one that
    // passed — the same rule `legacy_tls.rs` follows for the same reason.
    if served.load(std::sync::atomic::Ordering::SeqCst) == 0 {
        eprintln!("SKIP: the stylesheet request never reached the test server");
        return;
    }

    let green = page
        .pixels()
        .chunks_exact(4)
        .filter(|pixel| pixel[0] < 80 && pixel[1] > 150 && pixel[2] < 80)
        .count();
    assert!(
        green > 0,
        "the sheet was decoded as something other than the charset it was served with"
    );
}

#[test]
fn an_ordinary_page_can_be_asked_for_the_fallback_and_given_it_back() {
    // The other direction of the same override, across the same boundary. Not
    // the absence of the test above: an ordinary page classifies as `Authored`
    // and has no fallback to return to, so wanting one is its own request
    // (ADR-0009).
    let mut page = viewport(
        "<body><h1>Title</h1><p>An ordinary paragraph.</p></body>",
        300,
    );
    assert!(
        matches!(page.mode(), layout::RenderMode::Authored),
        "this fixture is only useful while it needs no fallback: {:?}",
        page.mode()
    );
    assert!(
        !page.can_toggle_layout(),
        "nothing has been decided about this page yet"
    );
    assert!(!page.forcing_document());
    let plain = page.pixels().to_vec();

    page.set_forcing_document(true, 300, 2000)
        .expect("re-renders");
    assert!(
        matches!(page.mode(), layout::RenderMode::Document { .. }),
        "asking for the fallback across the boundary did not produce one: {:?}",
        page.mode()
    );
    assert!(page.forcing_document());
    assert!(
        page.can_toggle_layout(),
        "a reader who asked for this needs the way back"
    );
    assert_ne!(
        plain,
        page.pixels(),
        "the forced fallback rendered identically to the author's layout"
    );

    // And the way back actually goes back, rather than leaving the reader in a
    // rendering they cannot get out of.
    page.set_forcing_document(false, 300, 2000)
        .expect("re-renders");
    assert!(matches!(page.mode(), layout::RenderMode::Authored));
    assert!(!page.can_toggle_layout());
    assert_eq!(plain, page.pixels(), "going back did not go back");
}

#[test]
fn the_renderer_cannot_open_a_socket_or_a_file() {
    // The only honest way to test a sandbox is from inside it: a filter that
    // installs successfully and blocks nothing would pass any test written from
    // outside. So the binary confines and reports from within.
    //
    // Both mechanisms answer here. On Linux the process filters itself; on
    // Windows it builds an AppContainer and runs the probes in a child, because
    // there is no call a process can make to put *itself* in one.
    let output = Command::new(env!("CARGO_BIN_EXE_2kbrowser"))
        .arg(sandbox::confine::SELFTEST_ARGUMENT)
        // `rappct`'s own diagnostic, which prints what it handed `CreateProcessW`
        // — the flags, whether a command line and a working directory were
        // present, and the raw last-error. Asked for here rather than in the CI
        // workflow so that a developer reproducing a Windows failure gets the
        // same detail without having to know the variable exists. It prints
        // only when a launch fails, so it costs nothing the rest of the time.
        .env("RAPPCT_DEBUG_LAUNCH", "1")
        .output()
        .expect("the selftest runs");
    let report = String::from_utf8_lossy(&output.stdout);
    // Everything the selftest said that was not the report. On the failure path
    // this is where the useful part is: `os error 203` on its own is not
    // something anyone can act on, and two pushes were spent guessing at it
    // before this was here to read.
    let aside = String::from_utf8_lossy(&output.stderr);

    // Skipped rather than failed where the sandbox is unavailable — an old
    // kernel, a container that forbids installing a filter. A check that cannot
    // run must not look like a check that passed, so it says so.
    //
    // Except that it did look like one, for as long as the skip was only an
    // `eprintln!`. `cargo test` captures the output of a test that passes, so a
    // skipped run and a verified one both printed `... ok` and nothing else —
    // the third time this project has produced a check that could not fail, and
    // the worst of the three, because it covered the other two.
    //
    // So: a skip is legitimate on a developer's machine and never in CI. CI is
    // where the claim gets made, and a sandbox that could not be installed
    // there is the finding, not a reason to say nothing. The distinction is the
    // `CI` variable every runner sets.
    if report.contains("confinement=Failed") || report.contains("confinement=Unavailable") {
        assert!(
            std::env::var_os("CI").is_none(),
            "no sandbox was installed, and this is CI — where a skip is a \
             failure, because a green run here is what the README's claims \
             rest on:\n{report}\n--- stderr ---\n{aside}"
        );
        eprintln!("SKIP: no sandbox is available here\n{report}\n{aside}");
        return;
    }

    let expected = if cfg!(target_os = "windows") {
        "confinement=AppContainer"
    } else if cfg!(target_os = "macos") {
        "confinement=AppSandbox"
    } else {
        "confinement=Seccomp"
    };
    assert!(report.contains(expected), "{report}");

    let field = |name: &str| {
        report
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .unwrap_or_else(|| panic!("no `{name}` line in:\n{report}"))
            .to_owned()
    };

    // First: were the probes aimed at the things the parent prepared? Both
    // previous versions of this check failed here rather than at the assertions
    // below — a path that did not exist on the far side, and then a temp
    // directory an AppContainer silently redirects. A refusal is only evidence
    // if the thing refused was really there.
    assert_eq!(
        field("port="),
        field("expect-port="),
        "the probe connected somewhere else:\n{report}"
    );
    assert_eq!(
        field("file-path="),
        field("expect-file="),
        "the probe opened something else:\n{report}"
    );

    // Something is listening on that port, so `OPENED` is the one outcome that
    // proves the network was reachable, and nothing else is ambiguous with it.
    // A port with nothing behind it would not do: an AppContainer's network
    // block is enforced by the firewall, which resets rather than failing the
    // call, so blocked and dead both read as `ConnectionRefused`.
    //
    // Worth being precise about what this covers on Windows: loopback and
    // outbound are separate AppContainer rules, and only loopback can be probed
    // without reaching the internet from a test. What rules out outbound is the
    // capability set being empty, which `sandbox::contain` asserts directly.
    let socket = field("socket=");
    assert_ne!(
        socket, "OPENED",
        "the renderer could still reach the network:\n{report}"
    );
    let file = field("file=");
    assert_ne!(
        file, "OPENED",
        "the renderer could still open a file:\n{report}"
    );
    // The direction of the filter, which the two assertions above cannot show.
    // A denylist that names `socket` and `openat` produces exactly the same two
    // refusals as an allowlist naming neither — so the check that ADR-0016
    // actually landed is a call *nobody named*, refused because it was not on
    // the list. Reading the working directory is that call.
    //
    // Linux only. An AppContainer restricts resources rather than filtering
    // calls, so this succeeds inside one and is not evidence of anything there.
    if cfg!(target_os = "linux") {
        assert_ne!(
            field("unnamed-call="),
            "ALLOWED",
            "the filter permits calls it does not name — it is not an allowlist:\n{report}"
        );
    }

    // And it is still able to do its actual job, which a sandbox that broke
    // everything would also satisfy the two assertions above.
    assert!(report.contains("compute=55"), "{report}");
}

#[test]
fn a_renderer_child_renders_a_page_with_subresources_over_the_pipe() {
    // Named for what it checks on *every* platform. It used to be called
    // `a_confined_renderer_...`, which on Windows was a green test asserting
    // confinement that does not exist there — a name that reads as
    // confirmation of something untrue.
    //
    // What it does check everywhere: the era fixture, which has images, a tiled
    // background, and nested tables, rendered by a child that fetches nothing
    // itself, so every one of those arrived over the pipe. On Linux the child
    // is additionally confined, which makes this the check that the syscall
    // filter did not break the thing it protects.
    let path = std::path::Path::new("../../tests/ref/fixtures/era-page.html")
        .canonicalize()
        .expect("the era fixture");
    let bytes = std::fs::read(&path).expect("read");
    let (origin, at) = net::parse_url(&net::file_url(&path)).expect("parses");

    let renderer =
        sandbox::Renderer::with_program(std::path::PathBuf::from(env!("CARGO_BIN_EXE_2kbrowser")));
    let page = shell::viewport::Viewport::open(
        &renderer,
        shell::viewport::Document {
            body: bytes.clone(),
            content_type: None,
            origin: origin.clone(),
            path: at.clone(),
        },
        800,
        4000,
        false,
        false,
    )
    .expect("the confined renderer opens the page");

    // Byte-identical to rendering it here, where nothing is confined and the
    // fetcher is in-process.
    let mut fonts = text::FontStore::new();
    let (html, ..) = net::encoding::decode_document(&bytes, None);
    let direct =
        shell::render::render_with_base(&html, 800, 4000, &mut fonts, Some((&origin, &at)));

    assert_eq!(direct.images_loaded, 3, "the fixture should load 3 images");
    assert_eq!(
        (page.width(), page.height()),
        (direct.pixmap.width(), direct.pixmap.height())
    );
    assert_eq!(
        page.pixels(),
        direct.pixmap.data(),
        "a confined renderer produced different pixels"
    );
}

#[test]
fn a_page_taller_than_its_band_is_still_scrollable_to_the_end() {
    // What banded rendering is for. A page used to stop at the canvas it was
    // given: the rows past it had no pixels, the scroll stopped there, and the
    // bar said the end was not shown. Now the canvas is a *band*, the scroll
    // range is the document, and the rows arrive when they are reached.
    let dir = std::env::temp_dir().join("2kbrowser-truncation-test");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("tall.html");
    let html = format!("<body>{}</body>", "<p>line</p>".repeat(200));
    std::fs::write(&path, &html).expect("write");
    let (origin, at) = net::parse_url(&net::file_url(&path)).expect("parses");

    let renderer =
        sandbox::Renderer::with_program(std::path::PathBuf::from(env!("CARGO_BIN_EXE_2kbrowser")));
    let document = shell::viewport::Document {
        body: html.as_bytes().to_vec(),
        content_type: None,
        origin,
        path: at,
    };

    let mut page =
        shell::viewport::Viewport::open(&renderer, document.clone(), 400, 300, false, false)
            .expect("the page opens");
    assert!(
        page.content_height() > page.height() as f32,
        "the fixture is too short to band: {} content, {} band",
        page.content_height(),
        page.height()
    );
    // The whole document, not the band. This is the assertion that used to say
    // the opposite.
    assert_eq!(page.scrollable_height(), page.content_height());
    assert_eq!(page.band_top(), 0);

    // And the last rows of the document are reachable.
    let last = page.content_height() as u32 - 100;
    page.request_band(last, 300).expect("asks");
    let arrived = loop {
        if page.accept_band() {
            break true;
        }
        if !page.band_outstanding() {
            break false;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert!(arrived, "the band never arrived");
    assert_eq!(page.band_top(), last);

    // Every match is offered, wherever it is. They used to be filtered to the
    // painted canvas because a match below it could not be shown; now it can.
    let matches = page.find("line");
    assert_eq!(matches.len(), 200, "every line should match");
    assert!(
        matches.iter().any(|rect| rect.y > page.height() as f32),
        "matches beyond the band should still be offered"
    );
    assert_eq!(page.links().len(), 0);
}

#[test]
#[cfg(target_os = "linux")]
fn the_render_command_parses_in_a_child_rather_than_in_itself() {
    // `render` and `links` used to parse and lay out in the calling process,
    // which made ADR-0012 a property of the window rather than of the browser.
    // Nothing failed when they did — the pixels are identical either way, which
    // is exactly why this needs checking directly rather than through output.
    //
    // Linux only, because `/proc/<pid>/task/<pid>/children` is an exact answer
    // and `ps` is a guess. Elsewhere this says it did not run rather than
    // passing quietly.
    let out = std::env::temp_dir().join("2kbrowser-cli-isolation.png");
    let mut process = Command::new(env!("CARGO_BIN_EXE_2kbrowser"))
        .args(["render", "../../tests/ref/fixtures/era-page.html"])
        .arg("--out")
        .arg(&out)
        // Wide and tall, so the render takes long enough to be watched. A
        // faster machine makes this shorter; the poll below is fine either way,
        // because it only has to see the child once out of hundreds of looks.
        .args(["--width", "2000", "--height", "8000"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the render command starts");

    let children = format!("/proc/{pid}/task/{pid}/children", pid = process.id());
    let mut saw_a_child = false;
    loop {
        if let Ok(listed) = std::fs::read_to_string(&children)
            && !listed.trim().is_empty()
        {
            saw_a_child = true;
            break;
        }
        match process.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
            Err(_) => break,
        }
    }
    let status = process.wait().expect("reaps");
    assert!(status.success(), "the render itself failed");
    assert!(
        saw_a_child,
        "`2kbrowser render` never spawned a renderer — it parsed the page itself"
    );
}

#[test]
fn renderers_built_at_the_same_time_all_start() {
    // Windows builds the sandbox in the parent: an AppContainer profile in the
    // registry, and a grant on the executable's DACL so the container can read
    // the binary it is meant to run. Doing that from several threads while
    // other threads launch processes from the same executable made
    // `CreateProcessW` fail — two tests out of seventeen, on a run whose code
    // had been green the time before.
    //
    // It was found by luck, so it is worth looking for on purpose: a race that
    // only appears when the schedule interleaves the right two operations is
    // one that comes back and gets dismissed as flakiness.
    //
    // On Linux this costs a few child processes and asserts nothing new, which
    // is the right price for a check that only fails on the platform it is
    // about.
    let threads: Vec<_> = (0..6)
        .map(|index| {
            std::thread::spawn(move || {
                let renderer = sandbox::Renderer::with_program(std::path::PathBuf::from(env!(
                    "CARGO_BIN_EXE_2kbrowser"
                )));
                renderer
                    .render(
                        format!("<body><p>thread {index}</p></body>").into_bytes(),
                        None,
                        200,
                        0,
                        400,
                        None,
                        String::new(),
                        false,
                        false,
                    )
                    .map(|page| page.width)
                    .map_err(|error| format!("thread {index}: {error}"))
            })
        })
        .collect();

    let outcomes: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().expect("the thread does not panic"))
        .collect();
    let failures: Vec<_> = outcomes
        .iter()
        .filter_map(|outcome| outcome.as_ref().err())
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} concurrent renderers failed to start: {failures:?}",
        failures.len(),
        outcomes.len()
    );
}

#[test]
fn a_band_fetched_over_the_pipe_is_the_rows_it_names() {
    // The same property `paint` asserts of `rasterise_band`, demanded of the
    // whole arrangement: parent asks a live child for rows it does not have,
    // and gets exactly the rows it would have got from one enormous canvas.
    //
    // This is what makes banding a fix for long pages rather than a rendering
    // change wearing one as a disguise.
    let html = format!(
        "<body bgcolor=\"#eef\">{}</body>",
        (0..120)
            .map(|n| format!("<p>line {n} with some words on it</p>"))
            .collect::<String>()
    );
    let dir = std::env::temp_dir().join("2kbrowser-band-tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("tall.html");
    std::fs::write(&path, &html).expect("write");
    let (origin, at) = net::parse_url(&net::file_url(&path)).expect("parses");

    let renderer =
        sandbox::Renderer::with_program(std::path::PathBuf::from(env!("CARGO_BIN_EXE_2kbrowser")));
    let (mut session, whole) = renderer
        .open(
            html.as_bytes().to_vec(),
            None,
            300,
            0,
            8000,
            Some(origin),
            at,
            false,
            false,
        )
        .expect("the renderer opens the page");
    assert!(
        whole.height > 600,
        "the fixture is too short to band: {}",
        whole.height
    );
    assert_eq!(whole.top, 0);

    let row = |page: &sandbox::Rendered, n: u32| {
        let stride = (page.width * 4) as usize;
        page.pixels[n as usize * stride..(n as usize + 1) * stride].to_vec()
    };

    for (top, height) in [
        (0u32, 90u32),
        (137, 90),
        (400, 250),
        (whole.height - 30, 90),
    ] {
        let band = session.band(top, height).expect("the child paints a band");
        assert_eq!(band.top, top);
        assert_eq!(band.width, whole.width);
        // Clipped to what the document has below `top`, exactly as a first
        // render is clipped to the content.
        assert_eq!(band.height, height.min(whole.height - top));
        assert_eq!(
            band.content_height, whole.content_height,
            "moving down the page changed how tall it is"
        );
        for n in 0..band.height {
            assert_eq!(
                row(&band, n),
                row(&whole, top + n),
                "band at {top}: row {n} is not document row {}",
                top + n
            );
        }
    }

    // A band is pixels, not a re-render: what the parent knows about the page
    // must survive one. A band that blanked the title or the links would empty
    // the tab strip and every keyboard target on the page.
    let band = session.band(0, 100).expect("paints");
    assert_eq!(band.links.len(), whole.links.len());
    assert_eq!(band.title, whole.title);
    assert_eq!(band.can_toggle_layout, whole.can_toggle_layout);
}

/// A tall page in a live session, for the band tests.
fn tall_session(lines: usize, width: u32) -> (sandbox::Session, sandbox::Rendered) {
    let html = format!(
        "<body bgcolor=\"#eef\">{}</body>",
        (0..lines)
            .map(|n| format!("<p>line {n} with some words on it</p>"))
            .collect::<String>()
    );
    let dir = std::env::temp_dir().join("2kbrowser-band-tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("tall.html");
    std::fs::write(&path, &html).expect("write");
    let (origin, at) = net::parse_url(&net::file_url(&path)).expect("parses");

    sandbox::Renderer::with_program(std::path::PathBuf::from(env!("CARGO_BIN_EXE_2kbrowser")))
        .open(
            html.as_bytes().to_vec(),
            None,
            width,
            0,
            8000,
            Some(origin),
            at,
            false,
            false,
        )
        .expect("the renderer opens the page")
}

#[test]
fn a_band_asked_for_speculatively_arrives_without_being_waited_on() {
    // The point of putting the conversation on a thread. A reader approaching
    // the edge of what has been painted should not have to stop there, so the
    // rows ahead are asked for while the window carries on drawing — which
    // only works if asking does not block and arriving is announced.
    let (mut session, whole) = tall_session(120, 300);

    let woken = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&woken);
    session.set_wake(Box::new(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));

    session.request_band(200, 150).expect("asks");
    assert!(session.band_outstanding(), "the band should be in flight");

    let band = loop {
        if let Some(band) = session.take_band() {
            break band.expect("the band paints");
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert_eq!(band.top, 200);
    assert!(
        !session.band_outstanding(),
        "nothing should be left in flight"
    );
    assert!(
        woken.load(std::sync::atomic::Ordering::SeqCst) >= 1,
        "the wake callback never fired, so a window would never redraw"
    );

    let stride = (whole.width * 4) as usize;
    for n in 0..band.height {
        let from_band = &band.pixels[n as usize * stride..(n as usize + 1) * stride];
        let document_row = (200 + n) as usize;
        let from_whole = &whole.pixels[document_row * stride..(document_row + 1) * stride];
        assert_eq!(from_band, from_whole, "row {n} of the band");
    }
}

#[test]
fn a_blocking_question_does_not_swallow_a_band_in_flight() {
    // Answers come back in the order they were asked for, so waiting for a find
    // means reading past the band that was asked for first. Dropping it there
    // would leave the window waiting for pixels that already came and went —
    // and it would happen exactly when the reader is scrolling *and* searching,
    // which is not a rare combination.
    let (mut session, _) = tall_session(120, 300);

    session.request_band(300, 120).expect("asks for a band");
    let matches = session.find("line 7").expect("the child answers");
    assert!(!matches.is_empty(), "the fixture should contain the query");

    let band = session
        .take_band()
        .expect("the band asked for before the find was kept")
        .expect("the band paints");
    assert_eq!(band.top, 300);
}

#[test]
fn a_charset_that_only_the_header_knows_still_reaches_the_renderer() {
    // The bytes stay undecoded across the boundary because the encoding sniffer
    // lives with every other parser on the far side (ADR-0012). The header has
    // to travel with them: on a page that declares its encoding nowhere else,
    // the header is the only thing that knows.
    //
    // Found by looking at a screenshot. Hacker News says `charset=utf-8` in the
    // header and nothing at all in the markup, and the command line was passing
    // `content_type: None` — so every em dash and curly quote on it came out as
    // windows-1252 mojibake. The same page reached through a *link* decoded
    // correctly, because that path kept the header, which is the sort of
    // inconsistency nobody would think to look for.
    let utf8 = "<title>Möbius — 1,060 texts</title><body><p>Möbius</p></body>";
    assert!(
        !utf8.is_ascii(),
        "the fixture has to contain something that decodes differently"
    );

    let renderer =
        sandbox::Renderer::with_program(std::path::PathBuf::from(env!("CARGO_BIN_EXE_2kbrowser")));
    let titled = |content_type: Option<&str>| {
        renderer
            .render(
                utf8.as_bytes().to_vec(),
                content_type.map(str::to_owned),
                300,
                0,
                400,
                None,
                String::new(),
                false,
                false,
            )
            .expect("renders")
            .title
    };

    assert_eq!(
        titled(Some("text/html; charset=utf-8")).as_deref(),
        Some("Möbius — 1,060 texts"),
        "the header's charset did not reach the renderer"
    );
    // And without it the browser falls back to the era's default, which is what
    // makes the assertion above about the header rather than about the bytes.
    assert_ne!(
        titled(None).as_deref(),
        Some("Möbius — 1,060 texts"),
        "the fixture decodes the same either way, so it proves nothing"
    );
}

#[test]
fn the_pages_canvas_colour_crosses_the_boundary_with_it() {
    // The window paints rows the child sent no pixels for — below a page
    // shorter than the window, and ahead of a band still being painted — and
    // the only thing it can paint them is what the child told it the canvas
    // was. Nothing else in the parent knows what colour the page is: the box
    // tree and the display list are both on the other side.
    let mut page = viewport("<body><h1>Title</h1><p>An ordinary page.</p></body>", 300);
    let unpack = |packed: u32| {
        (
            (packed >> 16) as u8,
            ((packed >> 8) & 0xff) as u8,
            (packed & 0xff) as u8,
        )
    };
    let corner = &page.pixels()[..3];
    assert_eq!(
        unpack(page.background()),
        (corner[0], corner[1], corner[2]),
        "the child reported a canvas colour it did not paint"
    );

    // And it follows the page rather than being fixed: the document fallback
    // is dark, so the same page in the other layout reports another colour.
    let authored = page.background();
    page.set_forcing_document(true, 300, 2000)
        .expect("re-renders");
    assert_ne!(
        page.background(),
        authored,
        "the fallback reported the same canvas colour as the author's layout"
    );
    let (r, g, b) = unpack(page.background());
    let brightness = (0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)) / 255.0;
    assert!(brightness < 0.2, "the fallback canvas is {r},{g},{b}");
}
