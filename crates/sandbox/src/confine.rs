//! Taking away what the renderer does not need.
//!
//! ADR-0012 gives the child no OS access "of its own". Until this file existed
//! that was an intention rather than a mechanism: the child simply did not
//! *choose* to open a socket, and a compromised one would have chosen
//! differently. This makes it unable to.
//!
//! # What is taken away
//!
//! The renderer's needs are unusually small, which is what makes this
//! practical. It reads one pipe, writes another, allocates memory, and computes.
//! It does not open files — the fonts are compiled into the binary (ADR-0010) —
//! and it does not touch the network, because every subresource is a request
//! the parent answers.
//!
//! So: no sockets, no opening files, no starting processes, no attaching to
//! them.
//!
//! # A denylist, and why
//!
//! An allowlist is stronger: anything not named is refused, so a syscall nobody
//! thought about is refused too. It is also the one that breaks the browser in
//! the field, because the set a renderer touches is decided by the allocator,
//! the shaper, and the standard library, and it changes underneath you on a
//! toolchain bump.
//!
//! This denies the families that matter and returns `EPERM` rather than killing
//! the process. Two consequences, both deliberate: a syscall nobody listed is
//! *allowed*, and a legitimate call that runs into the filter degrades into an
//! error the renderer already knows how to handle instead of a crash a reader
//! sees. An allowlist is the stronger end state and wants a measured set of what
//! the renderer actually uses; this is the version that can ship without
//! guessing.
//!
//! # Where each platform's confinement lives
//!
//! This module is the *child* taking away its own privileges, which is what
//! seccomp is. Windows does not work that way: an AppContainer is built by the
//! parent and handed to `CreateProcess`, and by the time the child is running
//! there is no call it can make to put itself in one. That half lives in
//! [`crate::contain`], and on Windows [`apply`] correctly does nothing.
//!
//! Landlock would add filesystem confinement on top of seccomp and is not
//! usable here — `landlock_create_ruleset` returns `ENOSYS` on this kernel — so
//! filesystem access is denied at the syscall level instead, which covers
//! opening but not every path to a file descriptor.
//!
//! macOS has an equivalent (the App Sandbox) and it is not implemented. It is
//! *not* stubbed out to look done: [`Confinement::Unavailable`] is what is
//! reported there, and the README says which platforms are actually confined. A
//! sandbox that claims to work and does not is worse than one that says it is
//! missing.

/// What confinement was actually applied.
///
/// Returned rather than assumed so that "it worked" is something the caller can
/// check and say, rather than something the docs assert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confinement {
    /// Syscall filtering is in force.
    Seccomp,
    /// The renderer is running in an AppContainer with no capabilities.
    AppContainer,
    /// This platform has no implementation here yet.
    Unavailable,
    /// The platform has one and it could not be installed.
    ///
    /// A kernel too old, or a container that forbids it. Reported rather than
    /// swallowed: a renderer running unconfined is a fact the operator should
    /// be able to find out.
    Failed,
}

impl Confinement {
    /// Whether the renderer is actually confined.
    pub fn is_confined(self) -> bool {
        matches!(self, Confinement::Seccomp | Confinement::AppContainer)
    }

    /// A phrase for a log line or a status message.
    pub fn describe(self) -> &'static str {
        match self {
            Confinement::Seccomp => "confined: no sockets, no file opens, no new processes",
            Confinement::AppContainer => {
                "confined: an AppContainer with no capabilities — no network, no filesystem"
            }
            Confinement::Unavailable => "NOT confined: no sandbox is implemented on this platform",
            Confinement::Failed => "NOT confined: the sandbox could not be installed",
        }
    }
}

