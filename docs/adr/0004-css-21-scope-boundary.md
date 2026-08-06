# ADR-0004: CSS 2.1 is the scope boundary

Status: accepted

## Context

The standing objection to any from-scratch browser engine is that web
compatibility is unwinnable. CSS gains features every year, faster than a small
team can implement them, so such a project is behind permanently by
construction and its backlog never shrinks. This is the correct objection, and
it has killed real projects.

The project name resolves it. "2k" is read as the web of approximately the year
2000, and the web of that era is approximately **CSS 2.1** — which is a
*completed* specification. It is frozen. It has an official W3C test suite. It
will never gain another feature.

Targeting a frozen specification converts an unbounded problem into a bounded
one, and gives the project something almost no browser engine has: a finish
line, and a published pass rate to measure the distance to it.

## Decision

CSS 2.1 is the scope boundary.

**In scope:** the CSS 2.1 box model, block layout, inline layout, floats,
tables, positioned layout, and quirks mode.

**Out of scope:** flexbox and grid. Not "later" — out. The era's web laid out
with tables and floats; supporting those *plus* modern layout is exactly how the
scope becomes unbounded again.

The era also imposes requirements a modern-only engine would not have, and these
are in scope: legacy character encodings and encoding sniffing (windows-1252,
ISO-8859-*, Shift_JIS, GB2312, EUC-KR), framesets, animated GIFs, and the era's
image formats only (GIF, JPEG, PNG).

The official CSS 2.1 test suite is the project's north-star metric, tracked from
the first milestone able to pass any of it.

## Consequences

- "Done" becomes a reachable state rather than a receding horizon.
- Progress becomes measurable as a pass rate against a fixed suite, rather than
  as a judgement call.
- Modern sites that depend on flexbox or grid will render as jumbled boxes. With
  ADR-0003 this is the project's main user-visible cost.
- **This boundary will be under constant pressure**, and every "just add
  flexbox" is the compat treadmill returning. PLAN.md §8 names this as the risk
  most likely to kill the project, precisely because it kills it pleasantly.
  Relaxing the boundary requires a new ADR superseding this one.
- Quirks mode is not optional. Pages of this era were authored against it, and
  getting it wrong misrenders them badly.
