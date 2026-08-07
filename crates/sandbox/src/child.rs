//! The untrusted side.
//!
//! Reads a render request from the pipe, renders, and writes back pixels. It
//! never opens a socket and never opens a file: anything the document
//! references is asked for, and the parent decides.
//!
//! Generic over the rendering itself, which is what keeps this crate below
//! `shell` rather than tangled with it. The caller supplies a [`Render`]; this
//! module supplies the conversation.

use std::io::{Read, Write};

use crate::message::{Rendered, ToChild, ToParent};
use crate::{Error, read_frame, write_frame};

/// What a subresource fetch returns.
///
/// A refusal and a failure are the same value, deliberately — see the parent's
/// note on why the child is not told which.
pub type Fetched = Option<Resource>;

/// A subresource the parent supplied.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Resource {
    /// The bytes.
    pub bytes: Vec<u8>,
    /// The `Content-Type` it was served with, when there was one.
    pub content_type: Option<String>,
}

/// Renders a document, asking `fetch` for anything it references.
pub trait Render {
    /// Renders one page.
    ///
    /// `fetch` is the only way out: it goes over the pipe to the parent, which
    /// applies the network policy. Returning `Err` produces a
    /// [`ToParent::Failed`] rather than a panic, so the parent gets a message
    /// to show instead of a dead child.
    fn render(
        &mut self,
        request: &ToChild,
        fetch: &mut dyn FnMut(&str, net::RequestKind) -> Fetched,
    ) -> Result<Rendered, String>;

    /// Where `query` appears on the page most recently rendered.
    ///
    /// A question rather than something the parent works out: the text and the
    /// box tree it searches never cross the boundary, so the only thing that
    /// can answer is the process holding them.
    fn find(&mut self, query: &str) -> Vec<layout::Rect>;
}

/// Runs the child's side of the conversation until the parent goes away.
///
/// One *page* per process, not one message. A page's lifetime includes the
/// questions asked of it while it is on screen — find, and re-rendering at a
/// new width — and those need the document and the box tree, which never cross
/// the boundary. Answering them means staying alive.
///
/// The security property is unchanged and is worth naming precisely: a child is
/// killed when the page it holds is replaced, so a page's leftovers — caches,
/// font state, whatever an exploit managed to leave behind — never outlive the
/// page. Reuse *across* pages is what would be dangerous, and that is what the
/// parent does not do.
pub fn serve(
    input: &mut impl Read,
    output: &mut impl Write,
    renderer: &mut impl Render,
) -> Result<(), Error> {
    loop {
        // A closed pipe is the parent exiting, which is how this ends.
        let frame = match read_frame(input) {
            Ok(frame) => frame,
            Err(Error::Died) => return Ok(()),
            Err(error) => return Err(error),
        };
        let request = ToChild::decode(&frame)?;
        match &request {
            ToChild::Render { .. } => serve_render(input, output, renderer, request)?,
            ToChild::Find { query } => {
                let rects = renderer.find(query);
                write_frame(output, &ToParent::Matches { rects }.encode())?;
            }
            // A `Resource` with nothing outstanding means the parent is not
            // what we think it is. Refusing beats guessing.
            ToChild::Resource { .. } => {
                write_frame(
                    output,
                    &ToParent::Failed {
                        message: "expected a render request".to_owned(),
                    }
                    .encode(),
                )?;
                return Ok(());
            }
        }
    }
}

