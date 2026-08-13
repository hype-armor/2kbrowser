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
        } else if cfg!(target_os = "macos") {
            (Confinement::AppSandbox, None)
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
        force_document: bool,
    ) -> Result<(Session, Rendered), Error> {
        let mut session = Session::new(self.spawn()?, self.fetcher.clone(), self.timeout)?;
        let page = session.render(
            body,
            content_type,
            width,
            top,
            height,
            origin,
            path,
            force_authored,
            force_document,
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
        force_document: bool,
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
            force_document,
        )
        .map(|(_, page)| page)
    }
}

/// What the worker is asked to do.
enum Job {
    Render(Box<RenderJob>),
    Band { top: u32, height: u32 },
    Find(String),
}

/// A render request, boxed because it carries the whole document.
struct RenderJob {
    body: Vec<u8>,
    content_type: Option<String>,
    width: u32,
    top: u32,
    height: u32,
    origin: Option<Origin>,
    path: String,
    force_authored: bool,
    force_document: bool,
}

/// Which request an answer belongs to.
///
/// Answers come back in the order they were asked for, because the worker is
/// serial, so the oldest outstanding request is whose answer this is. That is
/// all the matching this needs — no request ids, no map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Page,
    Band,
    Find,
}

/// What came back.
enum Answer {
    Rendered(Box<Rendered>),
    Matches(Vec<layout::Rect>),
    Failed(Error),
}

/// Called when an answer is ready, so an event loop can be woken.
type Wake = Box<dyn Fn() + Send + Sync>;

/// A live renderer holding one page.
///
/// Dropping it kills the child. That is the mechanism that keeps "one page per
/// process" true: the caller drops the session when the page is replaced, and
/// nothing a page accumulated — caches, font state, whatever an exploit left
/// behind — survives into the next one.
///
/// The conversation happens on a thread rather than here. That is what lets a
/// band be asked for and collected later instead of blocking whoever asked:
/// scrolling a long page can fetch the rows ahead of the reader while the
/// window keeps drawing, which is the whole point of doing it speculatively.
/// The pipes, the fetcher, and the policy all move onto that thread with the
/// conversation, because the child asks the parent for subresources *during* a
/// render and there is nobody else to answer.
pub struct Session {
    /// `None` once dropped, which closes the channel and ends the worker.
    jobs: Option<std::sync::mpsc::Sender<Job>>,
    answers: std::sync::mpsc::Receiver<Answer>,
    /// What has been asked for and not yet answered, oldest first.
    outstanding: std::collections::VecDeque<Kind>,
    /// A band that arrived while something else was being waited for.
    band: Option<Result<Rendered, Error>>,
    child_id: u32,
    wake: std::sync::Arc<std::sync::OnceLock<Wake>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Session {
    /// Starts the worker for a freshly spawned child.
    fn new(child: Spawned, fetcher: Fetcher, timeout: Duration) -> Result<Self, Error> {
        let child_id = child.id();
        let (jobs, work) = std::sync::mpsc::channel::<Job>();
        let (replies, answers) = std::sync::mpsc::channel::<Answer>();
        let wake: std::sync::Arc<std::sync::OnceLock<Wake>> = std::sync::Arc::default();
        let woken = std::sync::Arc::clone(&wake);

        let worker = std::thread::Builder::new()
            .name("renderer-session".to_owned())
            .spawn(move || {
                let mut conversation = Conversation {
                    child,
                    fetcher,
                    timeout,
                    document: None,
                };
                // Ends when the handle is dropped and the channel closes, which
                // is what kills the child: `Conversation` owns it.
                while let Ok(job) = work.recv() {
                    let answer = conversation.perform(job);
                    if replies.send(answer).is_err() {
                        break;
                    }
                    if let Some(wake) = woken.get() {
                        wake();
                    }
                }
            })
            .map_err(|error| Error::Spawn(format!("cannot start a renderer thread: {error}")))?;

        Ok(Self {
            jobs: Some(jobs),
            answers,
            outstanding: std::collections::VecDeque::new(),
            band: None,
            child_id,
            wake,
            worker: Some(worker),
        })
    }

    /// Sets what to call when an answer is ready.
    ///
    /// How a speculative band reaches a window that is otherwise asleep: winit
    /// waits for events rather than polling, so a band arriving has to be an
    /// event. Takes a callback rather than anything winit-shaped, because this
    /// crate has no business knowing what a window is.
    ///
    /// Only the first call counts. There is one owner of a session.
    pub fn set_wake(&self, wake: Wake) {
        let _ = self.wake.set(wake);
    }

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
        force_document: bool,
    ) -> Result<Rendered, Error> {
        self.submit(
            Job::Render(Box::new(RenderJob {
                body,
                content_type,
                width,
                top,
                height,
                origin,
                path,
                force_authored,
                force_document,
            })),
            Kind::Page,
        )?;
        match self.wait_for(Kind::Page)? {
            Answer::Rendered(page) => Ok(*page),
            Answer::Failed(error) => Err(error),
            Answer::Matches(_) => Err(Error::Wire(crate::WireError::Unknown)),
        }
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
        self.child_id
    }

