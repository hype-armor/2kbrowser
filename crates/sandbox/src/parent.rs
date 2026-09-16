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
use crate::message::{Rendered, Supplied, ToChild, ToParent};
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

/// How many subresources the parent will fetch at the same time.
///
/// A bound on the host at the other end as much as on this process. Browsers
/// settled on roughly this many per host decades ago, and a renderer that
/// opened five hundred sockets because a page named five hundred images would
/// be a denial of service wearing a page's clothes.
pub const MAX_CONCURRENT_FETCHES: usize = 6;

/// Which of `urls` still have to be fetched: allowed, not already held, and
/// each one only once however many times it appears.
///
/// The collapsing matters more than it looks. A batch's answers are only put in
/// the cache once the whole batch is done, so duplicates within one would all
/// miss together and all go to the network together — a page turning one
/// request into a hundred against somebody else's server. The renderer we ship
/// collapses repeats before they leave, since it knows which images a page
/// names; this is about the renderer we might be sent, which is the one the
/// boundary exists for.
fn still_wanted<'a>(urls: &'a [String], allowed: &[bool], held: &[String]) -> Vec<&'a str> {
    let mut out: Vec<&str> = Vec::new();
    for (url, allowed) in urls.iter().zip(allowed) {
        if *allowed && !held.contains(url) && !out.contains(&url.as_str()) {
            out.push(url);
        }
    }
    out
}

/// Fetches one URL, as one of a batch.
///
/// A free function rather than a method because it runs on a borrowed thread
/// and must touch nothing the conversation owns mutably: the cache is read
/// before the threads start and written after they have all finished.
fn supplied(
    fetcher: &Fetcher,
    url: &str,
    document: Option<&Origin>,
    kind: RequestKind,
) -> (Supplied, bool) {
    match fetcher.fetch_raw(url, document, kind) {
        Ok(fetched) => (
            Supplied {
                body: fetched.body,
                content_type: fetched.content_type,
                ok: true,
            },
            fetched.storable,
        ),
        // A failure is remembered but never *stored* across pages. Within a
        // page it stops a broken image being retried on every re-render; across
        // them it would turn one bad minute on somebody's network into a page
        // that stays broken for the rest of the run.
        Err(_) => (Supplied::default(), false),
    }
}

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
    /// Subresources kept across this run's pages (ADR-0018).
    ///
    /// Here rather than on the session because a session is one page: the whole
    /// point is to outlive one.
    cache: std::sync::Arc<std::sync::Mutex<Cache>>,
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
            cache: std::sync::Arc::default(),
        }
    }

    /// Forgets everything the cache holds for `site` (ADR-0018).
    ///
    /// What the reload control means. A reload that served the same cached
    /// bytes back would leave the one control a reader has over staleness doing
    /// nothing — and a stale page they cannot refresh is a page they have to
    /// restart the browser to see.
    ///
    /// Only that site, because reloading one page says nothing about any other.
    pub fn forget(&self, site: Option<&Origin>) {
        self.cache
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .forget(site);
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

    /// The policy itself, for a caller that only wants to read it.
    pub fn policy(&self) -> &net::Policy {
        &self.fetcher.policy
    }

    /// The policy, to change.
    ///
    /// Changing it affects the *next* child. A session clones the fetcher when
    /// it is spawned, so the page already on screen keeps the policy it was
    /// rendered under — which is the honest behaviour: granting an exception
    /// does not retroactively fetch what was already refused, and the caller
    /// has to ask for the page again.
    pub fn policy_mut(&mut self) -> &mut net::Policy {
        &mut self.fetcher.policy
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
        zoom: f32,
    ) -> Result<(Session, Rendered), Error> {
        let mut session = Session::new(
            self.spawn()?,
            self.fetcher.clone(),
            self.timeout,
            std::sync::Arc::clone(&self.cache),
        )?;
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
            zoom,
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
        zoom: f32,
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
            zoom,
        )
        .map(|(_, page)| page)
    }
}

