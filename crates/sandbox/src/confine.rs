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
//! them — and, because this is an allowlist, no anything else either.
//!
//! # An allowlist, and how its contents were arrived at
//!
//! This was a denylist first, and said so: an allowlist is stronger, because a
//! syscall nobody thought of is refused rather than allowed, but it is also the
//! one that breaks a browser in the field. The set a renderer touches is decided
//! by the allocator, the shaper, and the standard library, and guessing at it
//! from the outside is how you ship a filter that kills the renderer on a page
//! nobody tested.
//!
//! So it was not guessed at. Every call named in [`allowed`] was either
//! *observed* — `strace` on real renderer children, across every reference
//! fixture, the fuzzer's corpus, band and find requests, a re-render at a new
//! width, and subresources arriving over the pipe — or is in a short, named
//! margin of calls whose absence is unfixable rather than degrading.
//! `scripts/renderer-syscalls.sh` is that measurement, so the list can be
//! rechecked after a toolchain bump instead of trusted.
//!
//! The measurement's surprise was how small the set is: rendering a page uses
//! nine calls. That is what makes an allowlist practical here and would not in a
//! browser that opened its own fonts, resolved its own hostnames, or ran a
//! thread pool.
//!
//! **Measured on two C libraries and two architectures**, because one
//! measurement of a set decided by the libc is not evidence about the libc. The
//! rendering set is identical under glibc and musl. Failing is not: glibc's
//! `abort` raises its signal with `tgkill` and musl's with `tkill`, and the
//! first version of this list named only the one it had seen. That is what a
//! second measurement is for, and it is the shape of the difference to expect —
//! the *failure* paths vary where the working path does not.
//!
//! aarch64 then needed nothing x86_64 did not; its set is a strict subset,
//! short by `futex`, which the x86_64 panic path takes and the aarch64 one does
//! not. CI measures it on every push, so this is a check rather than a claim.
//!
//! What is *not* in the list is the point. No `socket`, no `openat`, no
//! `execve`, no `ptrace`, no `io_uring` — and no `open_by_handle_at`, `fsopen`,
//! or `open_tree`, which are the routes to a file descriptor that a denylist
//! naming `openat` and stopping there leaves open. Under an allowlist those stop
//! being entries anyone has to remember, which is the whole argument for one.
//! [`must_stay_denied`] keeps naming them anyway, as an assertion rather than a
//! filter: it is the test that a future edit widening this list does not quietly
//! let one back in.
//!
//! The default action is still `EPERM` rather than killing the process. A
//! syscall this list forgot degrades into an error — usually a page that fails
//! to render, which the parent already reports — rather than a renderer that
//! dies where a reader sees it. That is a deliberate softening of an allowlist's
//! usual posture, and it is what makes the stronger filter safe to ship on a
//! measurement taken on one machine.
//!
//! Deliberately not done: refusing `PROT_EXEC` on `mmap` and `mprotect`. It is
//! easy from here — seccomp can filter on arguments — and it is worth less than
//! it looks, because code that has got far enough to map a page has already got
//! far enough not to need to.
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
            Confinement::Seccomp => {
                "confined: only the syscalls a renderer was measured to need — no sockets, \
                 no file opens, no new processes"
            }
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
    use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, SeccompRule, apply_filter};
    use std::collections::BTreeMap;

    // An architecture this was not built for gets no filter at all. That is not
    // caution, it is the only safe answer: seccompiler puts an architecture
    // check at the head of the program and the mismatch branch is
    // `SECCOMP_RET_KILL_PROCESS`, so a filter naming the wrong architecture does
    // not fail open or fail closed — it kills the renderer on its next syscall,
    // whatever that is.
    let Some(architecture) = architecture() else {
        return Confinement::Unavailable;
    };

    let rules: BTreeMap<i64, Vec<SeccompRule>> =
        allowed().iter().map(|nr| (*nr, Vec::new())).collect();

    // Everything not named returns EPERM; the named calls are allowed. The
    // second argument is the default and the third is what a *matched* rule
    // does, so an allowlist is this pair the other way round from a denylist —
    // a two-line edit that inverts the filter without changing a syscall name.
    // Which is why the self-test probes a call *nobody* named: refusing
    // `socket` proves the list, and only refusing something absent from it
    // proves the direction.
    //
    // `Errno` rather than `KillProcess` for the default: see this module's
    // header. A call this list forgot should cost a page, not the browser.
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Errno(libc::EPERM as u32),
        SeccompAction::Allow,
        architecture,
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

