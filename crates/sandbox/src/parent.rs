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

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use net::{Fetcher, Origin, RequestKind};

use crate::confine::Confinement;
use crate::message::{Rendered, ToChild, ToParent};
use crate::{CHILD_ARGUMENT, Error, read_frame, write_frame};

/// A renderer process, however it was started.
///
/// Two ways in: an ordinary child, and — on Windows — one launched into an
/// AppContainer, which cannot go through `Command` because the container has to
/// be attached at `CreateProcess` time (see [`crate::contain`]). Both ends up as
/// two pipes and something that kills the process, which is all the rest of this
/// file needs.
enum Spawned {
    Plain(Child),
    #[cfg(target_os = "windows")]
    Contained(crate::contain::Contained),
}

impl Spawned {
    fn stdin(&mut self) -> Option<&mut dyn Write> {
        match self {
            Spawned::Plain(child) => child.stdin.as_mut().map(|pipe| pipe as &mut dyn Write),
            #[cfg(target_os = "windows")]
            Spawned::Contained(child) => child.stdin().map(|pipe| pipe as &mut dyn Write),
        }
    }

    fn stdout(&mut self) -> Option<&mut dyn Read> {
        match self {
            Spawned::Plain(child) => child.stdout.as_mut().map(|pipe| pipe as &mut dyn Read),
            #[cfg(target_os = "windows")]
            Spawned::Contained(child) => child.stdout().map(|pipe| pipe as &mut dyn Read),
        }
    }

    fn id(&self) -> u32 {
        match self {
            Spawned::Plain(child) => child.id(),
            #[cfg(target_os = "windows")]
            Spawned::Contained(child) => child.id(),
        }
    }

    fn kill(&mut self) {
        match self {
            Spawned::Plain(child) => {
                let _ = child.kill();
                let _ = child.wait();
            }
            // Nothing to do: the job object was created with kill-on-close, so
            // dropping this closes the handle and the kernel kills the child.
            // That is stronger than an explicit `kill`, because it also fires
            // when the browser is killed outright rather than exiting.
            #[cfg(target_os = "windows")]
            Spawned::Contained(_) => {}
        }
    }
}

/// Starts an ordinary, unconfined child.
fn spawn_plain(program: &Path) -> Result<Spawned, Error> {
    let mut command = Command::new(program);
    // Nothing from the parent's environment, for the same reason the Windows
    // container names its own (see `contain::environment`): a browser's
    // environment routinely holds API tokens, proxy credentials, and the shape
    // of someone's home directory, and the renderer is the process that parses
    // documents strangers wrote. Nothing in the render path reads any of it —
    // the fonts are compiled in (ADR-0010) and every resource arrives over the
    // pipe — so there is nothing to lose by withholding all of it.
    //
    // `RUST_BACKTRACE` is the exception, and only when it is already set: a
    // renderer that panics is the case where its output matters most.
    command.env_clear();
    if let Some(backtrace) = std::env::var_os("RUST_BACKTRACE") {
        command.env("RUST_BACKTRACE", backtrace);
    }
    command
        .arg(CHILD_ARGUMENT)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Left inherited on purpose: a panic message from the child is the most
        // useful thing it can produce when something is wrong, and swallowing it
        // would make every renderer bug invisible.
        .stderr(Stdio::inherit())
        .spawn()
        .map(Spawned::Plain)
        .map_err(|error| Error::Spawn(error.to_string()))
}

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
    /// The container children are launched into, where the parent builds one.
    #[cfg(target_os = "windows")]
    container: Option<crate::contain::Container>,
    confinement: Confinement,
    /// Why the container could not be built, when that is what happened.
    failure: Option<String>,
    /// Why the container stopped being usable, if a launch failed.
    ///
    /// Set at most once, by the first launch that fails. Building a container
    /// can succeed and launching into it still fail — an environment block the
    /// machine will not accept, a policy that forbids it — and that is not
    /// discoverable until the first page.
    launch_failure: std::sync::OnceLock<String>,
}

impl Renderer {
    /// A renderer that re-invokes this executable.
    ///
    /// The child is the same binary, the way every browser does it: one copy of
    /// the font payload on disk, and no second thing to keep in step.
    pub fn new() -> Result<Self, Error> {
        let program = std::env::current_exe()
            .map_err(|error| Error::Spawn(format!("cannot find this executable: {error}")))?;
        Ok(Self::for_program(program))
    }

    /// A renderer that runs a named program instead. For tests.
    pub fn with_program(program: PathBuf) -> Self {
        Self::for_program(program)
    }

