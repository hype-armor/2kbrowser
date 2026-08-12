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
