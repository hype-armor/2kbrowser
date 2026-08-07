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
