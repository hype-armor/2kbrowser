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
//! # Linux only, for now
//!
//! Landlock would add filesystem confinement on top and is not usable here —
//! `landlock_create_ruleset` returns `ENOSYS` on this kernel — so filesystem
//! access is denied at the syscall level instead, which covers opening but not
//! every path to a file descriptor.
//!
//! macOS and Windows have equivalents (the App Sandbox, an AppContainer with a
//! restricted token) and neither is implemented. They are *not* stubbed out to
//! look done: [`Confinement::Unavailable`] is what the child reports there, and
//! the README says which platforms are actually confined. A sandbox that claims
//! to work and does not is worse than one that says it is missing.

/// What confinement was actually applied.
///
/// Returned rather than assumed so that "it worked" is something the caller can
/// check and say, rather than something the docs assert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confinement {
    /// Syscall filtering is in force.
    Seccomp,
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
        matches!(self, Confinement::Seccomp)
    }

    /// A phrase for a log line or a status message.
    pub fn describe(self) -> &'static str {
        match self {
            Confinement::Seccomp => "confined: no sockets, no file opens, no new processes",
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
pub const fn available() -> bool {
    cfg!(target_os = "linux")
}

/// Argument that makes the binary check its own confinement and report.
///
/// The only honest way to test a sandbox is from inside it: a filter that
/// installs successfully and blocks nothing would pass every test written from
/// the outside.
pub const SELFTEST_ARGUMENT: &str = "--confine-selftest";

/// Applies confinement and then tries the things it is supposed to prevent.
///
/// Prints one line per attempt. Uses `std` rather than raw syscalls on purpose:
/// `TcpStream::connect` and `File::open` are what a compromised renderer would
/// reach for, and they go through the same syscalls the filter names.
pub fn selftest() -> String {
    // The probe file is created *before* confinement, and by this process, so
    // that afterwards it certainly exists and certainly is readable. Anything
    // else and a refusal is indistinguishable from a wrong path.
    //
    // This was got wrong first time round: the probe was `/etc/hostname`, which
    // does not exist on Windows, so the check reported `NotFound` there whether
    // or not a sandbox was blocking it — a test that could not fail, on the one
    // platform where the sandbox is not written yet. Found by someone running
    // it on Windows.
    let probe = std::env::temp_dir().join("2kbrowser-confine-probe");
    let prepared = std::fs::write(&probe, b"probe");

    let confinement = apply();
    let mut lines = vec![format!("confinement={confinement:?}")];

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_matches_the_platform_that_has_an_implementation() {
        // The parent reports this without asking a child, so it has to agree
        // with what `apply` would actually do.
        assert_eq!(available(), cfg!(target_os = "linux"));
        if available() {
            assert_ne!(apply(), Confinement::Unavailable);
        } else {
            assert_eq!(apply(), Confinement::Unavailable);
        }
    }

    #[test]
    fn the_description_says_plainly_whether_it_is_confined() {
        // The word "NOT" is load-bearing: this string ends up where someone
        // decides whether to trust the browser with a strange page.
        assert!(Confinement::Seccomp.is_confined());
        assert!(!Confinement::Unavailable.is_confined());
        assert!(!Confinement::Failed.is_confined());
        assert!(Confinement::Unavailable.describe().contains("NOT"));
        assert!(Confinement::Failed.describe().contains("NOT"));
        assert!(!Confinement::Seccomp.describe().contains("NOT"));
    }
}