/// Drops the privileges the renderer does not need.
///
/// Call once, in the child, *before* reading anything the parent sends — the
/// first frame is already attacker-influenced, since its body is the document.
#[cfg(target_os = "linux")]
pub fn apply() -> Confinement {
    use seccompiler::{
        BpfProgram, SeccompAction, SeccompFilter, SeccompRule, TargetArch, apply_filter,
    };
    use std::collections::BTreeMap;

    // Everything here is a family, not a single call. Denying `socket` while
    // leaving `socketpair` is not a denial.
    let denied: &[i64] = &[
        // Network. The renderer has no business reaching anything; the parent
        // fetches on its behalf.
        libc::SYS_socket,
        libc::SYS_socketpair,
        libc::SYS_connect,
        libc::SYS_bind,
        libc::SYS_listen,
        libc::SYS_accept,
        libc::SYS_accept4,
        libc::SYS_sendto,
        libc::SYS_sendmsg,
        libc::SYS_recvfrom,
        libc::SYS_recvmsg,
        // Opening files. The fonts are in the binary and the pipes are already
        // open, so nothing legitimate opens anything.
        libc::SYS_open,
        libc::SYS_openat,
        libc::SYS_openat2,
        libc::SYS_creat,
        libc::SYS_truncate,
        libc::SYS_unlink,
        libc::SYS_unlinkat,
        libc::SYS_rename,
        libc::SYS_renameat,
        libc::SYS_renameat2,
        libc::SYS_mkdir,
        libc::SYS_mkdirat,
        // Starting or inspecting other processes. A renderer that has been
        // taken over should not be able to run anything.
        libc::SYS_execve,
        libc::SYS_execveat,
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
    ];

    let rules: BTreeMap<i64, Vec<SeccompRule>> =
        denied.iter().map(|nr| (*nr, Vec::new())).collect();

    // Denied calls return EPERM; everything else is allowed. The default has to
    // be `Allow` for a denylist, and that is the trade this file's header
    // states plainly.
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        TargetArch::x86_64,
    );
    let Ok(filter) = filter else {
        return Confinement::Failed;
    };
    let Ok(program) = BpfProgram::try_from(filter) else {
        return Confinement::Failed;
    };
    match apply_filter(&program) {
        Ok(()) => Confinement::Seccomp,
        // A kernel without seccomp, or a container that forbids installing a
        // filter. Reported rather than swallowed.
        Err(_) => Confinement::Failed,
    }
}

/// Drops the privileges the renderer does not need.
///
/// Not implemented on this platform. Deliberately not a silent success — see
/// the note in this module's header about why a sandbox that claims to work is
/// worse than one that says it is missing.
#[cfg(not(target_os = "linux"))]
pub fn apply() -> Confinement {
    Confinement::Unavailable
}

/// Whether this platform has a sandbox implementation at all.
///
/// A compile-time fact, so the *parent* can say "this build cannot confine its
/// renderers" once at startup instead of every child announcing it on spawn.
/// That distinction matters: a warning printed twenty times in one test run is
/// a warning people learn to scroll past.
///
/// True on Linux and Windows by two different mechanisms — see this module's
/// header for why they are not interchangeable.
pub const fn available() -> bool {
    cfg!(any(target_os = "linux", target_os = "windows"))
}

/// Argument that makes the binary check its own confinement and report.
///
/// The only honest way to test a sandbox is from inside it: a filter that
/// installs successfully and blocks nothing would pass every test written from
/// the outside.
pub const SELFTEST_ARGUMENT: &str = "--confine-selftest";

/// Argument that runs only the probes, for a process that is already confined.
///
/// Windows needs this because the confinement is applied from outside: the
/// process running the probes cannot be the process that built the container.
/// Not in the usage text — it is how the self-test talks to itself.
pub const SELFTEST_PROBE_ARGUMENT: &str = "--confine-selftest-probe";

/// Where the file-open probe writes and then tries to read.
///
/// Both halves of the Windows self-test have to agree on this path, and they
/// are different processes, so it is derived rather than passed: `temp_dir`
/// reads `TMP`/`TEMP`, which the child inherits.
fn probe_path() -> std::path::PathBuf {
    std::env::temp_dir().join("2kbrowser-confine-probe")
}

/// Applies confinement and then tries the things it is supposed to prevent.
///
/// Prints one line per attempt. Uses `std` rather than raw syscalls on purpose:
/// `TcpStream::connect` and `File::open` are what a compromised renderer would
/// reach for, and they go through the same syscalls the filter names.
///
/// On Windows this cannot confine the process it is running in, so it builds a
/// container and runs [`probes`] inside one instead.
pub fn selftest() -> String {
    #[cfg(target_os = "windows")]
    {
        windows_selftest()
    }
    #[cfg(not(target_os = "windows"))]
    {
        // The probe file is created *before* confinement, and by this process,
        // so that afterwards it certainly exists and certainly is readable.
        // Anything else and a refusal is indistinguishable from a wrong path.
        //
        // This was got wrong first time round: the probe was `/etc/hostname`,
        // which does not exist on Windows, so the check reported `NotFound`
        // there whether or not a sandbox was blocking it — a test that could not
        // fail, on the one platform where the sandbox was not written yet. Found
        // by someone running it on Windows.
        let prepared = std::fs::write(probe_path(), b"probe");
        let confinement = apply();
        format!("confinement={confinement:?}\n{}", probes(prepared))
    }
}