    /// Repaints a different band of the page this child is holding, and waits.
    ///
    /// No fetching and no layout: the document is already parsed and laid out,
    /// so this costs the pixels and the pipe. That is what makes scrolling a
    /// long page affordable, and it is why it is not a `render` — a render can
    /// ask the parent for resources and this deliberately cannot.
    pub fn band(&mut self, top: u32, height: u32) -> Result<Rendered, Error> {
        self.request_band(top, height)?;
        match self.wait_for(Kind::Band)? {
            Answer::Rendered(page) => Ok(*page),
            Answer::Failed(error) => Err(error),
            Answer::Matches(_) => Err(Error::Wire(crate::WireError::Unknown)),
        }
    }

    /// Asks for a band without waiting for it.
    ///
    /// The speculative half: a reader approaching the edge of what has been
    /// painted should not have to stop there, so the rows ahead are asked for
    /// while the window carries on drawing the rows it has. Collect it with
    /// [`Session::take_band`], or be told by the callback given to
    /// [`Session::set_wake`].
    pub fn request_band(&mut self, top: u32, height: u32) -> Result<(), Error> {
        self.submit(Job::Band { top, height }, Kind::Band)
    }

    /// Whether a band has been asked for and not yet collected.
    pub fn band_outstanding(&self) -> bool {
        self.band.is_none() && self.outstanding.contains(&Kind::Band)
    }

    /// Takes a band that has arrived, if one has. Never blocks.
    pub fn take_band(&mut self) -> Option<Result<Rendered, Error>> {
        while self.band.is_none() {
            match self.answers.try_recv() {
                Ok(answer) => self.stash(answer),
                Err(_) => break,
            }
        }
        self.band.take()
    }

    /// Asks where `query` appears on the page this child is holding.
    pub fn find(&mut self, query: &str) -> Result<Vec<layout::Rect>, Error> {
        self.submit(Job::Find(query.to_owned()), Kind::Find)?;
        match self.wait_for(Kind::Find)? {
            Answer::Matches(rects) => Ok(rects),
            Answer::Failed(error) => Err(error),
            Answer::Rendered(_) => Err(Error::Wire(crate::WireError::Unknown)),
        }
    }

    fn submit(&mut self, job: Job, kind: Kind) -> Result<(), Error> {
        let jobs = self.jobs.as_ref().ok_or(Error::Died)?;
        jobs.send(job).map_err(|_| Error::Died)?;
        self.outstanding.push_back(kind);
        Ok(())
    }

    /// Files an answer against the oldest outstanding request.
    fn stash(&mut self, answer: Answer) {
        match self.outstanding.pop_front() {
            // A band nobody is waiting for is kept rather than dropped: it was
            // asked for on purpose and the window still wants it.
            Some(Kind::Band) => {
                self.band = Some(match answer {
                    Answer::Rendered(page) => Ok(*page),
                    Answer::Failed(error) => Err(error),
                    Answer::Matches(_) => Err(Error::Wire(crate::WireError::Unknown)),
                });
            }
            _ => drop(answer),
        }
    }