/// Renders one page, answering the child's own resource requests along the way.
fn serve_render(
    input: &mut impl Read,
    output: &mut impl Write,
    renderer: &mut impl Render,
    request: ToChild,
) -> Result<(), Error> {
    // Errors inside the fetch closure are recorded rather than returned,
    // because the closure's signature belongs to the renderer and a broken pipe
    // is not something it can do anything about. The first failure stops
    // further requests, so a dead parent does not produce hundreds of retries.
    let mut transport: Result<(), Error> = Ok(());
    let outcome = {
        let mut fetch = |url: &str, kind: net::RequestKind| -> Fetched {
            if transport.is_err() {
                return None;
            }
            let asked = ToParent::Fetch {
                url: url.to_owned(),
                kind,
            };
            if let Err(error) = write_frame(output, &asked.encode()) {
                transport = Err(error);
                return None;
            }
            let answer = match read_frame(input) {
                Ok(frame) => frame,
                Err(error) => {
                    transport = Err(error);
                    return None;
                }
            };
            match ToChild::decode(&answer) {
                Ok(ToChild::Resource {
                    body,
                    content_type,
                    ok: true,
                }) => Some(Resource {
                    bytes: body,
                    content_type,
                }),
                Ok(_) => None,
                Err(error) => {
                    transport = Err(Error::Wire(error));
                    None
                }
            }
        };
        renderer.render(&request, &mut fetch)
    };
    transport?;

    let reply = match outcome {
        Ok(page) => ToParent::Rendered(Box::new(page)),
        Err(message) => ToParent::Failed { message },
    };
    write_frame(output, &reply.encode())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Mode;

    /// A renderer that returns a fixed page and records what it was asked for.
    struct Stub {
        wants: Vec<String>,
        got: Vec<Fetched>,
        fail: bool,
        /// Queries this stub was asked to find, so the loop can be checked.
        queried: Vec<String>,
    }

    impl Stub {
        fn new() -> Self {
            Self {
                wants: Vec::new(),
                got: Vec::new(),
                fail: false,
                queried: Vec::new(),
            }
        }

        fn wanting(mut self, urls: &[&str]) -> Self {
            self.wants = urls.iter().map(|url| (*url).to_owned()).collect();
            self
        }

        fn failing(mut self) -> Self {
            self.fail = true;
            self
        }
    }

    impl Render for Stub {
        fn render(
            &mut self,
            _: &ToChild,
            fetch: &mut dyn FnMut(&str, net::RequestKind) -> Fetched,
        ) -> Result<Rendered, String> {
            for url in self.wants.clone() {
                self.got.push(fetch(&url, net::RequestKind::Subresource));
            }
            if self.fail {
                return Err("nope".to_owned());
            }
            Ok(Rendered {
                pixels: vec![0; 4],
                width: 1,
                height: 1,
                content_height: 1.0,
                mode: Mode::Authored,
                title: None,
                links: Vec::new(),
                can_toggle_layout: false,
            })
        }

        fn find(&mut self, query: &str) -> Vec<layout::Rect> {
            self.queried.push(query.to_owned());
            vec![layout::Rect {
                x: 1.0,
                y: 2.0,
                width: 3.0,
                height: 4.0,
            }]
        }
    }

    fn request() -> Vec<u8> {
        ToChild::Render {
            body: b"<p>x</p>".to_vec(),
            content_type: None,
            width: 1,
            max_height: 1,
            origin: None,
            path: String::new(),
            force_authored: false,
        }
        .encode()
    }

    fn pipe(frames: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        for frame in frames {
            write_frame(&mut out, frame).expect("writes");
        }
        out
    }

    #[test]
    fn a_page_with_no_subresources_answers_immediately() {
        let input = pipe(&[request()]);
        let mut output = Vec::new();
        let mut stub = Stub::new();
        serve(&mut input.as_slice(), &mut output, &mut stub).expect("serves");

        let frame = read_frame(&mut output.as_slice()).expect("reads");
        assert!(matches!(
            ToParent::decode(&frame),
            Ok(ToParent::Rendered(_))
        ));
    }

    #[test]
    fn a_subresource_becomes_a_request_and_its_answer_comes_back() {
        let input = pipe(&[
            request(),
            ToChild::Resource {
                body: b"image bytes".to_vec(),
                content_type: None,
                ok: true,
            }
            .encode(),
        ]);
        let mut output = Vec::new();
        let mut stub = Stub::new().wanting(&["https://example.com/x.png"]);
        serve(&mut input.as_slice(), &mut output, &mut stub).expect("serves");
        assert_eq!(
            stub.got,
            vec![Some(Resource {
                bytes: b"image bytes".to_vec(),
                content_type: None,
            })]
        );

        let mut reading = output.as_slice();
        let asked = ToParent::decode(&read_frame(&mut reading).expect("reads")).expect("decodes");
        assert!(matches!(asked, ToParent::Fetch { .. }));
    }

    #[test]
    fn a_refused_subresource_reads_as_absent_rather_than_as_an_error() {
        // The child is not told whether the policy blocked it or the server was
        // down. Telling it would leak the parent's configuration to the
        // untrusted side.
        let input = pipe(&[
            request(),
            ToChild::Resource {
                body: Vec::new(),
                content_type: None,
                ok: false,
            }
            .encode(),
        ]);
        let mut output = Vec::new();
        let mut stub = Stub::new().wanting(&["https://tracker.example.net/pixel.gif"]);
        serve(&mut input.as_slice(), &mut output, &mut stub).expect("serves");
        assert_eq!(stub.got, vec![None]);
    }

    #[test]
    fn a_render_failure_becomes_a_message_rather_than_a_dead_child() {
        let input = pipe(&[request()]);
        let mut output = Vec::new();
        let mut stub = Stub::new().failing();
        serve(&mut input.as_slice(), &mut output, &mut stub).expect("serves");

        let frame = read_frame(&mut output.as_slice()).expect("reads");
        assert_eq!(
            ToParent::decode(&frame),
            Ok(ToParent::Failed {
                message: "nope".to_owned()
            })
        );
    }

    #[test]
    fn a_first_message_that_is_not_a_render_request_is_refused() {
        let input = pipe(&[ToChild::Resource {
            body: Vec::new(),
            content_type: None,
            ok: true,
        }
        .encode()]);
        let mut output = Vec::new();
        let mut stub = Stub::new();
        serve(&mut input.as_slice(), &mut output, &mut stub).expect("serves");

        let frame = read_frame(&mut output.as_slice()).expect("reads");
        assert!(matches!(
            ToParent::decode(&frame),
            Ok(ToParent::Failed { .. })
        ));
    }

    #[test]
    fn a_parent_that_goes_away_mid_fetch_stops_the_conversation() {
        // Only the render request, no answer to the fetch. The child must stop
        // rather than spin asking into a closed pipe.
        let input = pipe(&[request()]);
        let mut output = Vec::new();
        let mut stub = Stub::new().wanting(&["a", "b", "c"]);
        let outcome = serve(&mut input.as_slice(), &mut output, &mut stub);
        assert!(outcome.is_err(), "{outcome:?}");
        assert_eq!(
            stub.got,
            vec![None, None, None],
            "later fetches short-circuit instead of retrying"
        );
    }
}
