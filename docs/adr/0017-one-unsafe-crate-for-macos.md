# ADR-0017: One crate may write `unsafe`, so that macOS can be confined

Status: accepted

Amends [ADR-0002](0002-rust-and-no-unsafe.md), which forbids `unsafe` in this
workspace. That decision stands everywhere except one crate, named here.
Completes the sandbox story begun in [ADR-0012](0012-process-isolation.md) and
continued in [ADR-0014](0014-appcontainer-through-a-dependency.md) and
[ADR-0016](0016-syscall-allowlist-measured.md). Answers the macOS half of
[issue #4](https://github.com/hype-armor/2kbrowser/issues/4).

## Context

Linux and Windows both confine the renderer, and neither needed an exception to
ADR-0002. Linux installs a seccomp filter through `seccompiler`; Windows builds
an AppContainer through `rappct`. In both cases somebody else had already
written the `unsafe`, published it, and taken on maintaining it — ADR-0014
recorded that trade for Windows and did not pretend it was free.

macOS has an equivalent mechanism and no such crate. Confining a process from
inside itself means calling `sandbox_init`, a C function in libSystem, and there
is no safe wrapper for it on crates.io — only raw FFI bindings, which is the
same `unsafe` with an extra dependency in front of it. So the pattern that
worked twice does not work a third time, and macOS has been the one platform
where the renderer is a separate process and nothing else.

That is the worst position of the three. The README has carried a warning about
it, which is honest and is not protection: a reader who wants this browser
confined on their Mac is not helped by a paragraph explaining why it is not.

Three options were on the table.

**Leave it unconfined.** Defensible while ADR-0002 was read as absolute, and it
makes the milestone unreachable — M4 is done when we would tell a stranger to
browse untrusted sites with this, and "except on macOS" is not that.

**Write a wrapper crate and publish it.** The dependency-shaped answer, and the
same code either way. It relocates the `unsafe` rather than removing it, and it
would make this project responsible for a crate other people depend on.

**Carve out one crate here.** What ADR-0002 anticipated: it chose `forbid` over
`deny` specifically so that an exception has to be an edit to a manifest, "a
reviewable diff rather than a quiet local exception". This is that diff.

## Decision

`crates/seatbelt` does not inherit the workspace lints, and may write `unsafe`.
No other crate may.

The exception is bounded by four things, and the argument depends on all of
them:

**It contains one call.** `sandbox_init`, plus the deallocator libSystem pairs
with its error string. The crate takes a `&str` and returns a `Result`.

**It contains no policy.** What the profile says — what is denied, what is
allowed, and why — lives in `sandbox::confine` beside the Linux and Windows
policies, in a crate where `unsafe` is still forbidden. The split is the point:
an exception is only as narrow as the code inside it, so the decisions worth
arguing about are kept outside it.

**It is small enough to read in one sitting, and that is enforced.** A test
fails if the crate exceeds 200 lines. The way this decision degrades is not
somebody choosing to abandon it — it is a second useful thing being added to the
one crate where `unsafe` happens to be available.

**It cannot spread.** A test reads every workspace member's manifest and asserts
that `seatbelt` is the only one not inheriting the workspace lints. Copying four
lines into another `Cargo.toml` produces no warning and would read in review as
consistency with an existing pattern; it now fails a test whose message says an
ADR is required.

## Consequences

**ADR-0002's central claim needs restating precisely.** It is no longer "this
workspace contains no `unsafe`". It is: this workspace contains one `unsafe`
crate, a page long, holding one foreign call, on the path that *takes privileges
away*. Nothing that parses a byte from the network is in it. That is a weaker
claim and it is the one that is true, which matters more — ADR-0002's own
reasoning is about the code that meets hostile input, and this is the opposite
end of the program.

**The exception is where the risk is lowest and the reward is highest.** A bug
in `seatbelt` is a sandbox that fails to install, which is reported and which
leaves macOS exactly where it is today. A bug in a parser is a bug in a parser.

**macOS is confined by a strict-deny profile, which is stronger than Linux's and
more brittle.** `(deny default)` is where SBPL starts, so there is no denylist
question of the kind ADR-0016 answered. It also has no equivalent of the `EPERM`
default that makes the Linux allowlist safe to ship on a measurement: a resource
this profile forgot is refused outright. Two allowances are named — signalling
itself, and reading sysctls — and the first is a lesson imported rather than
guessed, because refusing the equivalent on Linux made a panicking renderer die
of `SIGSEGV` instead of `SIGABRT`.

**It is tested where it runs, which is CI and not here.** Neither author has a
Mac to hand. The self-test applies the profile and then tries what it forbids,
from inside, on every push — which is the same standard the other two platforms
are held to, and is how both of the Windows self-test's earlier mistakes were
caught.

**A published wrapper crate remains the better long-term answer**, and this does
not foreclose it. If one appears that is maintained and reviewed, `seatbelt`
becomes a dependency swap and the exception goes away with it.
