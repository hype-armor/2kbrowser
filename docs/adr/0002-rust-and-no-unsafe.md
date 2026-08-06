# ADR-0002: Rust, with `unsafe` forbidden in our own code

Status: accepted

## Context

This program's entire job is parsing bytes supplied by strangers. HTML, CSS,
images, and HTTP responses all arrive from the open internet, and much of the
content this browser targets is old, malformed, or authored against
long-dead quirks. The parsing paths are the attack surface, and historically
they are where browser CVEs concentrate — overwhelmingly as memory-safety bugs.

A from-scratch engine written in C or C++ would be re-litigating three decades
of that history with a fraction of the review capacity.

## Decision

Implement in Rust.

Additionally, set `unsafe_code = "forbid"` at the workspace level in
`Cargo.toml`. `forbid` is chosen over `deny` deliberately: it cannot be
overridden by an `#[allow]` at a call site, so introducing `unsafe` requires
editing the workspace manifest. That turns a local exception into a reviewable
diff.

This applies to code we write. Dependencies use `unsafe` internally — `tiny-skia`
and `winit` necessarily do — and that is accepted.

## Consequences

- Memory-safety bugs in our own code become largely a non-category. Logic bugs,
  resource exhaustion, and same-origin mistakes remain entirely possible; this
  decision does not address them (see PLAN.md §8).
- Some optimisations available to a C++ engine are closed to us. At the
  performance targets in PLAN.md §4 this is not expected to bind, and a
  measured case for relaxing it would be a new ADR.
- Rust also supplies the specific crates ADR-0007 depends on, which is not true
  of the alternatives.