/// What the worker is asked to do.
enum Job {
    Render(Box<RenderJob>),
    Band { top: u32, height: u32 },
    Find(String),
    Select { from: (f32, f32), to: (f32, f32) },
    Focus { at: (f32, f32) },
    Type { key: crate::message::Key },
    Choose { node: u32, index: u32 },
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
    zoom: f32,
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
    Select,
}

/// What came back.
enum Answer {
    Rendered(Box<Rendered>),
    Matches(Vec<layout::Rect>),
    Selected(Vec<layout::Rect>, String),
    Failed(Error),
}

/// Called when an answer is ready, so an event loop can be woken.
type Wake = Box<dyn Fn() + Send + Sync>;

/// A live renderer holding one page.
///
/// What the policy refused this page, for the chrome to report.
///
/// ADR-0006 removes advertising and tracking by refusing third-party
/// subresources, and until now it did it silently: a page missing half its
/// images looked exactly like a page whose server was having a bad day. Issue
/// #118 calls that dishonest rather than incorrect, and this is the record that
/// fixes it — what was asked for, what it was refused for, and on whose behalf.
///
/// Deliberately *not* sent to the child. A refusal and a failure are the same
/// shape on the wire on purpose (see [`Conversation::fetch_all`]), so this is
/// assembled on the parent's side of the boundary and read by the window
/// directly. A compromised renderer cannot use it to probe what the user has
/// allowed, because it never sees it.
#[derive(Debug, Default, Clone)]
pub struct Withheld {
    /// Refused URLs, deduplicated, in the order the page first asked.
    urls: Vec<String>,
    /// The hosts those URLs are on, deduplicated, in the order first seen.
    hosts: Vec<String>,
}

impl Withheld {
    /// How many distinct subresources this page asked for and did not get.
    ///
    /// Distinct, because a page that names the same tracking pixel in forty
    /// places asked for one thing forty times, and "40 blocked" would overstate
    /// what the reader is missing by thirty-nine.
    pub fn subresources(&self) -> usize {
        self.urls.len()
    }

    /// The hosts involved, in the order the page first asked for them.
    ///
    /// What a per-site exception is granted against: the user decides about
    /// `fonts.example.net`, not about each of its eleven files.
    pub fn hosts(&self) -> &[String] {
        &self.hosts
    }

    /// Whether anything was refused at all.
    pub fn is_empty(&self) -> bool {
        self.urls.is_empty()
    }

    /// Notes one refused URL. Repeats are ignored.
    fn record(&mut self, url: &str, host: &str) {
        if self.urls.iter().any(|seen| seen == url) {
            return;
        }
        self.urls.push(url.to_owned());
        if !self.hosts.iter().any(|seen| seen == host) {
            self.hosts.push(host.to_owned());
        }
    }
}

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
    /// What the policy refused this page, written by the worker.
    withheld: std::sync::Arc<std::sync::Mutex<Withheld>>,
}