/// The architecture to build the filter for, or `None` if this is not one.
///
/// Taken from the build target rather than named, which sounds obvious and was
/// got wrong: the filter said `x86_64` unconditionally. On an ARM machine that
/// is not a filter that does the wrong thing, it is a filter that kills the
/// renderer immediately — the architecture check at the head of a seccompiler
/// program returns `SECCOMP_RET_KILL_PROCESS` when it does not match.
#[cfg(target_os = "linux")]
const fn architecture() -> Option<seccompiler::TargetArch> {
    #[cfg(target_arch = "x86_64")]
    {
        Some(seccompiler::TargetArch::x86_64)
    }
    #[cfg(target_arch = "aarch64")]
    {
        Some(seccompiler::TargetArch::aarch64)
    }
    #[cfg(target_arch = "riscv64")]
    {
        Some(seccompiler::TargetArch::riscv64)
    }
    // 32-bit ARM, x86, and anything else seccompiler does not know. Reported as
    // unconfined rather than approximated with a neighbouring architecture,
    // because syscall numbers are per-architecture and a filter built from the
    // wrong table denies whatever happens to share a number.
    #[cfg(not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    )))]
    {
        None
    }
}

/// The only syscalls the renderer is allowed to make.
///
/// Two groups, and the difference between them is the difference between a
/// measurement and a judgement, so they are kept apart rather than merged into
/// one alphabetical list.
///
/// Everything in the first group was *seen*, by `strace` on real renderer
/// children — `scripts/renderer-syscalls.sh` is the measurement and prints this
/// set. Everything in the second was not seen and is here anyway, because
/// denying it is either unfixable or would turn a rare event into a hang.
#[cfg(target_os = "linux")]
fn allowed() -> Vec<i64> {
    vec![
        // ---- Measured: rendering a page ----
        //
        // Nine calls, and they do not vary. The pipes, the allocator, and one
        // seed for the hash tables — that is the whole of what a renderer that
        // parses, lays out, and rasterises asks the kernel for. Fonts are in the
        // binary (ADR-0010) and every subresource is a request the parent
        // answers (ADR-0012), which is why nothing here opens or connects to
        // anything.
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_mmap,
        libc::SYS_munmap,
        libc::SYS_mremap,
        libc::SYS_brk,
        libc::SYS_getrandom,
        libc::SYS_sigaltstack,
        libc::SYS_exit_group,
        // ---- Measured: failing ----
        //
        // The paths a hostile page can drive the renderer down, which no
        // fixture reaches and which were measured separately: a panic, an
        // abort, and a stack overflow from a document nested deeply enough.
        libc::SYS_getpid,
        libc::SYS_gettid,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigreturn,
        libc::SYS_futex,
        // How `abort` raises its own signal — and there are two of them, which
        // is the one thing measuring a second libc found. glibc uses `tgkill`;
        // musl uses `tkill`. Refusing the one your libc uses does not prevent
        // the abort: musl falls through to crashing on purpose, so the renderer
        // dies of `SIGSEGV` rather than `SIGABRT`. Nothing hangs, and the
        // outcome is still wrong — in a codebase that forbids `unsafe`, a panic
        // that reports itself as a segmentation fault is a false alarm about the
        // one thing this project claims not to have.
        //
        // Allowing both costs nothing. `tkill` reaches no further than `tgkill`:
        // either can signal any thread the user could signal anyway, and
        // `tgkill` was already here.
        libc::SYS_tgkill,
        libc::SYS_tkill,
        // ---- Not measured, and here on purpose ----
        //
        // `restart_syscall` is issued by the kernel rather than by the program,
        // to resume a call a signal interrupted. Refusing it turns a signal into
        // a spurious error on a read that was going fine.
        libc::SYS_restart_syscall,
        // Ending a thread, as opposed to the process. Nothing here has a second
        // thread today and one library version could change that.
        libc::SYS_exit,
        // Memory the allocator manages rather than the program: returning pages
        // (`madvise`), and the permissions on its own arenas (`mprotect`).
        // Whether either is called at all depends on the libc, its version, and
        // how much the page allocated, so their absence from one machine's
        // measurement says very little. Neither can reach anything outside this
        // process's own address space.
        libc::SYS_madvise,
        libc::SYS_mprotect,
        // `close` is the one entry seen in a confined child that was not seen
        // in a *render*: the self-test's probes release the descriptors they
        // failed to get. Rendering never closes anything, because it never opens
        // anything. Allowed regardless, since giving up a descriptor cannot gain
        // the caller anything.
        libc::SYS_close,
        // `sched_yield` is what a spin lock does before it gives up and sleeps.
        // `clock_gettime` is normally answered by the vDSO without a syscall at
        // all — but not on every kernel and not in every container, and a
        // browser whose clock reads fail is a baffling thing to debug.
        libc::SYS_sched_yield,
        libc::SYS_clock_gettime,
    ]
}

