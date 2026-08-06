# ADR-0005: CPU rasterisation and bundled fonts, for deterministic rendering

Status: accepted

## Context

Linux, macOS, and Windows are all first-class targets from M1.

For a rendering engine that normally implies triple the correctness-testing
burden. Reference tests compare rendered output against baseline images, and
platform font rasterisers disagree about essentially every glyph — different
hinting, different antialiasing, different subpixel positioning. The standard
consequence is a separate baseline set per platform, each maintained by hand,
each drifting independently.

But nothing forces us to use the platform's rasteriser. Text shaping via
`rustybuzz` and rasterisation via `tiny-skia` are both pure computation with no
system dependency. If the font files are also ours, the entire pipeline from
markup to pixels is deterministic.

There is a second reason to stay on the CPU. PLAN.md's performance constraint is
"fast on a ten-year-old laptop," and on that machine the GPU driver is the least
reliable component in the system.

## Decision

Rasterise on the CPU with `tiny-skia`, shape with `rustybuzz` via `cosmic-text`,
and **bundle our own fonts, never touching the system font rasteriser**.

Reference tests therefore run on all three platforms against a single shared set
of baseline images, and any per-platform pixel difference is a bug rather than
an expected variation.

## Consequences

- Correctness testing on three platforms costs roughly what one platform costs.
  Divergence is detected rather than tolerated.
- **The browser will not use your system fonts and will not look native.** This
  is a real cost, accepted deliberately. For a document renderer it is the right
  trade; for an application platform it would not be.
- Bundled fonts must be redistributable under licences compatible with
  GPL-3.0-or-later, and they count against the binary size budget.
- GPU rasterisation is not precluded, but it is an optimisation that would have
  to preserve determinism or be excluded from reference testing. That would be a
  new ADR.
- Font choice becomes a product decision with rendering consequences, not a
  system detail.