impl Session {
    /// Starts the worker for a freshly spawned child.
    fn new(
        child: Spawned,
        fetcher: Fetcher,
        timeout: Duration,
        cache: std::sync::Arc<std::sync::Mutex<Cache>>,
    ) -> Result<Self, Error> {
        let child_id = child.id();
        let (jobs, work) = std::sync::mpsc::channel::<Job>();
        let (replies, answers) = std::sync::mpsc::channel::<Answer>();
        let wake: std::sync::Arc<std::sync::OnceLock<Wake>> = std::sync::Arc::default();
        let woken = std::sync::Arc::clone(&wake);
        let withheld: std::sync::Arc<std::sync::Mutex<Withheld>> = std::sync::Arc::default();
        let recorded = std::sync::Arc::clone(&withheld);

        let worker = std::thread::Builder::new()
            .name("renderer-session".to_owned())
            .spawn(move || {
                let mut conversation = Conversation {
                    child,
                    fetcher,
                    fetched: cache,
                    timeout,
                    document: None,
                    withheld: recorded,
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
            withheld,
        })
    }

    /// What the policy refused this page.
    ///
    /// Rebuilt from scratch on every render rather than accumulated, so a
    /// resize does not double the number the reader is shown: the child asks
    /// again for everything it needs, the allowed ones come from the page's
    /// cache, and the refused ones are refused again.
    pub fn withheld(&self) -> Withheld {
        // A panic on the worker thread while this was held would poison the
        // lock, and a poisoned lock must not take the window with it. What is
        // inside is a list of hostnames; the worst a half-written one can do is
        // under-report, and the render that panicked has already failed.
        self.withheld
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
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
        zoom: f32,
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
                zoom,
            })),
            Kind::Page,
        )?;
        match self.wait_for(Kind::Page)? {
            Answer::Rendered(page) => Ok(*page),
            Answer::Failed(error) => Err(error),
            _ => Err(Error::Wire(crate::WireError::Unknown)),
        }
    }

    /// Tells the child the reader pressed a point on the page (#110).
    ///
    /// Comes back as a fresh render, because focusing draws a ring and a caret
    /// that were not there before. The parent never learns *what* was focused —
    /// only whether anything was, which is all it needs to decide where the
    /// next keystroke goes.
    pub fn focus(&mut self, at: (f32, f32)) -> Result<Rendered, Error> {
        self.submit(Job::Focus { at }, Kind::Page)?;
        match self.wait_for(Kind::Page)? {
            Answer::Rendered(page) => Ok(*page),
            Answer::Failed(error) => Err(error),
            _ => Err(Error::Wire(crate::WireError::Unknown)),
        }
    }

    /// Sends a keystroke to whatever the child has focused.
    pub fn type_key(&mut self, key: crate::message::Key) -> Result<Rendered, Error> {
        self.submit(Job::Type { key }, Kind::Page)?;
        match self.wait_for(Kind::Page)? {
            Answer::Rendered(page) => Ok(*page),
            Answer::Failed(error) => Err(error),
            _ => Err(Error::Wire(crate::WireError::Unknown)),
        }
    }

    /// Tells the child which row of the dropdown it opened was chosen.
    ///
    /// `node` is the child's own name for the `<select>`, from the [`Dropdown`]
    /// it sent, handed back untouched. This side does not read it: it is an
    /// index into an arena in another process, and the only correct thing to do
    /// with it is give it back.
    ///
    /// [`Dropdown`]: crate::message::Dropdown
    pub fn choose(&mut self, node: u32, index: u32) -> Result<Rendered, Error> {
        self.submit(Job::Choose { node, index }, Kind::Page)?;
        match self.wait_for(Kind::Page)? {
            Answer::Rendered(page) => Ok(*page),
            Answer::Failed(error) => Err(error),
            _ => Err(Error::Wire(crate::WireError::Unknown)),
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
            _ => Err(Error::Wire(crate::WireError::Unknown)),
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
            _ => Err(Error::Wire(crate::WireError::Unknown)),
        }
    }

    /// What lies between two points of the page, and where it is.
    ///
    /// Asked of the child because the text is in the box tree, which is on
    /// that side. The two points are all that crosses.
    pub fn select(
        &mut self,
        from: (f32, f32),
        to: (f32, f32),
    ) -> Result<(Vec<layout::Rect>, String), Error> {
        self.submit(Job::Select { from, to }, Kind::Select)?;
        match self.wait_for(Kind::Select)? {
            Answer::Selected(rects, text) => Ok((rects, text)),
            Answer::Failed(error) => Err(error),
            _ => Err(Error::Wire(crate::WireError::Unknown)),
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
                    _ => Err(Error::Wire(crate::WireError::Unknown)),
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

/// Most the cache may hold, in bytes.
///
/// The era fixture peaks at around 27 MB across both processes against the
/// budget harness's limit of 100, so this is sized to be a comfortable fraction
/// of the room left rather than to hold everything. It is the same number the
/// per-page cache used, now spent once for the run instead of once per page —
/// which is a smaller total, not a larger one.
const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024;

/// What a document is allowed to be served from the cache (ADR-0018).
///
/// The pair, and not the URL alone. ADR-0006 already refuses third-party
/// subresources by default, and ADR-0003 removes every way a page could time a
/// load and report the answer — which is why this browser does not need cache
/// partitioning for the reason Chrome and Firefox did. What it does need it for
/// is ADR-0006's own exception: a host a reader has allowed becomes reachable
/// from more than one site, and a cache keyed on the URL alone would make it a
/// cross-site identifier — the exact shape the third-party rule exists to
/// remove, rebuilt by consent.
///
/// The scheme stays in the URL half because `Origin::is_same_site` compares
/// *host only*: `http://example.com` and `https://example.com` are one site to
/// the policy. Normalising the scheme out of the key would let somebody on
/// plain HTTP poison an entry a secure page then reads.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    /// The host of the document that asked. Empty for a document with no
    /// origin, which cannot share with a named one because the strings differ.
    site: String,
    /// The full URL, scheme included.
    url: String,
}

/// One cached answer, and what it costs.
#[derive(Debug, Clone)]
struct Entry {
    answer: Supplied,
    /// Bytes of body, counted once so eviction does not re-measure.
    bytes: usize,
    /// The clock reading when this was last served, for eviction.
    used: u64,
}

/// Subresources kept across the pages of one run (ADR-0018).
///
/// This is the single exception to a rule this codebase states repeatedly — *a
/// page's leftovers never outlive the page* — and it is written down in
/// ADR-0018 rather than discovered here. The child process is still torn down
/// between pages; this is the parent's store, and it lives on `Renderer`, which
/// outlives the `Conversation` that consults it.
///
/// In memory, for one run. On disk is a persistent record of what a person has
/// read, against a README that says bookmarks are the only state this browser
/// keeps between runs. That is a separate and much larger decision, and this
/// does not license it.
#[derive(Debug, Default)]
pub struct Cache {
    entries: std::collections::HashMap<Key, Entry>,
    /// How many bytes of body they hold between them.
    bytes: usize,
    /// Ticks once per access, so the least recently used is the smallest.
    clock: u64,
}

impl Cache {
    fn key(site: Option<&Origin>, url: &str) -> Key {
        Key {
            site: site.map(|origin| origin.host.clone()).unwrap_or_default(),
            url: url.to_owned(),
        }
    }

    /// The answer held for `url` on behalf of a document from `site`.
    ///
    /// Never call this before the policy has been applied to `url`. Per page,
    /// getting that ordering wrong cost one page; across pages it would be any
    /// page served anything any earlier page fetched.
    fn get(&mut self, site: Option<&Origin>, url: &str) -> Option<Supplied> {
        self.clock += 1;
        let clock = self.clock;
        let entry = self.entries.get_mut(&Self::key(site, url))?;
        entry.used = clock;
        Some(entry.answer.clone())
    }

    /// Remembers an answer, evicting the least recently used to make room.
    ///
    /// A policy rather than stop-when-full, which was fine for one page's
    /// lifetime and is not fine for something that outlives pages: a store that
    /// fills once and then never changes stops being a cache of what is being
    /// read and becomes a cache of whatever was read first.
    fn put(&mut self, site: Option<&Origin>, url: &str, answer: &Supplied, storable: bool) {
        if !storable {
            return;
        }
        let size = answer.body.len();
        if size > MAX_CACHE_BYTES {
            return;
        }
        self.clock += 1;
        let key = Self::key(site, url);
        if let Some(previous) = self.entries.remove(&key) {
            self.bytes -= previous.bytes;
        }
        while self.bytes + size > MAX_CACHE_BYTES {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&oldest) {
                self.bytes -= evicted.bytes;
            }
        }
        self.bytes += size;
        self.entries.insert(
            key,
            Entry {
                answer: answer.clone(),
                bytes: size,
                used: self.clock,
            },
        );
    }

    /// Forgets everything a given site was served.
    ///
    /// What reload means. Without it the one control a reader has over
    /// staleness does nothing, which is worse than having no cache: a stale
    /// page they cannot refresh is a page they have to restart the browser to
    /// see. Only that site's entries, because reloading one page is not a
    /// statement about any other.
    pub fn forget(&mut self, site: Option<&Origin>) {
        let site = site.map(|origin| origin.host.clone()).unwrap_or_default();
        self.entries.retain(|key, entry| {
            let keep = key.site != site;
            if !keep {
                self.bytes -= entry.bytes;
            }
            keep
        });
    }
}