    /// The container is built once here rather than per page: creating the
    /// profile writes to the registry and granting the executable rewrites its
    /// ACL, and doing either on every navigation would be absurd.
    fn for_program(program: PathBuf) -> Self {
        #[cfg(target_os = "windows")]
        let (container, confinement, failure) = match crate::contain::Container::new(&program) {
            Ok(container) => (Some(container), Confinement::AppContainer, None),
            // Not fatal. A browser that refuses to render anything because it
            // could not build a sandbox is a browser nobody can use to find out
            // why; the failure is carried instead, and said once by the caller.
            Err(reason) => (None, Confinement::Failed, Some(reason)),
        };
        #[cfg(not(target_os = "windows"))]
        // What the child will install for itself after `exec`. It reports its
        // own failure — the parent cannot see it from here.
        let (confinement, failure) = if cfg!(target_os = "linux") {
            (Confinement::Seccomp, None)
        } else {
            (Confinement::Unavailable, None)
        };

        Self {
            program,
            fetcher: Fetcher::default(),
            timeout: RENDER_TIMEOUT,
            #[cfg(target_os = "windows")]
            container,
            confinement,
            failure,
            launch_failure: std::sync::OnceLock::new(),
        }
    }

    /// What confines the renderers this spawns.
    ///
    /// Half the story on Linux, and honestly so: there the child installs its
    /// own filter after `exec`, so this is what the build *will* apply rather
    /// than what it did, and the child prints if the install failed. On Windows
    /// the parent builds the container itself, so this is the outcome.
    pub fn confinement(&self) -> Confinement {
        if self.launch_failure.get().is_some() {
            return Confinement::Failed;
        }
        self.confinement
    }

    /// Why the container could not be built, if that is what happened.
    ///
    /// A sentence for the operator. `None` everywhere the parent does not build
    /// the confinement itself.
    pub fn confinement_failure(&self) -> Option<&str> {
        self.launch_failure
            .get()
            .map(String::as_str)
            .or(self.failure.as_deref())
    }

    /// Starts one renderer, contained if this platform's parent can contain it.
    ///
    /// A container that cannot launch falls back to an ordinary child, loudly
    /// and once. That is a security control degrading, so it is worth being
    /// explicit about why: the alternative is a browser that renders nothing at
    /// all on a machine where `CreateProcess` refuses the container, and a
    /// browser nobody can open is a browser nobody can use to find out why.
    /// It is the same answer already given when the container cannot be *built*
    /// — this is the same fact discovered one step later — and it is not
    /// silent: the reason is printed, [`Renderer::confinement`] then reports
    /// [`Confinement::Failed`], and the chrome says so at startup.
    fn spawn(&self) -> Result<Spawned, Error> {
        #[cfg(target_os = "windows")]
        if let Some(container) = &self.container
            && self.launch_failure.get().is_none()
        {
            match container.spawn(CHILD_ARGUMENT) {
                Ok(child) => return Ok(Spawned::Contained(child)),
                Err(reason) => {
                    // Said here rather than left to the caller, because this is
                    // the only place that knows, and said once because
                    // `OnceLock` admits exactly one writer.
                    eprintln!("2kbrowser: {reason}");
                    eprintln!(
                        "2kbrowser: {} — falling back to an unconfined renderer",
                        Confinement::Failed.describe()
                    );
                    let _ = self.launch_failure.set(reason);
                }
            }
        }
        spawn_plain(&self.program)
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

    /// Starts a renderer and renders a document in it.
    ///
    /// The child stays alive afterwards, holding the document and the box tree,
    /// so the page can still be searched and re-laid-out at a new width. It is
    /// killed when the [`Session`] is dropped, which the caller does when the
    /// page is replaced — so a page's leftovers never outlive the page.
    #[expect(
        clippy::too_many_arguments,
        reason = "the render request's fields, threaded explicitly"
    )]
    pub fn open(
        &self,
        body: Vec<u8>,
        content_type: Option<String>,
        width: u32,
        top: u32,
        height: u32,
        origin: Option<Origin>,
        path: String,
        force_authored: bool,
    ) -> Result<(Session, Rendered), Error> {
        let mut session = Session {
            child: self.spawn()?,
            fetcher: self.fetcher.clone(),
            timeout: self.timeout,
            document: origin.clone(),
        };
        let page = session.render(
            body,
            content_type,
            width,
            top,
            height,
            origin,
            path,
            force_authored,
        );
        match page {
            Ok(page) => Ok((session, page)),
            Err(error) => Err(error),
        }
    }

    /// Renders once and throws the child away.
    ///
    /// For callers with nothing to ask afterwards — the command line, and tests.
    #[expect(
        clippy::too_many_arguments,
        reason = "the render request's fields, threaded explicitly"
    )]
    pub fn render(
        &self,
        body: Vec<u8>,
        content_type: Option<String>,
        width: u32,
        top: u32,
        height: u32,
        origin: Option<Origin>,
        path: String,
        force_authored: bool,
    ) -> Result<Rendered, Error> {
        self.open(
            body,
            content_type,
            width,
            top,
            height,
            origin,
            path,
            force_authored,
        )
        .map(|(_, page)| page)
    }
}

