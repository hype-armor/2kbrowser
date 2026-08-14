//! Confining a renderer from the outside, on Windows.
//!
//! [`crate::confine`] is the child taking away its own privileges after `exec`.
//! That is what seccomp is, and it is not what Windows has. An AppContainer is
//! built by the *parent*: a package SID, a set of capabilities, and a
//! `SECURITY_CAPABILITIES` structure handed to `CreateProcess` through
//! `STARTUPINFOEX`. By the time the child is running it is too late — there is
//! no call it can make to put itself in one.
//!
//! So the confinement lives here, next to the spawn, and the child does nothing
//! at all on Windows.
//!
//! # No capabilities
//!
//! An AppContainer's capabilities are the holes deliberately left in it:
//! `internetClient` lets it open outbound sockets, `picturesLibrary` lets it
//! read that folder, and so on. This one is built with **none**, which is the
//! whole point — the renderer's needs are memory and CPU, and every resource it
//! wants is a request the parent answers over the pipe.
//!
//! Both platforms refuse by default now — the Linux filter became an allowlist
//! in [ADR-0016](../../../docs/adr/0016-syscall-allowlist-measured.md), and
//! before that this paragraph said the Windows side was categorically stronger
//! for exactly that reason. What is left is a difference in kind rather than in
//! strength. seccomp filters *calls* and is installed by the process being
//! confined; an AppContainer restricts access to *resources* and is built by the
//! parent, so nothing the child does can undo it. Neither subsumes the other.
//!
//! # What it still needs
//!
//! Two things, both unavoidable:
//!
//! * **The executable.** An AppContainer access check requires a grant to the
//!   package SID *in addition to* the ordinary user grant, so a binary sitting
//!   in someone's home directory is unreadable to it until told otherwise. The
//!   grant is made once, to this package alone, for read and execute — not to
//!   `ALL APPLICATION PACKAGES`, which would open the file to every sandboxed
//!   program on the machine.
//! * **The pipes.** Handles are inherited, and an inherited handle carries the
//!   access it was granted when it was duplicated, so the container's SID never
//!   enters into it. They are passed through `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`
//!   so that *only* those three cross the boundary.
//!
//! # The profile is persistent
//!
//! `CreateAppContainerProfile` writes a registry entry and a folder under
//! `%LOCALAPPDATA%\Packages`. One entry, created the first time the browser
//! runs, reused after that. It is deliberately not deleted when the browser
//! exits: creating and destroying it per launch would be registry churn for no
//! benefit, and the folder is where the container would put anything it managed
//! to write, which is worth being able to look at afterwards.

use std::io::Read;
use std::path::{Path, PathBuf};

use rappct::acl::{AccessMask, ResourcePath};
use rappct::launch::{
    JobLimits, LaunchOptions, LaunchedIo, StdioConfig, launch_in_container_with_io,
};
use rappct::{AppContainerProfile, SecurityCapabilities, SecurityCapabilitiesBuilder};

/// The AppContainer profile the renderers run in.
///
/// Stable across runs, and namespaced, because it becomes a registry entry and
/// a folder on the user's machine.
pub const PROFILE_NAME: &str = "2kbrowser.renderer";

/// `FILE_GENERIC_READ | FILE_GENERIC_EXECUTE`, from `winnt.h`.
///
/// Spelled out rather than reached for through the `windows` crate: this is the
/// one number in this file that decides how much of the disk the container can
/// see, and it should be readable without following a re-export chain. Read and
/// execute, no write — the renderer has no business modifying the binary it is
/// running.
const READ_AND_EXECUTE: u32 = 0x0012_00A9;

/// An error and everything under it, joined into one sentence.
///
/// `rappct`'s `Display` names the stage and a hint and stops there, so the
/// actual Win32 error — the part that says *why* — is only reachable through
/// `source`. A CI failure that says "Process launch failed at CreateProcessW"
/// and nothing else costs a round trip to diagnose, and that round trip is ten
/// minutes each time.
fn chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut next = error.source();
    while let Some(cause) = next {
        text.push_str(&format!(": {cause}"));
        next = cause.source();
    }
    text
}

