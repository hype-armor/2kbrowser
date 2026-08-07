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
//! That makes the Windows confinement stronger than the Linux one rather than
//! equivalent to it. Linux gets a denylist, so a syscall nobody named is
//! allowed; an AppContainer with no capabilities refuses by default and grants
//! only what is named, and nothing is named.
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
        let profile = AppContainerProfile::ensure(
            PROFILE_NAME,
            "2kbrowser renderer",
            Some("Parses and lays out web pages. No network, no filesystem."),
        )
        .map_err(|error| format!("could not create the AppContainer profile: {error}"))?;

        // Without this the container cannot read the binary it is meant to run,
        // and `CreateProcess` fails with access denied — which is the single
        // most likely way this whole path breaks on a machine we have not seen.
        rappct::acl::grant_to_package(
            ResourcePath::File(program.to_path_buf()),
            &profile.sid,
            AccessMask(READ_AND_EXECUTE),
        )
        .map_err(|error| {
            format!(
                "could not grant the renderer container access to {}: {error}",
                program.display()
            )
        })?;

        let capabilities = SecurityCapabilitiesBuilder::new(&profile.sid)
            .build()
            .map_err(|error| format!("could not build the container's capabilities: {error}"))?;
        debug_assert!(
            capabilities.caps.is_empty(),
            "the renderer container must have no capabilities"
        );

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

        let mut io = launch_in_container_with_io(&self.capabilities, &options)
            .map_err(|error| format!("could not start a contained renderer: {error}"))?;

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
pub fn capture(container: &Container, arguments: &str, timeout: std::time::Duration) -> String {
    let mut child = match container.spawn(arguments) {
        Ok(child) => child,
        Err(error) => return format!("spawn-failed={error}"),
    };
    let mut output = String::new();
    if let Some(stdout) = child.stdout()
        && let Err(error) = stdout.read_to_string(&mut output)
    {
        output.push_str(&format!("\nread-failed={error}"));
    }
    // The read above ends at end of pipe, which is the child exiting, so this
    // only ever waits on a child that has already closed its stdout.
    let _ = child.io.wait(Some(timeout));
    output
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
}
