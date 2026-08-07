//! The trusted side.
//!
//! Spawns a renderer, hands it a document, answers its requests for
//! subresources, and takes back pixels. Everything a stranger wrote is parsed
//! on the far side of this.
//!
//! The network lives here rather than in the child, which is the part that
//! makes the boundary worth having: a renderer with no sockets cannot
//! exfiltrate anything regardless of what it is tricked into computing, and
//! ADR-0006's policy is enforced somewhere a compromised renderer cannot reach.

use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use net::{Fetcher, Origin, RequestKind};

use crate::message::{Rendered, ToChild, ToParent};
use crate::{CHILD_ARGUMENT, Error, read_frame, write_frame};

/// How long one page may take before the renderer is killed.
///
/// This is the half `tests/fuzz` could not have. It times each input after the
/// fact, so an input that never returns hangs the harness rather than being
/// reported — recorded as a known gap when it landed. A child process can be
/// killed; an in-process loop cannot.
///
/// Generous, because a large table-heavy page on a slow machine is legitimately
/// slow, and killing a page someone is waiting for is worse than waiting.
pub const RENDER_TIMEOUT: Duration = Duration::from_secs(20);

/// How many subresources one page may ask for.
///
/// The conversation is driven by the child, so without a bound a compromised
/// renderer could keep the parent fetching forever — a request loop is a denial
/// of service against whatever the parent is pointed at, not just against us.
pub const MAX_RESOURCES: usize = 512;

/// A renderer process and the conversation with it.
pub struct Renderer {
    program: PathBuf,
    fetcher: Fetcher,
    timeout: Duration,
}

impl Renderer {
    /// A renderer that re-invokes this executable.
    ///
    /// The child is the same binary, the way every browser does it: one copy of
    /// the font payload on disk, and no second thing to keep in step.
    pub fn new() -> Result<Self, Error> {
        let program = std::env::current_exe()
            .map_err(|error| Error::Spawn(format!("cannot find this executable: {error}")))?;
        Ok(Self {
            program,
            fetcher: Fetcher::default(),
            timeout: RENDER_TIMEOUT,
        })
    }

    /// A renderer that runs a named program instead. For tests.
    pub fn with_program(program: PathBuf) -> Self {
        Self {
            program,
            fetcher: Fetcher::default(),
            timeout: RENDER_TIMEOUT,
        }
    }

    /// Sets how long a page may take.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The policy applied to everything the child asks for.
    pub fn fetcher_mut(&mut self) -> &mut Fetcher {
        &mut self.fetcher
    }

    /// Renders a document in a child process.
    #[expect(
        clippy::too_many_arguments,
        reason = "the render request's fields, threaded explicitly"
    )]
    pub fn render(
        &self,
        body: Vec<u8>,
        content_type: Option<String>,
        width: u32,
        max_height: u32,
        origin: Option<Origin>,
        path: String,
        force_authored: bool,
    ) -> Result<Rendered, Error> {
        let mut child = Command::new(&self.program)
            .arg(CHILD_ARGUMENT)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Left inherited on purpose: a panic message from the child is the
            // most useful thing it can produce when something is wrong, and
            // swallowing it would make every renderer bug invisible.
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| Error::Spawn(error.to_string()))?;

        let outcome = self.converse(
            &mut child,
            ToChild::Render {
                body,
                content_type,
                width,
                max_height,
                origin: origin.clone(),
                path,
                force_authored,
            },
            origin.as_ref(),
        );

        // Killed unconditionally, including on success. A renderer that has
        // answered has nothing left to do, and one that is wedged must not
        // outlive the request that started it.
        let _ = child.kill();
        let _ = child.wait();
        outcome
    }

    fn converse(
        &self,
        child: &mut Child,
        request: ToChild,
        document: Option<&Origin>,
    ) -> Result<Rendered, Error> {
        let mut to_child = BufWriter::new(child.stdin.take().ok_or(Error::Died)?);
        let mut from_child = BufReader::new(child.stdout.take().ok_or(Error::Died)?);

        write_frame(&mut to_child, &request.encode())?;

        let started = Instant::now();
        let mut resources = 0usize;
        loop {
            // Checked between messages rather than during a read. A child that
            // is silently spinning is caught the moment it next speaks or its
            // pipe closes; one that is spinning *and* silent is caught by the
            // caller's own deadline. Interrupting a blocked read needs either a
            // thread per child or platform-specific polling, and neither buys
            // enough to be worth the complexity here.
            if started.elapsed() > self.timeout {
                return Err(Error::Render(format!(
                    "the page took longer than {}s to render",
                    self.timeout.as_secs()
                )));
            }

            let frame = read_frame(&mut from_child)?;
            match ToParent::decode(&frame)? {
                ToParent::Rendered(page) => return Ok(*page),
                ToParent::Failed { message } => return Err(Error::Render(message)),
                ToParent::Fetch { url, kind } => {
                    resources += 1;
                    if resources > MAX_RESOURCES {
                        return Err(Error::Render(format!(
                            "the page asked for more than {MAX_RESOURCES} resources"
                        )));
                    }
                    let answer = self.fetch(&url, document, kind);
                    write_frame(&mut to_child, &answer.encode())?;
                }
            }
        }
    }

    /// Fetches what the child asked for, subject to the policy.
    ///
    /// A refusal and a failure are reported identically. The child has no
    /// business knowing whether a resource was blocked or merely missing, and
    /// telling it would leak the parent's configuration to the untrusted side —
    /// which is exactly the sort of thing a compromised renderer would probe
    /// for.
    fn fetch(&self, url: &str, document: Option<&Origin>, kind: RequestKind) -> ToChild {
        match self.fetcher.fetch_bytes(url, document, kind) {
            Ok(body) => ToChild::Resource {
                body,
                content_type: None,
                ok: true,
            },
            Err(_) => ToChild::Resource {
                body: Vec::new(),
                content_type: None,
                ok: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_program_fails_rather_than_hanging() {
        let renderer = Renderer::with_program(PathBuf::from("/definitely/not/a/program"));
        let outcome = renderer.render(
            b"<p>x</p>".to_vec(),
            None,
            100,
            100,
            None,
            String::new(),
            false,
        );
        assert!(matches!(outcome, Err(Error::Spawn(_))), "{outcome:?}");
    }

    #[test]
    fn a_child_that_says_nothing_is_reported_as_dead() {
        // `true` exits immediately without writing a frame. The parent must
        // notice rather than block on bytes that are not coming.
        let program = ["/bin/true", "/usr/bin/true"]
            .iter()
            .map(PathBuf::from)
            .find(|path| path.exists());
        let Some(program) = program else {
            return;
        };
        let outcome = Renderer::with_program(program).render(
            b"<p>x</p>".to_vec(),
            None,
            100,
            100,
            None,
            String::new(),
            false,
        );
        assert!(
            matches!(outcome, Err(Error::Died) | Err(Error::Io(_))),
            "{outcome:?}"
        );
    }

    #[test]
    fn the_timeout_is_configurable_and_defaults_to_something_generous() {
        // Killing a page someone is waiting for is worse than waiting, so the
        // default is well past any legitimate render.
        assert!(RENDER_TIMEOUT >= Duration::from_secs(10));
        let renderer = Renderer::with_program(PathBuf::from("/nonexistent"))
            .with_timeout(Duration::from_millis(1));
        assert_eq!(renderer.timeout, Duration::from_millis(1));
    }
}
