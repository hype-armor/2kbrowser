# ADR-0013: Period engine, present-day security

Status: accepted

Overrules the earlier answer on
[issue #3](https://github.com/hype-armor/2kbrowser/issues/3), and sets a
standing rule for the rest of M4.

## Context

ADR-0004 fixes the engine's era at CSS 2.1 and ADR-0011 keeps the shell modern.
Neither says anything about the security posture, and the gap let a bad
inference in.

That inference was mine, and it is worth recording so nobody repeats it.
ADR-0006 allows plain HTTP because much of the surviving old web needs it, on
condition it is never presented as secure. Legacy TLS looks like the same
question wearing a different hat, so issue #3 was answered with the same
treatment: a marked, per-site, opt-in downgrade.

The analogy does not hold, for two reasons.

**A marked downgrade is not markable.** Plain HTTP is visibly unauthenticated,
and a reader can price that in — the whole page is in the clear and always was.
A downgraded TLS session looks exactly like a working one. Any marking has to
compete with a padlock-shaped intuition that took twenty years to build, and it
loses. Refusing is the only version that cannot be misread.

**There is nothing to opt into.** `rustls` contains no TLS 1.0 or 1.1 code at
all. They were removed upstream as policy, not hidden behind a feature. So
"allow it per-site" means adding a second TLS stack — taking on a dependency in
order to be less safe, in a project whose entire pitch is that it is safe to
point at old things.

The general form of the mistake: reading "renders the web of 2000" as licence to
accept the *security* of 2000. It is not. The era is a rendering scope, and it
stops at the engine.

## Decision

**The engine is period. Everything protecting it is present-day.** Where a
choice exists between a current mechanism and one contemporary with the content,
take the current one, and take the unreachable-site outcome rather than reaching
for a deprecated tool to avoid it.

Concretely, and immediately:

- **TLS 1.0 and 1.1 are refused.** Not marked, not per-site, not behind a
  confirmation. Sites that support nothing newer do not load, and that is the
  intended outcome. Issue #3 resolves to its first option.
- **No second TLS stack, ever, for compatibility's sake.** `native-tls` accepts
  whatever the platform allows, which on an older machine includes TLS 1.0.
  `rustls` is the provider and that is asserted rather than inherited.
- **No relaxed certificate verification.** No expired-certificate escape hatch,
  no self-signed exception, no `disable_verification`. The switch that would
  turn every padlock into theatre is stated as `false` in
  `crates/net/src/tls.rs` so that it is a decision rather than a default.
- **Mozilla's roots rather than the platform's.** Identical behaviour on all
  three platforms, in the spirit ADR-0005 asks of rendering — and a corporate
  root installed in the system store cannot silently read this browser's
  traffic.

The rule also corrects ADR-0012, which named **Seatbelt** as the macOS sandbox
mechanism. `sandbox_init` has been deprecated since macOS 10.8; browsers still
use it, but "what Chromium does with twenty years of momentum" is not the same
question as "what a new program should adopt". The macOS target is the **App
Sandbox** entitlement model. Linux remains **Landlock plus seccomp-bpf**, both
current; Windows remains an **AppContainer** with a restricted token and a job
object. ADRs are immutable, so this supersedes rather than edits.

## Consequences

- Some of the old web is unreachable, permanently and by choice. A browser built
  to read archived pages will fail on a subset of live ones that never upgraded.
  The Internet Archive serves the same content over modern TLS, which covers
  most of the loss.
- The failure needs to be legible. "Refused: this site only offers obsolete
  encryption" is a different message from a network error, and the chrome should
  say which — otherwise the honest refusal reads as a bug. Not yet built.
- Being right today is not the property that matters; not quietly becoming wrong
  is. The posture is asserted in tests, so a dependency bump that changed a
  default fails CI rather than shipping.
- TLS 1.2 is still accepted. It is current, not legacy, and the overwhelming
  majority of the web speaks it. Restricting to 1.3 alone would refuse a large
  share of ordinary sites for a small margin, and is not what this decides —
  but it is a knob that now has one obvious place to be turned.