/// The profile and capability set, prepared once for the whole process.
///
/// `CreateAppContainerProfile` writes to the registry and the ACL grant below
/// rewrites the executable's DACL. Doing either more than once is waste; doing
/// them *concurrently* is what broke.
///
/// What was observed: two tests out of seventeen failing on Windows CI inside
/// `CreateProcessW`, while the same code had been green on the run before and
/// fifteen other tests passed in the same run. Both failures were in tests that
/// build several renderers, and the suite runs them on several threads — so
/// several threads were rewriting the same executable's security descriptor
/// while other threads were launching processes from that same executable.
///
/// The exact mechanism is not confirmed and is not claimed here; a security
/// descriptor being replaced while `CreateProcess` opens the image is the
/// obvious candidate. What is certain is that the rewrite never needed to
/// happen more than once, and the comments in this file already said it did
/// not. They were wrong, and nothing made that visible until the machine
/// disagreed.
static PREPARED: std::sync::OnceLock<Result<SecurityCapabilities, String>> =
    std::sync::OnceLock::new();

/// Executables already granted access, so the DACL is rewritten once each.
static GRANTED: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// The variables the renderer is given, where the parent has them.
///
/// `SystemRoot` and `windir` are how the loader finds the system DLLs every
/// Windows process links; the processor variables are read by the C runtime
/// during startup.
///
/// Named at module scope rather than inside [`environment`] because a launch
/// that fails needs to say which of them the machine did not have — see
/// [`launch_inputs`]. Absence is silent otherwise: the block is built with
/// `filter_map`, so a variable the parent lacks is simply not passed on, and
/// that is exactly the shape of the bug this list has already had twice.
#[cfg(target_os = "windows")]
const WANTED: &[&str] = &[
    "SystemRoot",
    "windir",
    "SystemDrive",
    "ComSpec",
    "PATHEXT",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "PROCESSOR_IDENTIFIER",
    "PROCESSOR_LEVEL",
    "PROCESSOR_REVISION",
    // Not needed by the renderer, which opens no files, but an AppContainer
    // redirects these into its own profile anyway, so they leak nothing and
    // their absence surprises anything that assumes them.
    "TEMP",
    "TMP",
    // The same argument one step back, and the actual cause of the launch
    // failure this list spent three attempts on. Redirecting `TEMP` into the
    // package profile means *computing* where that profile is, and Windows does
    // that from these. An explicit block that omits them is precisely what "the
    // system could not find the environment option that was entered" is
    // complaining about — the error names the problem, in its way, and reads as
    // nonsense until you know which option it means.
    //
    // They name the user's own directories, which the first version of this
    // list avoided on purpose. Kept because the alternative is no AppContainer
    // at all, and safe because the container cannot open any of them: a path it
    // may not follow is a string.
    "USERPROFILE",
    "LOCALAPPDATA",
    "APPDATA",
    // Only when the operator asked for it.
    "RUST_BACKTRACE",
];

