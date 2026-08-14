# Architecture decision records

Short, numbered, immutable records of decisions that constrain future work. See
[ADR-0001](0001-record-architecture-decisions.md) for the format and the rule
that ADRs are superseded rather than edited.

| # | Decision | Status |
| --- | --- | --- |
| [0001](0001-record-architecture-decisions.md) | Record architecture decisions | accepted |
| [0002](0002-rust-and-no-unsafe.md) | Rust, with `unsafe` forbidden in our own code | accepted |
| [0003](0003-no-javascript.md) | No JavaScript | accepted |
| [0004](0004-css-21-scope-boundary.md) | CSS 2.1 is the scope boundary | accepted |
| [0005](0005-deterministic-rendering.md) | CPU rasterisation and bundled fonts | accepted |
| [0006](0006-network-policy-defaults.md) | Network policy defaults | accepted |
| [0007](0007-dependency-posture.md) | Dependency posture | accepted |
| [0008](0008-font-selection.md) | Font selection | accepted |
| [0009](0009-automatic-document-fallback.md) | Re-render as a document when a page is too modern | accepted |
| [0010](0010-font-acquisition.md) | Fetch fonts at build time, against pinned checksums | accepted |
| [0011](0011-modern-shell-period-engine.md) | A modern shell around a period engine | accepted |
| [0012](0012-process-isolation.md) | Render in a separate process | accepted |
| [0013](0013-modern-security-only.md) | Period engine, present-day security | accepted |
| [0014](0014-appcontainer-through-a-dependency.md) | The Windows sandbox comes through a dependency | accepted |
| [0015](0015-local-roots-marked.md) | Accept this computer's roots, and say when they were needed | accepted |
| [0016](0016-syscall-allowlist-measured.md) | The renderer's syscall filter is an allowlist, and the list was measured | accepted |
| [0017](0017-one-unsafe-crate-for-macos.md) | One crate may write `unsafe`, so that macOS can be confined | accepted |
