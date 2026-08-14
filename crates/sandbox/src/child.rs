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
    ///
    /// It takes several URLs at once and answers positionally, one per URL.
    /// That is the only shape rather than one of two, so there is a single path
    /// through the pipe rather than a pair that could drift: asking for one
    /// thing is asking for a batch of one. Asking for several is what lets the
    /// parent fetch them at the same time, which is the difference between
    /// waiting for the sum of a page's latencies and waiting for the longest.
    fn render(
        &mut self,
        request: &ToChild,
        fetch: &mut dyn FnMut(&[String], net::RequestKind) -> Vec<Fetched>,
    ) -> Result<Rendered, String>;

    /// Where `query` appears on the page most recently rendered.
    ///
    /// A question rather than something the parent works out: the text and the
    /// box tree it searches never cross the boundary, so the only thing that
    /// can answer is the process holding them.
    fn find(&mut self, query: &str) -> Vec<layout::Rect>;

    /// Paints a different band of the page most recently rendered.
    ///
    /// Never fetches: the document is already parsed and laid out, and a band
    /// is only pixels. That is the whole reason a long page is affordable, and
    /// it is why this is not simply another `render` — a render can talk to the
    /// parent and this cannot.
    fn band(&mut self, top: u32, height: u32) -> Result<Rendered, String>;
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
            ToChild::Band { top, height } => {
                let answer = match renderer.band(*top, *height) {
                    Ok(page) => ToParent::Rendered(Box::new(page)),
                    Err(message) => ToParent::Failed { message },
                };
                write_frame(output, &answer.encode())?;
            }
            // Resources with nothing outstanding means the parent is not what
            // we think it is. Refusing beats guessing.
            ToChild::Resources { .. } => {
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
        let mut fetch = |urls: &[String], kind: net::RequestKind| -> Vec<Fetched> {
            let nothing = || vec![None; urls.len()];
            if transport.is_err() || urls.is_empty() {
                return nothing();
            }
            let asked = ToParent::Fetch {
                urls: urls.to_vec(),
                kind,
            };
            if let Err(error) = write_frame(output, &asked.encode()) {
                transport = Err(error);
                return nothing();
            }
            let answer = match read_frame(input) {
                Ok(frame) => frame,
                Err(error) => {
                    transport = Err(error);
                    return nothing();
                }
            };
            match ToChild::decode(&answer) {
                // One answer per URL, matched by position. A reply of the
                // wrong length is a parent that is not what we think it is,
                // and guessing which resource was which would be worse than
                // rendering the page without any of them.
                Ok(ToChild::Resources { resources }) if resources.len() == urls.len() => resources
                    .into_iter()
                    .map(|resource| {
                        resource.ok.then_some(Resource {
                            bytes: resource.body,
                            content_type: resource.content_type,
                        })
                    })
                    .collect(),
                Ok(_) => nothing(),
                Err(error) => {
                    transport = Err(Error::Wire(error));
                    nothing()
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
    use crate::message::{Mode, Supplied};

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
            fetch: &mut dyn FnMut(&[String], net::RequestKind) -> Vec<Fetched>,
        ) -> Result<Rendered, String> {
            let wants = self.wants.clone();
            if !wants.is_empty() {
                self.got
                    .extend(fetch(&wants, net::RequestKind::Subresource));
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
                images_loaded: 0,
                background: 0x00ff_ffff,
                top: 0,
            })
        }

        fn band(&mut self, _top: u32, _height: u32) -> Result<Rendered, String> {
            Err("the stub renderer paints no bands".to_owned())
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
            top: 0,
            height: 1,
            origin: None,
            path: String::new(),
            force_authored: false,
            force_document: false,
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
    fn a_batch_of_answers_lands_on_the_urls_that_asked_for_them() {
        // Nothing carries a request id: a batch is answered with exactly as
        // many resources as were asked for, matched by position. That is the
        // whole of the matching rule, so it is worth a test where getting it
        // wrong is visible — three answers that differ from each other, in an
        // order that is not symmetrical.
        let input = pipe(&[
            request(),
            ToChild::Resources {
                resources: vec![
                    Supplied {
                        body: b"first".to_vec(),
                        content_type: Some("text/css".to_owned()),
                        ok: true,
                    },
                    // The middle one could not be had, which must not shift the
                    // ones after it along.
                    Supplied::default(),
                    Supplied {
                        body: b"third".to_vec(),
                        content_type: Some("image/png".to_owned()),
                        ok: true,
                    },
                ],
            }
            .encode(),
        ]);
        let mut output = Vec::new();
        let mut stub = Stub::new().wanting(&["one.css", "two.css", "three.png"]);
        serve(&mut input.as_slice(), &mut output, &mut stub).expect("serves");

        assert_eq!(
            stub.got,
            vec![
                Some(Resource {
                    bytes: b"first".to_vec(),
                    content_type: Some("text/css".to_owned()),
                }),
                None,
                Some(Resource {
                    bytes: b"third".to_vec(),
                    content_type: Some("image/png".to_owned()),
                }),
            ]
        );

        // And all three were asked for in one message rather than three.
        let mut reading = output.as_slice();
        let asked = ToParent::decode(&read_frame(&mut reading).expect("reads")).expect("decodes");
        match asked {
            ToParent::Fetch { urls, .. } => assert_eq!(urls.len(), 3, "{urls:?}"),
            other => panic!("expected one batched request, got {other:?}"),
        }
    }

    #[test]
    fn an_answer_of_the_wrong_length_is_refused_rather_than_guessed_at() {
        // A reply with a different number of resources than were asked for
        // means the parent is not what we think it is. Lining them up anyway
        // would put one page's stylesheet where another's image belongs, which
        // is worse than rendering without either.
        let input = pipe(&[
            request(),
            ToChild::Resources {
                resources: vec![Supplied {
                    body: b"only one".to_vec(),
                    content_type: None,
                    ok: true,
                }],
            }
            .encode(),
        ]);
        let mut output = Vec::new();
        let mut stub = Stub::new().wanting(&["a.png", "b.png"]);
        serve(&mut input.as_slice(), &mut output, &mut stub).expect("serves");
        assert_eq!(
            stub.got,
            vec![None, None],
            "a short answer was spread across the requests anyway"
        );
    }

    #[test]
    fn a_subresource_becomes_a_request_and_its_answer_comes_back() {
        let input = pipe(&[
            request(),
            ToChild::Resources {
                resources: vec![Supplied {
                    body: b"image bytes".to_vec(),
                    content_type: None,
                    ok: true,
                }],
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
            ToChild::Resources {
                resources: vec![Supplied::default()],
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
        let input = pipe(&[ToChild::Resources {
            resources: Vec::new(),
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