/// The environment the renderer is given.
///
/// Named, rather than inherited. Two reasons, and both are the point.
///
/// **It is what the parent knows.** A browser's environment routinely holds
/// API tokens, proxy credentials, and the shape of someone's home directory,
/// and handing all of it to the process that parses hostile documents is
/// exactly backwards. The renderer reads a pipe, allocates, and computes; it
/// has no use for any of it.
///
/// **Inheriting does not work everywhere.** Passing no environment block means
/// `CreateProcessW` uses the caller's, and on some machines that fails when the
/// child is going into an AppContainer — reported as
/// `ERROR_ENVVAR_NOT_FOUND (203)`, "the system could not find the environment
/// option that was entered", which is an unusually unhelpful way to say it.
/// `rappct` documents an explicit block as the remedy.
///
/// The first version of this said the bug "was not reproducible on CI and was
/// on a real machine, which is the ordinary shape of an environment-dependent
/// bug". That was wrong, and wrong in the way that costs the most: it *was*
/// happening on CI, on every push, and the test that should have said so was
/// skipping silently because no sandbox had been installed. The explicit block
/// did not fix the error — it was the error, and nothing was watching.
///
/// What was missing was the three variables Windows needs to work out where an
/// AppContainer's package profile lives — `USERPROFILE`, `LOCALAPPDATA`, and
/// `APPDATA`. It redirects `TEMP` into that profile as the process starts, and
/// cannot when the block it was handed does not say where the user's is. The
/// error is not as unhelpful as it looks once you know which option it means.
///
/// Two other things were wrong with the block, found first because `rappct`
/// names them both, and fixed here despite being innocent of this bug:
///
/// **`PATH`.** Its `merge_parent_env` exists for exactly this, and its comment
/// says so — these are the keys "whose absence causes common failures (e.g.,
/// error 203)". Called here rather than copied, so that anything upstream
/// learns about this arrives with a version bump.
///
/// **Sorted order.** Win32 wants a Unicode environment block sorted
/// case-insensitively by name. `rappct`'s block builder does not sort what a
/// caller hands it, and its own test path sorts before calling — which is the
/// tell. An unsorted block is the kind of input that works until something
/// downstream does a binary search over it.
///
/// So: the handful Windows itself wants to start a process, plus what upstream
/// adds, in the order Win32 asks for. `RUST_BACKTRACE` is passed through when
/// it is set, because a renderer that panics is the case where its output
/// matters most.
///
/// # It is still an allowlist, and that is the risk that is left
///
/// The same error has since been reported again, from a machine none of the
/// above reached: a release build, on Windows, falling back to an unconfined
/// renderer at `CreateProcessW`. Nothing was changed here in response, because
/// nothing about it could be established without the machine — CI has launched
/// the identical container on every push since, and every input to that launch
/// that does *not* vary by machine is therefore already ruled out. What is
/// left is this list and the package directory, and the way to tell them apart
/// is to look, which is what [`launch_inputs`] now makes the failure say.
///
/// The shape of the risk is worth naming rather than discovering a fourth
/// time. [`WANTED`] is a hand-picked allowlist, so every machine that needs a
/// variable nobody thought of gets a renderer with no sandbox, and the only
/// signal is a line on stderr. `rappct`'s own remedy for error 203 is the
/// opposite trade — `RAPPCT_TEST_FORCE_ENV` passes the *whole* parent
/// environment — which would end this class of failure and hand the process
/// that parses hostile documents the tokens and credentials a browser's
/// environment holds. That is not obviously the wrong trade and it is not one
/// to make from a guess about which variable was missing; it needs the report
/// from a machine that fails, which is now a report worth having.
#[cfg(target_os = "windows")]
fn environment() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let wanted = WANTED
        .iter()
        .filter_map(|name| {
            std::env::var_os(name).map(|value| (std::ffi::OsString::from(*name), value))
        })
        .collect();

    // Adds `PATH` and anything else upstream considers essential, without
    // duplicating what is already here. It does bring in a variable naming
    // directories belonging to the user, which the earlier version of this
    // avoided on purpose — a fair trade only because the container cannot open
    // any of them, and not one to make silently.
    let mut environment = rappct::launch::merge_parent_env(wanted);
    environment.sort_by_key(|(name, _)| name.to_string_lossy().to_lowercase());
    environment
}

