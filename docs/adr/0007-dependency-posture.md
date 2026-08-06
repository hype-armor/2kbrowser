# ADR-0007: Dependency posture

Status: accepted

## Context

"From-scratch engine" invites a maximalist reading in which everything is
written here. That reading is wrong in a specific and dangerous way: several of
the components a browser needs are simultaneously the hardest to get right, the
most security-critical, and the least interesting as product work. Hand-rolling
them buys nothing and costs years.

Two categories in particular:

**Specified error recovery.** The HTML parsing algorithm is not a grammar — it
is a normative description of how to recover from broken markup, reverse
engineered from what browsers already did. A hand-written HTML parser is not a
simpler version of `html5ever`; it is a wrong one. The same applies to CSS
tokenisation and to character encoding sniffing, both of which are specified
precisely because implementations disagreeing was a bug.

**Text.** Shaping, line breaking, and bidi are the single hardest part of a
renderer, and getting them wrong is not a subtle failure — it renders entire
writing systems unreadable.

TLS belongs in neither category and is simply never a thing to write yourself.

Against that, every dependency is a supply-chain surface and a maintenance
obligation, and the project has limited review capacity.

## Decision

Take from upstream:

| Concern | Crate |
| --- | --- |
| HTML parsing | `html5ever` |
| CSS tokenisation | `cssparser` |
| Character encoding detection and decoding | `encoding_rs` |
| TLS | `rustls` |
| Shaping, line breaking, bidi | `cosmic-text` (`rustybuzz` + `swash`) |
| Image decoding | `image`, restricted to GIF, JPEG, PNG |
| Windowing and surfaces | `winit`, `softbuffer` |
| Rasterisation | `tiny-skia` |

Write here — this is the actual project:

- The DOM tree (arena-allocated, index-based; not `Rc<RefCell<_>>`)
- CSS 2.1 cascade: selector matching, specificity, inheritance, computed values
- Box tree and layout: block, inline, floats, tables, positioning, quirks mode
- Display list construction and painting
- Resource loading, cache, and network *policy*
- Browser chrome and input handling
- The slop layer

Note that M0 itself has zero third-party dependencies; the table above describes
what M1 and M2 are expected to pull in.

## Consequences

- The security-critical parsing surface is maintained by people who specialise
  in it, and CVEs there arrive as version bumps.
- Using a modern HTML parser for old content is correct, not a compromise: the
  HTML5 parsing algorithm was derived from how browsers handled exactly this
  era's markup.
- Each dependency is a supply-chain surface. Additions beyond this list should
  be argued for, since "every dependency avoided is code we maintain forever"
  cuts both ways.
- A stricter near-zero-dependency stance remains defensible, but roughly doubles
  M1 and M2 and moves shaping and TLS in-house. It is not recommended, and
  remains open in PLAN.md §9.