/// Builds a container, runs the probes inside it, and reports both halves.
///
/// The probe file is written out here, by the unconfined parent, so that a
/// refusal on the far side is the container refusing rather than a missing
/// file — the same mistake this self-test already made once.
#[cfg(target_os = "windows")]
fn windows_selftest() -> String {
    let prepared = std::fs::write(probe_path(), b"probe");
    if let Err(error) = &prepared {
        return format!(
            "confinement=Unknown\nPROBE-UNWRITABLE({:?})\nprobe={}",
            error.kind(),
            probe_path().display()
        );
    }

    let program = match std::env::current_exe() {
        Ok(program) => program,
        Err(error) => {
            return format!("confinement=Failed\nreason=cannot find this executable: {error}");
        }
    };
    let container = match crate::contain::Container::new(&program) {
        Ok(container) => container,
        Err(error) => return format!("confinement=Failed\nreason={error}"),
    };
    let inside = crate::contain::capture(
        &container,
        SELFTEST_PROBE_ARGUMENT,
        std::time::Duration::from_secs(30),
    );
    format!("confinement=AppContainer\n{}", inside.trim_end())
}

/// Tries the things confinement is supposed to prevent, and says what happened.
///
/// Split out from [`selftest`] because on Windows the process that confines and
/// the process that is confined are not the same one.
pub fn probes(prepared: std::io::Result<()>) -> String {
    let probe = probe_path();
    let mut lines = Vec::new();

    // A socket to a port nothing listens on. `ConnectionRefused` means the
    // syscall went through and the far end said no — the network was reachable.
    // A confined process never gets that far.
    let socket = std::net::TcpStream::connect("127.0.0.1:9");
    lines.push(format!(
        "socket={}",
        match socket {
            Ok(_) => "OPENED".to_owned(),
            Err(error) => format!("{:?}", error.kind()),
        }
    ));

    lines.push(format!(
        "file={}",
        match (&prepared, std::fs::File::open(&probe)) {
            // Nothing to conclude from a probe that was never written. Said
            // outright rather than reported as a failure to open.
            (Err(error), _) => format!("PROBE-UNWRITABLE({:?})", error.kind()),
            (Ok(()), Ok(_)) => "OPENED".to_owned(),
            (Ok(()), Err(error)) => format!("{:?}", error.kind()),
        }
    ));
    lines.push(format!("probe={}", probe.display()));

    // Something harmless, to prove the filter did not simply break everything —
    // two `PermissionDenied` lines are also what a filter that killed the whole
    // process would produce if it somehow got this far.
    lines.push(format!("compute={}", (1..=10).sum::<u32>()));

    lines.join("\n")
}

/// Runs the probes in a process something else has already confined.
///
/// The file it reports on was written by the parent, so `Ok(())` is passed in:
/// this process is not supposed to be able to write anything.
pub fn selftest_probe() -> String {
    probes(Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_matches_the_platforms_that_have_an_implementation() {
        assert_eq!(
            available(),
            cfg!(any(target_os = "linux", target_os = "windows"))
        );
    }

    #[test]
    fn self_confinement_is_only_claimed_where_it_is_the_mechanism() {
        // `apply` is the child restricting itself, which is Linux only. On
        // Windows the answer here is `Unavailable` *and the platform still has*
        // *a sandbox* — the parent builds it. Asserted because the obvious
        // shortcut, `available() implies apply() != Unavailable`, was true until
        // Windows landed and is now wrong.
        if cfg!(target_os = "linux") {
            assert_ne!(apply(), Confinement::Unavailable);
        } else {
            assert_eq!(apply(), Confinement::Unavailable);
        }
    }

    #[test]
    fn the_description_says_plainly_whether_it_is_confined() {
        // The word "NOT" is load-bearing: this string ends up where someone
        // decides whether to trust the browser with a strange page.
        for confined in [Confinement::Seccomp, Confinement::AppContainer] {
            assert!(confined.is_confined());
            assert!(!confined.describe().contains("NOT"));
        }
        for loose in [Confinement::Unavailable, Confinement::Failed] {
            assert!(!loose.is_confined());
            assert!(loose.describe().contains("NOT"));
        }
    }
}