/// What this machine handed the launch, for a failure only it can see.
///
/// `CreateProcessW` reports a container it would not build as a three-digit
/// code, and the code is not about the thing that is wrong: this file's history
/// is two rounds of guessing at `ERROR_ENVVAR_NOT_FOUND (203)` and one round of
/// printing what the launch had been handed, which is the one that ended it.
/// The same error has now been reported from a machine none of the fixes
/// reached, and the number on its own says no more this time than it did then.
///
/// So this says what went in, split into the two kinds, because the split is
/// the whole diagnostic value:
///
/// * **The same on every machine.** The capability count and the LPAC flag,
///   which together fix the shape of the attribute list — one entry for the
///   security capabilities, one for the inherited pipes, and, since LPAC is
///   never asked for here, no third. CI launches that identical structure on
///   every push and it succeeds, so a failure reporting `capabilities=0
///   lpac=false` is a failure that cannot be about the attribute list, however
///   much "extended startup with AC/LPAC" in the error sounds like it is. That
///   phrase is a fixed hint in `rappct`, printed whether or not LPAC was used.
///   Named here to be ruled out, not because they are suspects.
/// * **Only this machine's.** The package SID, whether the package directory
///   exists, and which of [`WANTED`] the parent did not have. These are the
///   only inputs that differ between a green CI run and a user's desktop, so a
///   launch that fails on one and not on the other is about one of them — and
///   the first two tell a profile that was never really created from an
///   environment block that is missing something, which is otherwise the same
///   error twice.
///
/// Names, never values. Not inheriting the environment is the whole point of
/// [`environment`] — a browser's holds tokens and someone's directory layout —
/// and a diagnostic that printed them into a bug report would hand them to a
/// wider audience than the renderer was refused.
#[cfg(target_os = "windows")]
fn launch_inputs(capabilities: &SecurityCapabilities) -> String {
    let absent: Vec<&str> = WANTED
        .iter()
        .copied()
        .filter(|name| std::env::var_os(name).is_none())
        .collect();
    let absent = if absent.is_empty() {
        "none".to_owned()
    } else {
        absent.join(",")
    };

    // `CreateAppContainerProfile` makes this directory, named for the profile,
    // and `rappct` falls back to *deriving* the SID when the call fails with
    // `ERROR_INVALID_PARAMETER` — so a container can be built, and this
    // directory still not exist, and nothing before the launch would say so.
    // Windows redirects the child's `TEMP` into it as the process starts.
    let package_directory = match std::env::var_os("LOCALAPPDATA") {
        Some(local) => {
            if Path::new(&local)
                .join("Packages")
                .join(PROFILE_NAME)
                .is_dir()
            {
                "present"
            } else {
                "absent"
            }
        }
        // Which is itself the finding, and one this cannot be reached with:
        // `LOCALAPPDATA` is in `WANTED`, so it would be named above too.
        None => "unknown, LOCALAPPDATA is not set",
    };

    format!(
        "launch inputs: capabilities={} lpac={} package-sid={} \
         package-directory={package_directory} \
         environment-variables={} wanted-but-absent={absent}",
        capabilities.caps.len(),
        capabilities.lpac,
        capabilities.package.as_string(),
        environment().len(),
    )
}

/// A built AppContainer, ready to spawn renderers into.
///
/// Built once per browser process rather than once per page: creating the
/// profile and granting the executable are idempotent, and doing them for every
/// tab would be a registry write on every navigation.
pub struct Container {
    capabilities: SecurityCapabilities,
    program: PathBuf,
}

impl Container {
    /// Creates the profile, grants the executable, and prepares the capability set.
    ///
    /// The error is a sentence rather than a type because there is exactly one
    /// thing the caller does with it: say why the renderer is not confined.
    pub fn new(program: &Path) -> Result<Self, String> {
        let capabilities = PREPARED
            .get_or_init(|| {
                let profile = AppContainerProfile::ensure(
                    PROFILE_NAME,
                    "2kbrowser renderer",
                    Some("Parses and lays out web pages. No network, no filesystem."),
                )
                .map_err(|error| {
                    format!(
                        "could not create the AppContainer profile: {}",
                        chain(&error)
                    )
                })?;

                let capabilities = SecurityCapabilitiesBuilder::new(&profile.sid)
                    .build()
                    .map_err(|error| {
                        format!(
                            "could not build the container's capabilities: {}",
                            chain(&error)
                        )
                    })?;
                debug_assert!(
                    capabilities.caps.is_empty(),
                    "the renderer container must have no capabilities"
                );
                Ok(capabilities)
            })
            .clone()?;

        // Without this the container cannot read the binary it is meant to run,
        // and `CreateProcess` fails with access denied — which is the single
        // most likely way this whole path breaks on a machine we have not seen.
        //
        // Under the lock, and once per executable: see `PREPARED` for what
        // happens when two threads rewrite the same DACL at the same time.
        let mut granted = GRANTED.lock().unwrap_or_else(|error| error.into_inner());
        if !granted.iter().any(|done| done == program) {
            rappct::acl::grant_to_package(
                ResourcePath::File(program.to_path_buf()),
                &capabilities.package,
                AccessMask(READ_AND_EXECUTE),
            )
            .map_err(|error| {
                format!(
                    "could not grant the renderer container access to {}: {}",
                    program.display(),
                    chain(&error)
                )
            })?;
            granted.push(program.to_path_buf());
        }
        drop(granted);

        Ok(Self {
            capabilities,
            program: program.to_path_buf(),
        })
    }

