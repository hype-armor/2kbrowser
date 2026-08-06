# 2kbrowser

A web browser without the slop.

2kbrowser renders HTML and CSS as the web did around the year 2000, and does not
execute JavaScript. That single constraint does most of the work: cookie walls,
newsletter modals, notification prompts, autoplay video, infinite scroll, and
tracking beacons are almost all script-delivered, so they simply do not run —
and an engine with no JavaScript has nowhere to put the usual browser bloat.

The scope boundary is **CSS 2.1**, a completed specification with an official
test suite. Unlike an engine chasing the modern web, this one has a finish line.

See [PLAN.md](PLAN.md) for the full rationale and roadmap, and
[docs/adr/](docs/adr/) for the decisions that constrain it.

## Status

**M0 — foundation.** Workspace, three-platform CI, architecture decision
records, and the budget harness. There is no engine yet: the binary starts,
prints its version, and exits. M1 is the first milestone that renders anything.

> **Not safe for browsing untrusted sites.** Sandboxing, parser fuzzing, and
> TLS review all land in M4. Until then this is a tool for its authors.

## Building

Requires a stable Rust toolchain; `rust-toolchain.toml` pins the components.

```sh
cargo build --release
cargo test --workspace
```

Budgets are enforced in CI and can be run locally after a release build. Checks
that cannot be measured yet report `PENDING` rather than passing:

```sh
cargo run --release -p budgets
```

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).
