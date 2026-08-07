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
        max_height: 2000,
        origin: None,
        path: String::new(),
        force_authored: false,
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
            &ToChild::Resource {
                body: Vec::new(),
                content_type: None,
                ok: true,
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
            2000,
            Some(origin),
            at,
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
            2000,
            Some(origin),
            at,
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

    // The proof that it is gone is that a fresh one still works — a leaked
    // child would eventually exhaust the process table rather than fail here,
    // so this checks the path is repeatable rather than the kill directly.
    for _ in 0..8 {
        let (live, page) = session("<body><p>x</p></body>", 200);
        assert!(page.width > 0);
        drop(live);
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
        .output()
        .expect("the selftest runs");
    let report = String::from_utf8_lossy(&output.stdout);

    // Skipped rather than failed where the sandbox is unavailable — an old
    // kernel, a container that forbids installing a filter, macOS. A check that
    // cannot run must not look like a check that passed, so it says so.
    if report.contains("confinement=Failed") || report.contains("confinement=Unavailable") {
        eprintln!("SKIP: no sandbox is available here\n{report}");
        return;
    }

    let expected = if cfg!(target_os = "windows") {
        "confinement=AppContainer"
    } else {
        "confinement=Seccomp"
    };
    assert!(report.contains(expected), "{report}");

    // Stated as the property rather than as an errno. `ConnectionRefused` is
    // the interesting one to exclude: it means the connect went all the way out
    // and something at the far end said no, which is proof the network was
    // reachable. Anything else is the attempt being stopped before that.
    let field = |name: &str| {
        report
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .unwrap_or_else(|| panic!("no `{name}` line in:\n{report}"))
            .to_owned()
    };
    let socket = field("socket=");
    assert!(
        socket != "OPENED" && socket != "ConnectionRefused",
        "the renderer could still reach the network (socket={socket}):\n{report}"
    );
    let file = field("file=");
    assert!(
        file != "OPENED",
        "the renderer could still open a file (file={file}):\n{report}"
    );
    assert!(
        !file.starts_with("PROBE-UNWRITABLE"),
        "the probe file was never written, so the file check proves nothing:\n{report}"
    );
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