    /// Starts a renderer inside the container.
    pub fn spawn(&self, arguments: &str) -> Result<Contained, String> {
        // `CreateProcess` takes the command line as one string and the child
        // parses it back into arguments, so argv[0] has to be here or the
        // renderer would read its own path as the first argument.
        let command = format!("\"{}\" {arguments}", self.program.display());

        let options = LaunchOptions {
            exe: self.program.clone(),
            cmdline: Some(command),
            stdio: StdioConfig::Pipe,
            env: Some(environment()),
            join_job: Some(JobLimits {
                // The renderer dies when this handle closes, which happens when
                // the session is dropped *and* if the browser is killed outright.
                // `kill()` on drop covers the first; only the job covers the
                // second, and an orphaned renderer holding a page is exactly the
                // thing process isolation is supposed to prevent.
                kill_on_job_close: true,
                // Deliberately unset. A memory cap is a real defence, but the
                // right number is the largest legitimate page rather than a
                // round figure, and guessing it here would break rendering to
                // look secure.
                memory_bytes: None,
                cpu_rate_percent: None,
            }),
            ..Default::default()
        };

        // The inputs go out with the failure rather than behind a flag. A
        // launch that fails here is a security control degrading on a machine
        // nobody here can see, and the report of it is the whole evidence there
        // will ever be — asking the person who hit it to rebuild with a
        // diagnostic turned on costs a round trip that this file has already
        // spent three times.
        let mut io =
            launch_in_container_with_io(&self.capabilities, &options).map_err(|error| {
                format!(
                    "could not start a contained renderer: {}; {}",
                    chain(&error),
                    launch_inputs(&self.capabilities)
                )
            })?;

        // The child's stderr is a pipe rather than the parent's console, because
        // an AppContainer cannot be handed an arbitrary inherited console
        // handle. Relayed on a thread instead of dropped: a panic message from
        // the renderer is the most useful thing it produces when something is
        // wrong, and swallowing it would make every renderer bug invisible.
        //
        // A pipe nobody drains fills and blocks the writer, so this thread is
        // required for correctness and not only for the diagnostics. It ends
        // when the child does and the pipe closes.
        if let Some(mut stderr) = io.stderr.take() {
            let _ = std::thread::Builder::new()
                .name("renderer-stderr".to_owned())
                .spawn(move || {
                    let mut out = std::io::stderr();
                    let _ = std::io::copy(&mut stderr, &mut out);
                });
        }

        Ok(Contained { io })
    }
}

/// A renderer running inside an AppContainer.
///
/// Dropping it closes the job handle, and the job was created with
/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so the child dies with it.
pub struct Contained {
    io: LaunchedIo,
}

impl Contained {
    /// The pipe the parent writes requests to.
    pub fn stdin(&mut self) -> Option<&mut std::fs::File> {
        self.io.stdin.as_mut()
    }

    /// The pipe the parent reads answers from.
    pub fn stdout(&mut self) -> Option<&mut std::fs::File> {
        self.io.stdout.as_mut()
    }

    /// The child's process id.
    pub fn id(&self) -> u32 {
        self.io.pid
    }
}