/// The syscalls that must never appear in [`allowed`].
///
/// This is the old denylist, kept as an assertion rather than a filter. Once
/// the filter refuses everything it does not name, none of these needs naming —
/// which is the argument for an allowlist and also the thing that makes the
/// reasoning behind them easy to lose. Held here so that widening [`allowed`]
/// has to get past a test that says why each of them was refused.
///
/// Everything here is a family, not a single call. Denying `socket` while
/// leaving `socketpair` is not a denial.
///
/// Compiled only for tests, because that is all it is now: an oracle, not a
/// filter. Building it into the shipped binary would suggest it does something
/// at run time.
#[cfg(all(target_os = "linux", test))]
fn must_stay_denied() -> Vec<i64> {
    let mut denied = vec![
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
        libc::SYS_sendmmsg,
        libc::SYS_recvfrom,
        libc::SYS_recvmsg,
        libc::SYS_recvmmsg,
        // Opening files. The fonts are in the binary and the pipes are already
        // open, so nothing legitimate opens anything.
        libc::SYS_openat,
        libc::SYS_openat2,
        libc::SYS_truncate,
        libc::SYS_unlinkat,
        libc::SYS_renameat2,
        libc::SYS_mkdirat,
        // The other routes to a file descriptor. Denying `openat` and stopping
        // there is the version of this that reads well and does not hold:
        // `open_by_handle_at` opens a file from a handle rather than a path,
        // and the mount API added `open_tree` and `fsopen`, which return
        // descriptors without going anywhere near `open`.
        libc::SYS_name_to_handle_at,
        libc::SYS_open_by_handle_at,
        libc::SYS_open_tree,
        libc::SYS_fsopen,
        libc::SYS_fsmount,
        libc::SYS_move_mount,
        // Asynchronous I/O, which is the hole a syscall denylist is worst at.
        // An io_uring ring performs opens, reads, writes, and network
        // operations on the *kernel* side: the filter sees `io_uring_enter` and
        // nothing about what was queued into it, so every denial above is
        // reachable around. It is also, on the record of the last few years,
        // the single richest source of kernel privilege escalations. Nothing
        // here uses it — there is no async runtime in a renderer that reads one
        // pipe and computes.
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        // Starting or inspecting other processes. A renderer that has been
        // taken over should not be able to run anything, and `pidfd_getfd`
        // takes a descriptor *out* of another process, which would reach the
        // parent's sockets without opening one.
        libc::SYS_execve,
        libc::SYS_execveat,
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_pidfd_open,
        libc::SYS_pidfd_getfd,
        // Kernel surface the renderer has no use for and attackers do. These
        // are not about what the renderer could reach directly — most need
        // privileges it does not have — but about how much of the kernel a bug
        // in this process can be pointed at. `userfaultfd` and
        // `perf_event_open` in particular have carried a long run of local
        // privilege escalations, and a user namespace is how an exploit gets
        // the capabilities it was missing.
        libc::SYS_bpf,
        libc::SYS_userfaultfd,
        libc::SYS_perf_event_open,
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_unshare,
        libc::SYS_setns,
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_kexec_load,
        // Changing what the filesystem looks like, and reading the kernel log.
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_chroot,
        libc::SYS_syslog,
        // Creating filesystem entries. Nothing legitimate here writes anything.
        libc::SYS_symlinkat,
        libc::SYS_linkat,
        libc::SYS_mknodat,
    ];

    // The pre-`*at` calls, which the architectures designed after them never
    // had. Naming them unconditionally does not compile on aarch64 — `libc` has
    // no `SYS_open` there, because Linux has no `open` there — which is how this
    // whole file turned out to be x86_64-only.
    //
    // Each of these was checked against `libc` on all three architectures
    // rather than assumed from the shape of the name: `accept` looks like it
    // belongs in this group and does not, and putting it here would have left
    // ARM able to accept connections.
    #[cfg(target_arch = "x86_64")]
    denied.extend_from_slice(&[
        libc::SYS_open,
        libc::SYS_creat,
        libc::SYS_unlink,
        libc::SYS_rename,
        libc::SYS_mkdir,
        libc::SYS_symlink,
        libc::SYS_link,
        libc::SYS_mknod,
    ]);
    // `renameat` predates `renameat2` and survives on x86_64 and aarch64, but
    // not on riscv64, which only ever had the newer one.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    denied.push(libc::SYS_renameat);

    denied
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
/// Takes the port and the file path to probe, because *neither can be derived
/// on the far side*. Not in the usage text — it is how the self-test talks to
/// itself.
pub const SELFTEST_PROBE_ARGUMENT: &str = "--confine-selftest-probe";

/// What the probes are pointed at.
///
/// Both halves of the Windows self-test have to agree on these, and they are
/// two different processes, so they are passed rather than derived. The first
/// attempt derived the file path from `temp_dir` on the assumption that the
/// child inherits `TMP` — it does, and an AppContainer *redirects* it anyway, so
/// the child probed
/// `…\Packages\2kbrowser.renderer\AC\Temp\` and got `NotFound` for a file that
/// was never there. A check that cannot fail, again, for a new reason.
struct Targets {
    /// A port in the parent, with something listening on it.
    port: u16,
    /// A file the parent has written and can read.
    file: std::path::PathBuf,
}

/// Applies confinement and then tries the things it is supposed to prevent.
///
/// Prints one line per attempt. Uses `std` rather than raw syscalls on purpose:
/// `TcpStream::connect` and `File::open` are what a compromised renderer would
/// reach for, and they go through the same calls the sandbox names.
///
/// On Windows this cannot confine the process it is running in, so it builds a
/// container and runs the probes inside one instead.
pub fn selftest() -> String {
    // A listener the probe can actually connect to, rather than a port nothing
    // answers on. That was the first design, and it was wrong on Windows:
    // AppContainer's network block is enforced by the firewall, which resets
    // the connection rather than failing the call, so a blocked connect and a
    // dead port both report `ConnectionRefused`. With something listening,
    // `OPENED` means the sandbox failed and nothing else does.
    //
    // Bound here, before anything is confined — under seccomp `bind` is denied,
    // and the point is to test `connect`.
    let listener = std::net::TcpListener::bind("127.0.0.1:0");
    let port = match &listener {
        Ok(listener) => match listener.local_addr() {
            Ok(address) => address.port(),
            Err(error) => return format!("confinement=Unknown\nLISTENER-UNBOUND({error})"),
        },
        Err(error) => return format!("confinement=Unknown\nLISTENER-UNBOUND({error})"),
    };

    // Written *and read back* by this process, so that a refusal on the far
    // side is the sandbox refusing and not a path that was never good.
    let file = std::env::temp_dir().join("2kbrowser-confine-probe");
    if let Err(error) = std::fs::write(&file, b"probe") {
        return format!("confinement=Unknown\nPROBE-UNWRITABLE({:?})", error.kind());
    }
    if let Err(error) = std::fs::File::open(&file) {
        return format!("confinement=Unknown\nPROBE-UNREADABLE({:?})", error.kind());
    }
    let targets = Targets { port, file };

    let report = {
        #[cfg(target_os = "windows")]
        {
            windows_selftest(&targets)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let confinement = apply();
            format!("confinement={confinement:?}\n{}", probes(&targets))
        }
    };
    // Echoed by the side that chose them, so that a reader — and the test — can
    // see whether the probes on the far side were aimed at the same things.
    format!(
        "{report}\nexpect-port={}\nexpect-file={}",
        targets.port,
        targets.file.display()
    )
}

/// Builds a container, runs the probes inside it, and reports both halves.
#[cfg(target_os = "windows")]
fn windows_selftest(targets: &Targets) -> String {
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
    // Quoted because a Windows temp path routinely contains a space, and the
    // child parses this back out of one command line.
    let arguments = format!(
        "{SELFTEST_PROBE_ARGUMENT} {} \"{}\"",
        targets.port,
        targets.file.display()
    );
    match crate::contain::capture(&container, &arguments, std::time::Duration::from_secs(30)) {
        Ok(inside) => format!("confinement=AppContainer\n{}", inside.trim_end()),
        // Not `AppContainer`: nothing ran inside one. Printing that above a
        // line explaining the launch failed is the self-test lying about the
        // one thing it exists to check, and it did — a real machine reported
        // `confinement=AppContainer` and `spawn-failed=` together.
        Err(reason) => format!("confinement=Failed\nreason={reason}"),
    }
}

/// Tries the things confinement is supposed to prevent, and says what happened.
///
/// Split out from [`selftest`] because on Windows the process that confines and
/// the process that is confined are not the same one.
fn probes(targets: &Targets) -> String {
    let mut lines = Vec::new();

    // Something *is* listening, so `OPENED` is the only outcome that means the
    // network was reachable, and it is unambiguous. Anything else is the
    // attempt being stopped — by a syscall filter, by a firewall, by a token
    // with no network capability. Which of those it was is not the question
    // this answers.
    let socket = std::net::TcpStream::connect(("127.0.0.1", targets.port));
    lines.push(format!(
        "socket={}",
        match socket {
            Ok(_) => "OPENED".to_owned(),
            Err(error) => format!("{:?}", error.kind()),
        }
    ));

    lines.push(format!(
        "file={}",
        match std::fs::File::open(&targets.file) {
            Ok(_) => "OPENED".to_owned(),
            Err(error) => format!("{:?}", error.kind()),
        }
    ));
    lines.push(format!("port={}", targets.port));
    lines.push(format!("file-path={}", targets.file.display()));

    // A call nothing here has ever named, and which is harmless: reading the
    // working directory. It is the probe that tells an allowlist from a
    // denylist, and neither of the two above can. `socket` and `openat` were
    // refused under the old filter too, so a report showing them refused is
    // consistent with a filter that permits everything nobody thought of —
    // which is precisely what this replaced.
    //
    // Only meaningful on Linux. An AppContainer restricts access to resources
    // rather than filtering calls, so `GetCurrentDirectory` inside one succeeds
    // and should: the line is still printed there, and the test that reads it
    // asserts only where the mechanism is a syscall filter.
    lines.push(format!(
        "unnamed-call={}",
        match std::env::current_dir() {
            Ok(_) => "ALLOWED".to_owned(),
            Err(error) => format!("{:?}", error.kind()),
        }
    ));

    // Something harmless, to prove the sandbox did not simply break everything —
    // two refusals above are also what a filter that killed the whole process
    // would produce if it somehow got this far.
    lines.push(format!("compute={}", (1..=10).sum::<u32>()));

    lines.join("\n")
}

/// Runs the probes in a process something else has already confined.
///
/// Takes the port and path from the command line: see [`Targets`] for why
/// neither can be worked out from inside a container.
pub fn selftest_probe(arguments: &[String]) -> String {
    let Some(port) = arguments.first().and_then(|port| port.parse::<u16>().ok()) else {
        return "NO-PORT".to_owned();
    };
    let Some(file) = arguments.get(1) else {
        return "NO-FILE".to_owned();
    };
    probes(&Targets {
        port,
        file: std::path::PathBuf::from(file),
    })
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

    #[cfg(target_os = "linux")]
    #[test]
    fn nothing_dangerous_is_reachable_through_the_allowlist() {
        // The check that matters after the inversion. An allowlist does not
        // name `socket` or `io_uring_enter`, so nothing stops someone widening
        // it until one of them is reachable again — except this, which walks the
        // families the denylist used to name and asserts none of them made it
        // in.
        let allowed = allowed();
        for dangerous in must_stay_denied() {
            assert!(
                !allowed.contains(&dangerous),
                "syscall {dangerous} is allowed and must not be",
            );
        }

        // No duplicates, and not empty: a duplicate is harmless to the filter
        // and a sign that two `cfg` blocks overlap.
        let mut sorted = allowed.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), allowed.len(), "a syscall is named twice");
        assert!(!allowed.is_empty());

        // The measured core. Named individually rather than counted, because a
        // renderer that cannot `read` its pipe or `write` its answer is not
        // confined, it is broken, and the failure would look like a hang.
        for essential in [
            libc::SYS_read,
            libc::SYS_write,
            libc::SYS_mmap,
            libc::SYS_exit_group,
            // The abort path. Refusing this does not stop a renderer aborting;
            // it stops it *finishing* aborting.
            libc::SYS_tgkill,
        ] {
            assert!(
                allowed.contains(&essential),
                "syscall {essential} is needed"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_denied_families_are_built_for_this_architecture() {
        // Syscall numbers are per-architecture, so a list assembled with the
        // wrong `cfg` names whatever happens to share a number. Two things
        // worth asserting cheaply: that the list is not empty on a platform
        // that claims to confine, and that no number appears twice — a
        // duplicate is harmless and a sign the `cfg` blocks overlap.
        let denied = must_stay_denied();
        assert!(!denied.is_empty());
        let mut sorted = denied.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), denied.len(), "a syscall is named twice");

        // The pre-`*at` calls exist only where Linux still has them, and
        // `accept` is the one that looks like it belongs in that group and does
        // not — it is on all three architectures, and leaving it out would have
        // let an ARM renderer accept connections.
        assert!(denied.contains(&libc::SYS_accept));
        #[cfg(target_arch = "x86_64")]
        assert!(denied.contains(&libc::SYS_open));

        // io_uring, named specifically because it is the one entry whose
        // absence would quietly undo the rest of the list: a ring does opens
        // and network I/O on the kernel side, where a syscall filter cannot see
        // them. Anyone tidying this list should have to delete this assertion
        // on purpose.
        for ring in [
            libc::SYS_io_uring_setup,
            libc::SYS_io_uring_enter,
            libc::SYS_io_uring_register,
        ] {
            assert!(denied.contains(&ring), "io_uring is reachable");
        }
        // The routes to a descriptor that are not `openat`.
        assert!(denied.contains(&libc::SYS_open_by_handle_at));
        assert!(denied.contains(&libc::SYS_open_tree));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_architecture_without_a_filter_is_unconfined_rather_than_guessed_at() {
        // The mismatch branch of seccompiler's architecture check is
        // `SECCOMP_RET_KILL_PROCESS`, so naming the wrong architecture does not
        // fail open or closed — it kills the renderer on its next syscall. The
        // only safe answer for an architecture this was not built for is no
        // filter at all, said out loud.
        if architecture().is_none() {
            assert_eq!(apply(), Confinement::Unavailable);
        } else {
            assert_ne!(apply(), Confinement::Unavailable);
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
