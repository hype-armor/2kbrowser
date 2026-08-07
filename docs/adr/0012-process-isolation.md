# ADR-0012: A sandboxed renderer process

Status: accepted

Decides the process model for M4 hardening. Raised as an open question during
M4 and answered directly rather than through an issue.

## Context

M4's stated goal is that we are willing to tell a stranger to browse untrusted
sites with this. Deciding whether that is defensible means being precise about
what an attacker actually gets to touch.

The tempting argument is that ADR-0002 already settles it: `unsafe` is
`forbid`den across the workspace, so a hostile page has no memory-safety bug to
reach. That argument is false, and it is worth writing down why, because it is
the kind of thing that gets re-derived approvingly by someone who has not
counted.

Our code cannot have a memory-safety bug. The code that meets attacker-chosen
bytes *first* is not our code. Counting `unsafe` in the crates on that path, at
the versions in the current lockfile:

| crate | `unsafe` occurrences | reached by |
| --- | --- | --- |
| `encoding_rs` | 271 | every page |
| `zune-jpeg` (via `image`) | 85 | every JPEG |
| `swash` (via `cosmic-text`) | 54 | every glyph outline |
| `cssparser` | 20 | every stylesheet |
| `rustybuzz` (via `cosmic-text`) | 15 | shaping |
| `image` | 11 | every image |
| `html5ever` | 4 | every page |

Roughly 460 `unsafe` sites between a hostile document and the process, in
libraries ADR-0007 deliberately chose not to write here — and would still
choose, because hand-rolling them produces something quietly wrong rather than
merely late. The decision to take them is right and does not become wrong here.
What it means is that "we forbid `unsafe`" describes our discipline, not our
attack surface.

`tiny-skia` has 148 more, but they are not in the same category: it consumes a
display list we built, not bytes a stranger chose.

Three models were considered.

**Single-process with in-process mitigations.** `catch_unwind` at the page
boundary, allocation caps, a watchdog. Cheap and portable. Contains panics and
some resource exhaustion, and contains none of the surface above: it makes the
browser harder to crash, not harder to exploit.

**Sandbox the decoders only.** Push image and character-set decoding into a
short-lived child. Covers the largest concentration — roughly 360 of the 460 —
at a fraction of the cost, and leaves `html5ever`, `cssparser`, and shaping
inside the trusted process.

**A sandboxed renderer process.** The parent keeps the chrome, the network, and
the disk. A child parses, lays out, and rasterises, and is given no OS access of
its own. This is what every browser that survived contact with the web
converged on.

## Decision

The third: a sandboxed renderer process.

Three properties made it cheaper here than the usual estimate of this work.

**The renderer is already a pure function.** `render(html, width, height,
fonts) -> Page` takes bytes and returns a pixmap. It holds no shared mutable
state, calls back into nothing, and reaches the network only through an
explicitly passed base URL. Real browsers spend years on this split because
their renderers are entangled with everything; ours is not, *yet*. Doing it
before M5 adds the slop layer is much cheaper than doing it after.

**The network moves to the trusted side, which is an improvement on its own.**
A child with no sockets cannot exfiltrate anything regardless of what it is
tricked into computing, and ADR-0006's policy is then enforced somewhere a
compromised renderer cannot reach. Subresources become a request the child
*asks* for and the parent decides on.

**It gives M4's fuzzing its missing half.** `tests/fuzz` times each input after
the fact, so an input that never returns hangs the harness rather than being
reported — recorded as a known gap when it landed. A child process can be
killed. An in-process loop cannot.

## Consequences

- Three sandbox implementations, not one: seccomp-bpf with Landlock or user
  namespaces on Linux, Seatbelt on macOS, an AppContainer with a restricted
  token and a job object on Windows. There is no portable answer and pretending
  otherwise would produce a sandbox that is real on one platform and decorative
  on the other two.
- The sandbox primitives are security-critical by construction, so taking a
  crate for them falls inside the dependency rule set in
  [issue #4](https://github.com/hype-armor/2kbrowser/issues/4) rather than
  needing a separate argument.
- A process boundary needs a protocol, and a protocol is a parsing surface —
  one now reachable *from* the sandboxed side. It is written here, it is
  length-prefixed and fully bounds-checked, and it is fuzzed like every other
  parser rather than trusted because both ends are ours.
- The 4 MiB embedded font payload is per-process. Acceptable at this size and a
  reason the coverage payload must move out of the binary before it grows
  (issue #7's recorded consequence).
- Process separation and OS sandboxing are separable, and land separately.
  Separation alone buys crash containment, hang killing, and memory limits;
  it does not contain an exploit. Until the platform primitives are applied the
  README must not claim it does.
- Rendering costs a process spawn. Measured rather than assumed, and the
  cold-start budget in `tests/budgets` is where it shows up.