/// A live renderer holding one page.
///
/// Dropping it kills the child. That is the mechanism that keeps "one page per
/// process" true: the caller drops the session when the page is replaced, and
/// nothing a page accumulated — caches, font state, whatever an exploit left
/// behind — survives into the next one.
pub struct Session {
    child: Spawned,
    fetcher: Fetcher,
    timeout: Duration,
    /// The origin the parent asked for a render of.
    ///
    /// Kept here rather than taken from the child's requests. The child could
    /// claim any origin it liked, and the policy would then be applied to a
    /// document that does not exist.
    document: Option<Origin>,
}

impl Session {
    /// Renders, or re-renders at a new width.
    ///
    /// Re-rendering in the same child is not only cheaper — the document is
    /// already parsed — it is what makes a resize not a fresh page.
    #[expect(
        clippy::too_many_arguments,
        reason = "the render request's fields, threaded explicitly"
    )]
    pub fn render(
        &mut self,
        body: Vec<u8>,
        content_type: Option<String>,
        width: u32,
        top: u32,
        height: u32,
        origin: Option<Origin>,
        path: String,
        force_authored: bool,
    ) -> Result<Rendered, Error> {
        self.document = origin.clone();
        self.converse(ToChild::Render {
            body,
            content_type,
            width,
            top,
            height,
            origin,
            path,
            force_authored,
        })
    }

    /// The renderer's process id.
    ///
    /// Exposed for one reason: so a test can go and look. Dropping a session is
    /// supposed to kill its child, and the two platforms do that by completely
    /// different means — an explicit `kill` on Unix, a job object closing on
    /// Windows — so "it works" is worth checking rather than asserting. The
    /// test that used to cover this said in its own comment that it checked the
    /// path was repeatable rather than that the child was gone.
    pub fn child_id(&self) -> u32 {
        self.child.id()
    }

    /// Repaints a different band of the page this child is holding.
    ///
    /// No fetching and no layout: the document is already parsed and laid out,
    /// so this costs the pixels and the pipe. That is what makes scrolling a
    /// long page affordable, and it is why it is not a `render` — a render can
    /// ask the parent for resources and this deliberately cannot.
    pub fn band(&mut self, top: u32, height: u32) -> Result<Rendered, Error> {
        self.send(&ToChild::Band { top, height })?;
        let frame = self.read()?;
        match ToParent::decode(&frame)? {
            ToParent::Rendered(page) => Ok(*page),
            ToParent::Failed { message } => Err(Error::Render(message)),
            _ => Err(Error::Wire(crate::WireError::Unknown)),
        }
    }

    /// Asks where `query` appears on the page this child is holding.
    pub fn find(&mut self, query: &str) -> Result<Vec<layout::Rect>, Error> {
        let request = ToChild::Find {
            query: query.to_owned(),
        };
        self.send(&request)?;
        let frame = self.read()?;
        match ToParent::decode(&frame)? {
            ToParent::Matches { rects } => Ok(rects),
            ToParent::Failed { message } => Err(Error::Render(message)),
            _ => Err(Error::Wire(crate::WireError::Unknown)),
        }
    }

    fn send(&mut self, message: &ToChild) -> Result<(), Error> {
        let stdin = self.child.stdin().ok_or(Error::Died)?;
        write_frame(stdin, &message.encode())
    }

    fn read(&mut self) -> Result<Vec<u8>, Error> {
        let stdout = self.child.stdout().ok_or(Error::Died)?;
        read_frame(stdout)
    }

    fn converse(&mut self, request: ToChild) -> Result<Rendered, Error> {
        self.send(&request)?;

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

            let frame = self.read()?;
            match ToParent::decode(&frame)? {
                ToParent::Rendered(page) => return Ok(*page),
                ToParent::Failed { message } => return Err(Error::Render(message)),
                ToParent::Matches { .. } => {
                    // Nothing asked a question. Either the child is confused or
                    // it is not ours.
                    return Err(Error::Wire(crate::WireError::Unknown));
                }
                ToParent::Fetch { url, kind } => {
                    resources += 1;
                    if resources > MAX_RESOURCES {
                        return Err(Error::Render(format!(
                            "the page asked for more than {MAX_RESOURCES} resources"
                        )));
                    }
                    let answer = self.fetch(&url, kind);
                    self.send(&answer)?;
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
    fn fetch(&self, url: &str, kind: RequestKind) -> ToChild {
        match self.fetcher.fetch_bytes(url, self.document.as_ref(), kind) {
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

impl Drop for Session {
    fn drop(&mut self) {
        // Unconditional, including after a clean render. A renderer whose page
        // is gone has nothing left to do, and one that is wedged must not
        // outlive the tab that started it.
        self.child.kill();
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
            0,
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
            0,
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