/// The child, the pipes, and the conversation — all on the worker thread.
struct Conversation {
    child: Spawned,
    fetcher: Fetcher,
    /// Subresources fetched for the pages of this run (ADR-0018).
    ///
    /// Shared with the `Renderer`, which outlives this conversation: the store
    /// is what makes going back to a page you were just on cheap, and one that
    /// died with the session could not.
    fetched: std::sync::Arc<std::sync::Mutex<Cache>>,
    timeout: Duration,
    /// The origin the parent asked for a render of.
    ///
    /// Kept here rather than taken from the child's requests. The child could
    /// claim any origin it liked, and the policy would then be applied to a
    /// document that does not exist.
    document: Option<Origin>,
    /// What the policy refused, shared with the [`Session`] handle.
    withheld: std::sync::Arc<std::sync::Mutex<Withheld>>,
}

impl Conversation {
    fn perform(&mut self, job: Job) -> Answer {
        let outcome = match job {
            Job::Render(request) => {
                self.document = request.origin.clone();
                // A fresh page, or the same one at a new width. Either way the
                // child is about to ask for its subresources again, so the
                // record is rebuilt rather than added to.
                *self.record() = Withheld::default();
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
                    zoom: request.zoom,
                })
                .map(|page| Answer::Rendered(Box::new(page)))
            }
            Job::Band { top, height } => self
                .converse(ToChild::Band { top, height })
                .map(|page| Answer::Rendered(Box::new(page))),
            Job::Find(query) => self.ask(&ToChild::Find { query }),
            Job::Select { from, to } => self.ask(&ToChild::Select { from, to }),
            // Through `converse` rather than `ask`, because these re-render:
            // a field that grew a line can bring a new row of the page into
            // the band, and a new row can want an image.
            Job::Focus { at } => self
                .converse(ToChild::Focus { at })
                .map(|page| Answer::Rendered(Box::new(page))),
            Job::Type { key } => self
                .converse(ToChild::Type { key })
                .map(|page| Answer::Rendered(Box::new(page))),
            Job::Choose { node, index } => self
                .converse(ToChild::Choose { node, index })
                .map(|page| Answer::Rendered(Box::new(page))),
        };
        outcome.unwrap_or_else(Answer::Failed)
    }

    /// The refusal record, unpoisoned. See [`Session::withheld`].
    fn record(&self) -> std::sync::MutexGuard<'_, Withheld> {
        self.withheld
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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
            ToParent::Selected { rects, text } => Ok(Answer::Selected(rects, text)),
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
                ToParent::Matches { .. } | ToParent::Selected { .. } => {
                    // Nothing asked a question. Either the child is confused or
                    // it is not ours.
                    return Err(Error::Wire(crate::WireError::Unknown));
                }
                ToParent::Fetch { urls, kind } => {
                    // Counted per URL rather than per message, so asking in
                    // batches cannot be a way around the ceiling.
                    resources += urls.len();
                    if resources > MAX_RESOURCES {
                        return Err(Error::Render(format!(
                            "the page asked for more than {MAX_RESOURCES} resources"
                        )));
                    }
                    let answer = self.fetch_all(&urls, kind);
                    self.send(&answer)?;
                }
            }
        }
    }

    /// Fetches everything the child asked for, subject to the policy.
    ///
    /// A refusal and a failure are reported identically. The child has no
    /// business knowing whether a resource was blocked or merely missing, and
    /// telling it would leak the parent's configuration to the untrusted side —
    /// which is exactly the sort of thing a compromised renderer would probe
    /// for.
    ///
    /// `fetch_raw` rather than anything that decodes: the parent hands over the
    /// bytes and the header saying how to read them, and the reading happens on
    /// the far side with every other parser (ADR-0012).
    ///
    /// Answers positionally, one per URL, including for URLs that appear twice
    /// in one batch — a page that uses the same spacer forty times gets forty
    /// answers and one fetch.
    fn fetch_all(&mut self, urls: &[String], kind: RequestKind) -> ToChild {
        // The policy is applied to every URL before anything is fetched or
        // remembered, and before the cache is consulted — never after. A cached
        // answer served without it would let a page ask once from a context
        // where it was allowed and be answered for ever after; ADR-0006's rule
        // is about who is asking as much as about what for, and the origin
        // asking can change within one live child. `fetch_raw` checks again on
        // a miss, which costs a URL parse and keeps the rule in one place
        // rather than depending on this having got there first.
        //
        // The refusal is looked at rather than only counted, because its reason
        // is what the chrome has to say (issue #118): "three images from two
        // hosts were not loaded" is a sentence a reader can act on, and "three
        // resources failed" is not. Counted here as well as in `net`, because a
        // URL refused in this pre-pass is dropped from `wanted` and never
        // reaches the fetch that would otherwise have counted it.
        let allowed: Vec<bool> = urls
            .iter()
            .map(|url| {
                let Ok((origin, _)) = net::parse_url(url) else {
                    return false;
                };
                let Err(refusal) = self
                    .fetcher
                    .policy
                    .check(self.document.as_ref(), &origin, kind)
                else {
                    return true;
                };
                net::count_refusal(&refusal);
                if let net::Refusal::ThirdParty { host } = &refusal {
                    self.record().record(url, host);
                }
                false
            })
            .collect();

        // The lock is taken to look up, released, the fetches made, and taken
        // again to insert. Two concurrent misses on the same URL do the work
        // twice, which is waste rather than error — where holding it across a
        // fetch would serialise the concurrency below and cost the page the
        // very thing that concurrency exists to give it.
        let held: Vec<String> = {
            let mut cache = self.fetched.lock().unwrap_or_else(|held| held.into_inner());
            urls.iter()
                .filter(|url| cache.get(self.document.as_ref(), url).is_some())
                .cloned()
                .collect()
        };
        let wanted = still_wanted(urls, &allowed, &held);

        // The concurrency this whole change exists for. A page waited for the
        // sum of its subresources' latencies rather than the longest of them,
        // which on twenty images is twenty round trips one after another.
        //
        // Bounded, and to a small number: the cap is about the host at the
        // other end as much as about this process. Browsers settled on roughly
        // this many per host decades ago, and a renderer that opened five
        // hundred sockets at once because a page named five hundred images
        // would be a denial of service wearing a page's clothes.
        let fetcher = &self.fetcher;
        let document = self.document.as_ref();
        let mut fresh: Vec<(String, (Supplied, bool))> = Vec::new();
        for chunk in wanted.chunks(MAX_CONCURRENT_FETCHES) {
            let done: Vec<(Supplied, bool)> = std::thread::scope(|scope| {
                let handles: Vec<_> = chunk
                    .iter()
                    .map(|url| scope.spawn(move || supplied(fetcher, url, document, kind)))
                    .collect();
                handles
                    .into_iter()
                    // A panicking fetch is a failed resource rather than a
                    // failed page: the thread is ours, but what it was parsing
                    // came from somewhere else.
                    .map(|handle| handle.join().unwrap_or_default())
                    .collect()
            });
            fresh.extend(chunk.iter().map(|url| (*url).to_owned()).zip(done));
        }

        let mut cache = self.fetched.lock().unwrap_or_else(|held| held.into_inner());
        for (url, (answer, storable)) in &fresh {
            cache.put(self.document.as_ref(), url, answer, *storable);
        }

        let resources = urls
            .iter()
            .zip(&allowed)
            .map(|(url, allowed)| {
                if !allowed {
                    return Supplied::default();
                }
                // What was just fetched first, because a response the server
                // said not to store is in `fresh` and will never be in the
                // cache — and this page asked for it and must still be given
                // it. Only keeping it out of the *next* page is what `no-store`
                // means here.
                fresh
                    .iter()
                    .find(|(fetched, _)| fetched == url)
                    .map(|(_, (answer, _))| answer.clone())
                    .or_else(|| cache.get(self.document.as_ref(), url))
                    .unwrap_or_default()
            })
            .collect();
        ToChild::Resources { resources }
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

    fn resource(bytes: usize) -> Supplied {
        Supplied {
            body: vec![0; bytes],
            content_type: None,
            ok: true,
        }
    }

    #[test]
    fn what_was_withheld_counts_resources_once_and_hosts_once() {
        let mut withheld = Withheld::default();
        withheld.record("https://cdn.example.net/a.png", "cdn.example.net");
        withheld.record("https://cdn.example.net/b.png", "cdn.example.net");
        // The same file asked for again — a page naming one spacer in forty
        // places asked for one thing, and saying "40 blocked" would overstate
        // what the reader is missing by thirty-nine.
        withheld.record("https://cdn.example.net/a.png", "cdn.example.net");
        withheld.record("https://ads.example.org/pixel.gif", "ads.example.org");

        assert_eq!(withheld.subresources(), 3);
        assert_eq!(
            withheld.hosts(),
            ["cdn.example.net", "ads.example.org"],
            "hosts are listed once each, in the order the page first asked"
        );
        assert!(!withheld.is_empty());
        assert!(Withheld::default().is_empty());
    }

    #[test]
    fn a_batch_asking_for_the_same_thing_many_times_fetches_it_once() {
        // Not something the renderer we ship does — it collapses repeats before
        // they leave, because it knows which images a page names. This is about
        // the renderer we might be sent. A batch's answers only reach the cache
        // once the whole batch is done, so duplicates inside one would all miss
        // together and all go out together: a page turning one request into a
        // hundred against somebody else's server.
        let urls: Vec<String> = ["a", "b", "a", "a", "b"]
            .iter()
            .map(|name| format!("https://example.com/{name}"))
            .collect();
        assert_eq!(
            still_wanted(&urls, &[true; 5], &[]),
            vec!["https://example.com/a", "https://example.com/b"]
        );
    }

    #[test]
    fn nothing_refused_or_already_held_is_fetched_again() {
        let urls: Vec<String> = [
            "https://example.com/held",
            "https://example.com/refused",
            "https://example.com/wanted",
        ]
        .iter()
        .map(|url| (*url).to_owned())
        .collect();
        assert_eq!(
            still_wanted(
                &urls,
                &[true, false, true],
                &["https://example.com/held".to_owned()],
            ),
            vec!["https://example.com/wanted"],
            "something already held, or refused by the policy, was fetched anyway"
        );
    }

    fn site(host: &str) -> Origin {
        Origin {
            scheme: net::Scheme::Https,
            host: host.to_owned(),
            port: 443,
        }
    }

    #[test]
    fn what_the_cache_holds_is_bounded() {
        // It is filled by whatever pages ask for, and a page is a stranger's.
        // Without a ceiling, documents referencing enough large resources would
        // have the *parent* hold all of them — the process that is supposed to
        // be the trustworthy one, and the one the memory budget is measured
        // against.
        let mut cache = Cache::default();
        let a = site("example.com");
        let half = MAX_CACHE_BYTES / 2 + 1;
        cache.put(Some(&a), "https://example.com/a", &resource(half), true);
        cache.put(Some(&a), "https://example.com/b", &resource(half), true);

        assert!(
            cache.bytes <= MAX_CACHE_BYTES,
            "{} bytes held against a limit of {MAX_CACHE_BYTES}",
            cache.bytes
        );
    }

    #[test]
    fn the_least_recently_used_entry_is_the_one_evicted() {
        // Not the first one in. A store that fills once and then never changes
        // stops being a cache of what is being read and becomes a cache of
        // whatever was read first, which is what stop-when-full gave — fine for
        // one page's lifetime, not for something that outlives pages.
        let mut cache = Cache::default();
        let a = site("example.com");
        let third = MAX_CACHE_BYTES / 3 + 1;
        cache.put(Some(&a), "https://example.com/1", &resource(third), true);
        cache.put(Some(&a), "https://example.com/2", &resource(third), true);
        // Touching the first makes the second the oldest.
        assert!(cache.get(Some(&a), "https://example.com/1").is_some());
        cache.put(Some(&a), "https://example.com/3", &resource(third), true);

        assert!(
            cache.get(Some(&a), "https://example.com/1").is_some(),
            "the one that was used again was thrown away",
        );
        assert!(
            cache.get(Some(&a), "https://example.com/2").is_none(),
            "the oldest survived and something else went instead",
        );
    }

    #[test]
    fn one_site_is_not_served_what_another_fetched() {
        // ADR-0018's second term. ADR-0006's per-site exception lets a reader
        // allow a host, which makes it reachable from more than one site — and
        // a cache keyed on the URL alone would then be a cross-site identifier,
        // the exact shape the third-party rule exists to remove.
        let mut cache = Cache::default();
        let url = "https://cdn.example.net/spacer.gif";
        cache.put(Some(&site("one.example")), url, &resource(16), true);

        assert!(cache.get(Some(&site("one.example")), url).is_some());
        assert!(
            cache.get(Some(&site("two.example")), url).is_none(),
            "a second site was served the first site's bytes",
        );
    }

    #[test]
    fn the_scheme_stays_in_the_key() {
        // `Origin::is_same_site` compares host only, so `http://example.com`
        // and `https://example.com` are one site to the policy. If the scheme
        // were normalised out of the URL half as well, somebody on plain HTTP
        // could poison an entry a secure page then reads.
        let mut cache = Cache::default();
        let a = site("example.com");
        cache.put(Some(&a), "http://example.com/x", &resource(16), true);

        assert!(cache.get(Some(&a), "http://example.com/x").is_some());
        assert!(
            cache.get(Some(&a), "https://example.com/x").is_none(),
            "the secure page was served the insecure entry",
        );
    }

    #[test]
    fn a_response_that_says_no_store_is_not_kept() {
        let mut cache = Cache::default();
        let a = site("example.com");
        cache.put(Some(&a), "https://example.com/secret", &resource(16), false);
        assert!(cache.get(Some(&a), "https://example.com/secret").is_none());
    }

    #[test]
    fn reload_forgets_that_site_and_leaves_the_others() {
        let mut cache = Cache::default();
        let (one, two) = (site("one.example"), site("two.example"));
        cache.put(Some(&one), "https://one.example/a", &resource(16), true);
        cache.put(Some(&two), "https://two.example/a", &resource(16), true);

        cache.forget(Some(&one));

        assert!(cache.get(Some(&one), "https://one.example/a").is_none());
        assert!(
            cache.get(Some(&two), "https://two.example/a").is_some(),
            "reloading one page threw away another site's bytes",
        );
        assert_eq!(cache.bytes, 16, "the byte count did not follow the entries");
    }

    #[test]
    fn remembering_the_same_url_twice_counts_it_once() {
        // A page that asks for the same sheet forty times is the case this
        // exists for. Counting each answer again would have the cache believe
        // it was full long before it was.
        let mut cache = Cache::default();
        let a = site("example.com");
        for _ in 0..40 {
            cache.put(
                Some(&a),
                "https://example.com/spacer.gif",
                &resource(1024),
                true,
            );
        }
        assert_eq!(cache.bytes, 1024);
        assert!(
            cache
                .get(Some(&a), "https://example.com/spacer.gif")
                .is_some()
        );
    }

    #[test]
    fn the_cache_is_behind_the_policy_and_not_in_front_of_it() {
        // ADR-0018's third term, as a property of the pair rather than of the
        // call order in `fetch_all`. A `file:` resource one document read must
        // never reach a second, and the two rules that stop it are independent:
        // the policy refuses the request outright, *and* the key does not match
        // because the sites differ.
        //
        // The second document differs in **scheme** as well as origin on
        // purpose. `Origin::is_same_site` compares host only, so an origin-only
        // test would pass while a `file:` leak went straight through.
        let mut cache = Cache::default();
        let local = Origin {
            scheme: net::Scheme::File,
            host: String::new(),
            port: 0,
        };
        let url = "file:///home/reader/notes.txt";
        cache.put(Some(&local), url, &resource(32), true);

        assert!(
            cache.get(Some(&site("example.com")), url).is_none(),
            "a web page was served bytes a local document read",
        );
        let policy = net::Policy::default();
        let (origin, _) = net::parse_url(url).expect("a file url");
        assert!(
            matches!(
                policy.check(
                    Some(&site("example.com")),
                    &origin,
                    RequestKind::Subresource
                ),
                Err(net::Refusal::LocalFile),
            ),
            "and the policy refuses it before the cache is even asked",
        );
    }

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
            1.0,
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
            1.0,
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
