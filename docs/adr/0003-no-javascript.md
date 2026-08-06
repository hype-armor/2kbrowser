# ADR-0003: No JavaScript

Status: accepted

## Context

The project's stated purpose is a browser "without the slop." Cataloguing what
that means in practice — consent modals, newsletter popups, notification
prompts, autoplay video, infinite scroll, tracking beacons, ad auctions,
layout that moves under the cursor — produces a list whose members are almost
all delivered by JavaScript.

Three options were considered:

1. **Full JS support.** Requires embedding or writing an engine, and re-admits
   every category above. It also makes the resource budgets in PLAN.md §4
   unreachable, since a JIT and a JS heap dominate a modern browser's footprint.
2. **A tiny JS subset.** Superficially a compromise.
3. **No JS at all.**

Option 2 is the trap. Sites feature-detect. A browser that runs *some*
JavaScript advertises that it runs JavaScript, is served the script path, and
then fails in silent, page-specific ways that are extremely hard to diagnose. A
browser that runs none is served `<noscript>` content and static fallbacks —
the behaviour we actually want. Partial support collects the downsides of both
ends and the benefits of neither.

## Decision

2kbrowser does not execute JavaScript. This is a property of the product, not a
gap in it.

There is no JavaScript milestone on the roadmap. Should the decision ever be
revisited, the only acceptable shape is an explicit, per-site, off-by-default
escape hatch backed by a real engine (QuickJS) — never a homegrown subset — and
it would require a new ADR superseding this one.

## Consequences

- Most page-level slop is eliminated by construction, with no filter lists to
  maintain and no update treadmill.
- The sites that break are disproportionately the sites generating the slop.
  The constraint doubles as the filter.
- Modern web *applications* are out of scope. This browser reads documents.
- Together with ADR-0004, many current sites will render as jumbled boxes rather
  than as clean documents. Reader mode is what makes this tolerable, and there
  is a live question (PLAN.md §9) about scheduling it earlier than M5.
- No JIT, no JS heap, and no script-driven reflow makes the PLAN.md §4 budgets
  achievable rather than aspirational.
