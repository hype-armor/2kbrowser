# ADR-0016: The renderer's syscall filter is an allowlist, and the list was measured

Status: accepted

Replaces the denylist described in `crates/sandbox/src/confine.rs` and assumed
by [ADR-0012](0012-process-isolation.md). Makes stale one sentence of
[ADR-0014](0014-appcontainer-through-a-dependency.md), which said the Windows
container was categorically stronger than the Linux filter *because* the Linux
side was a denylist.

## Context

The renderer's seccomp filter named the syscalls it refused: sockets, opening
files, `execve`, `ptrace`, io_uring, and a long tail of kernel surface a
renderer has no use for. Everything else was allowed.

The file said plainly what was wrong with that — a syscall nobody thought of is
permitted — and gave the reason it shipped anyway: an allowlist is what breaks a
browser in the field, because the set a renderer touches is decided by the
allocator, the shaper, and the standard library rather than by us, and it moves
on a toolchain bump. It also named the condition for changing: *an allowlist is
the stronger end state and wants a measured set of what the renderer actually
uses.*

That condition was met by measuring it.

`strace` on real renderer children, across every reference fixture, the fuzzer's
corpus, band and find requests, a re-render at a new width, and subresources
arriving over the pipe: **rendering a page uses nine syscalls.** Reading a pipe,
writing a pipe, four ways of asking for memory, one seed for the hash tables,
the signal stack the runtime sets up, and exiting.

Nine is small enough to change the decision. It is small because of choices
already made and recorded: the fonts are compiled into the binary
([ADR-0010](0010-font-acquisition.md)), so the renderer opens nothing; every
subresource is a request the parent answers
([ADR-0012](0012-process-isolation.md)), so it connects to nothing; and there is
no JavaScript engine ([ADR-0003](0003-no-javascript.md)), so there is no JIT
mapping executable pages. A browser that resolved its own hostnames or ran a
thread pool would not have this option.

Rendering is not the whole of it. A hostile page can also make the renderer
*fail*, and failing uses calls rendering never does — a panic and an abort want
`gettid` and `tgkill`, and a stack overflow from a deeply nested document runs a
signal handler first. Those were measured separately, because no fixture reaches
them and guessing at them is how an allowlist turns a crash into a hang.

## Decision

The filter is an allowlist. `crates/sandbox/src/confine.rs` names what the
renderer may call and refuses everything else.

Its contents are in three groups, kept apart in the source because the
difference between them is the difference between a measurement and a judgement:

1. **Measured, rendering.** The nine.
2. **Measured, failing.** `getpid`, `gettid`, `tgkill`, `rt_sigprocmask`,
   `futex`.
3. **Not measured, and there on purpose.** A short margin, each entry justified
   individually: calls the kernel issues rather than the program
   (`rt_sigreturn`, `restart_syscall`), calls whose presence depends on the libc
   version rather than on us (`madvise`, `mprotect`), and calls whose refusal
   would be baffling rather than protective (`clock_gettime`, which is usually
   the vDSO and is not always).

Two things are kept from the old design rather than inverted with it.

**The default action stays `EPERM`, not `SECCOMP_RET_KILL_PROCESS`.** This is
the unusual choice and it is deliberate. A syscall this list forgot degrades
into an error — in practice a page that fails to render, which the parent
already reports — instead of a renderer that dies where a reader sees it. It is
what makes a stronger filter safe to ship on a measurement taken on one machine,
and it is the honest response to not being able to test every libc and kernel
this will meet.

**The denylist is kept as an assertion.** It is no longer a filter — under an
allowlist, `socket` and `io_uring_enter` do not need naming — but the reasoning
about *why* each family is dangerous is worth more than the list was. It is now
what a test checks the allowlist against, so widening the allowlist has to get
past a check that says why each of those was refused.

The measurement is a script, `scripts/renderer-syscalls.sh`, not a paragraph in
a commit message. It traces the same set of children and prints what was used
and what was refused. Rerun it after a toolchain bump.

## Consequences

**A syscall nobody thought about is now refused.** That is the entire point, and
it is not hypothetical: inverting the filter immediately took away `getcwd` and
`readlink`, which the old list permitted because nobody had thought to name
them, and which the panic handler uses to symbolise a backtrace. The backtrace
degrades; nothing else changed.

**The measurement is of one machine, and says so.** x86_64, glibc, one kernel,
one toolchain. The syscall *numbers* are handled per-architecture already, but
the *set* could differ on musl, on aarch64, or after a Rust release changes how
the standard library starts up. Two things bound that risk: the margin group,
which is where the likely differences were anticipated, and the `EPERM` default,
which turns a difference into a failed render rather than a dead browser. A
missed call is a bug to fix, not an incident.

**ADR-0014's comparison no longer holds.** It argued the Windows AppContainer
was categorically stronger than the Linux filter because one refuses by default
and the other permitted anything unnamed. Both refuse by default now. The
remaining differences are real but narrower: the container is built by the
parent and cannot be tampered with from inside, and it covers resources rather
than calls. That ADR stands as written — it recorded a true comparison at the
time — and this one records that the gap it described has closed.

**It is still not a reason to call the renderer safe.** An allowlist bounds what
a compromised renderer can *ask the kernel for*. It does nothing about a bug in
the parent, and none of this has been reviewed by anyone but its authors. The
README's warning is unchanged.