    /// Waits for the answer to the most recent request of `kind`.
    ///
    /// Anything that arrives first belongs to an earlier request; a band among
    /// them is kept for the window rather than thrown away.
    fn wait_for(&mut self, kind: Kind) -> Result<Answer, Error> {
        loop {
            let answer = self.answers.recv().map_err(|_| Error::Died)?;
            if self.outstanding.front() == Some(&kind) {
                self.outstanding.pop_front();
                return Ok(answer);
            }
            self.stash(answer);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Closing the channel is what ends the worker, and the worker owns the
        // child — so this is also what kills it. Unconditional, including after
        // a clean render: a renderer whose page is gone has nothing left to do,
        // and one that is wedged must not outlive the tab that started it.
        self.jobs = None;
        if let Some(worker) = self.worker.take() {
            // Joined rather than detached, so that when this returns the child
            // is gone rather than probably-gone. A test can then look for the
            // process, which is the only way "dropping kills it" is checkable.
            let _ = worker.join();
        }
    }
}

/// The child, the pipes, and the conversation — all on the worker thread.
struct Conversation {
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

impl Conversation {
    fn perform(&mut self, job: Job) -> Answer {
        let outcome = match job {
            Job::Render(request) => {
                self.document = request.origin.clone();
                self.converse(ToChild::Render {
                    body: request.body,
                    content_type: request.content_type,
                    width: request.width,
                    top: request.top,
                    height: request.height,
                    origin: request.origin,
                    path: request.path,
                    force_authored: request.force_authored,
                    force_document: request.force_document,
                })
                .map(|page| Answer::Rendered(Box::new(page)))
            }
            Job::Band { top, height } => self
                .converse(ToChild::Band { top, height })
                .map(|page| Answer::Rendered(Box::new(page))),
            Job::Find(query) => self.ask(&ToChild::Find { query }),
        };
        outcome.unwrap_or_else(Answer::Failed)
    }

    fn send(&mut self, message: &ToChild) -> Result<(), Error> {
        let stdin = self.child.stdin().ok_or(Error::Died)?;
        write_frame(stdin, &message.encode())
    }

    fn read(&mut self) -> Result<Vec<u8>, Error> {
        let stdout = self.child.stdout().ok_or(Error::Died)?;
        read_frame(stdout)
    }

    /// One question, one answer, no resource requests in between.
    fn ask(&mut self, request: &ToChild) -> Result<Answer, Error> {
        self.send(request)?;
        let frame = self.read()?;
        match ToParent::decode(&frame)? {
            ToParent::Matches { rects } => Ok(Answer::Matches(rects)),
            ToParent::Rendered(page) => Ok(Answer::Rendered(page)),
            ToParent::Failed { message } => Err(Error::Render(message)),
            ToParent::Fetch { .. } => Err(Error::Wire(crate::WireError::Unknown)),
        }
    }

    fn converse(&mut self, request: ToChild) -> Result<Rendered, Error> {
        self.send(&request)?;

        let started = Instant::now();
        let mut resources = 0usize;
        loop {
            // Checked between messages rather than during a read. A child that
            // is silently spinning is caught the moment it next speaks or its
            // pipe closes; one that is spinning *and* silent is caught by the
            // caller's own deadline. Interrupting a blocked read needs
            // platform-specific polling, and now that the conversation is on a
            // thread of its own a stuck one no longer takes the window with it.
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
    /// `fetch_raw` rather than anything that decodes: the parent hands over the
    /// bytes and the header saying how to read them, and the reading happens on
    /// the far side with every other parser (ADR-0012).
    fn fetch(&self, url: &str, kind: RequestKind) -> ToChild {
        match self.fetcher.fetch_raw(url, self.document.as_ref(), kind) {
            Ok(fetched) => ToChild::Resource {
                body: fetched.body,
                content_type: fetched.content_type,
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

impl Drop for Conversation {
    fn drop(&mut self) {
        // The worker owns the child, so this is where it dies — when the
        // channel closes because the `Session` handle was dropped. Unconditional,
        // including after a clean render: a renderer whose page is gone has
        // nothing left to do, and one that is wedged must not outlive the tab
        // that started it.
        self.child.kill();
    }
}

/// The worker moves a spawned child onto a thread, so it has to be able to go.
///
/// Asserted rather than assumed because half of it is a dependency's type on
/// Windows: `Contained` holds `rappct`'s handles, and if a future version made
/// one of them thread-bound this would stop compiling instead of quietly
/// forcing the conversation back onto the caller's thread.
const _: fn() = || {
    fn is_send<T: Send>() {}
    is_send::<Spawned>();
    is_send::<Fetcher>();
};

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