/// Runs a program inside a container and returns everything it printed.
///
/// For [`crate::confine::selftest`], which has to run its probes on the far
/// side of the boundary — a container that installs successfully and confines
/// nothing would pass every check written from the outside.
/// The read happens on a thread so that `timeout` bounds it. Reading a pipe
/// blocks, and there is no portable way to interrupt that — a child that starts
/// and then wedges would otherwise hang whoever called this, which for the
/// self-test means hanging CI until the job's own timeout hours later. Giving
/// up leaves the thread parked on a read it will never finish, which is fine:
/// dropping `child` closes the job handle, the kernel kills the process, the
/// pipe closes, and the thread ends.
pub fn capture(
    container: &Container,
    arguments: &str,
    timeout: std::time::Duration,
) -> Result<String, String> {
    // A failure here is *not* a report about a confined process, and returning
    // one as if it were is how the self-test came to print
    // `confinement=AppContainer` above a line saying the container never
    // launched anything. The whole point of this file is that a sandbox which
    // claims to work and does not is worse than one that says it is missing.
    let mut child = container.spawn(arguments)?;
    let Some(stdout) = child.io.stdout.take() else {
        return Err("the contained process had no stdout".to_owned());
    };

    let (finished, output) = std::sync::mpsc::channel();
    if std::thread::Builder::new()
        .name("contained-selftest".to_owned())
        .spawn(move || {
            let mut stdout = stdout;
            let mut text = String::new();
            let read = stdout.read_to_string(&mut text);
            let _ = finished.send(match read {
                Ok(_) => text,
                Err(error) => format!("{text}\nread-failed={error}"),
            });
        })
        .is_err()
    {
        return Err("could not start a thread to read the contained process".to_owned());
    }

    output.recv_timeout(timeout).map_err(|_| {
        format!(
            "the contained process said nothing for {}s",
            timeout.as_secs()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_renderer_container_is_granted_nothing() {
        // The whole security argument rests on this being empty. Capabilities
        // are the holes deliberately left in an AppContainer, and one added by
        // accident — a `with_known` call that looked harmless, a builder default
        // that changed on a version bump — would not show up anywhere else.
        //
        // Derived rather than created: this needs no registry entry and no ACL
        // rewrite, so running the test leaves nothing behind.
        let sid = rappct::derive_sid_from_name(PROFILE_NAME).expect("derives the package SID");
        let capabilities = SecurityCapabilitiesBuilder::new(&sid)
            .build()
            .expect("builds the capability set");
        assert!(
            capabilities.caps.is_empty(),
            "the renderer container was granted {:?}",
            capabilities.caps
        );
        assert!(
            !capabilities.lpac,
            "LPAC changes which capabilities are needed, so it is not a free upgrade"
        );
    }

    #[test]
    fn the_three_variables_that_locate_the_package_profile_are_still_asked_for() {
        // These three cost a user's bug report, two guesses, and a CI run that
        // had been failing invisibly for days. Windows redirects an
        // AppContainer's `TEMP` into its package profile as the process starts
        // and works out where that profile is from them, so an explicit
        // environment block without them is a launch that fails with a message
        // about an environment option nobody entered.
        //
        // Named individually here because the reason they are in `WANTED` is
        // not visible from the renderer's own needs — it opens no files and
        // wants none of them — so they read like the sort of thing a later
        // tidy-up removes for privacy, which is exactly the argument that put
        // the bug there the first time.
        for locates_the_profile in ["USERPROFILE", "LOCALAPPDATA", "APPDATA"] {
            assert!(
                WANTED.contains(&locates_the_profile),
                "{locates_the_profile} is how Windows finds the package profile",
            );
        }
    }

    #[test]
    fn a_failed_launch_reports_what_only_this_machine_could_have_contributed() {
        // A diagnostic that prints nothing useful is the same shape of mistake
        // as a check that cannot fail, and this file has produced three of
        // those. So: the report must name the inputs that vary by machine —
        // they are the only ones a green CI run does not already rule out — and
        // must not print any variable's value.
        let sid = rappct::derive_sid_from_name(PROFILE_NAME).expect("derives the package SID");
        let capabilities = SecurityCapabilitiesBuilder::new(&sid)
            .build()
            .expect("builds the capability set");
        let report = launch_inputs(&capabilities);

        for named in [
            "capabilities=0",
            "lpac=false",
            "package-sid=",
            "package-directory=",
            "wanted-but-absent=",
        ] {
            assert!(report.contains(named), "no `{named}` in:\n{report}");
        }

        // Names, never values. `SystemRoot` is set on every Windows machine
        // there is, so its value is the one that can be checked for without
        // arranging anything — and if it leaked, so would `USERPROFILE`, which
        // is somebody's name.
        let system_root = std::env::var("SystemRoot").expect("Windows sets this");
        assert!(
            !report.contains(&system_root),
            "the report printed a variable's value:\n{report}"
        );
    }
}
