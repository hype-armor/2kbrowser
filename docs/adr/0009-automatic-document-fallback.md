# ADR-0009: Re-render as a document when a page is too modern

Status: accepted

Addresses the consequence recorded in [ADR-0003](0003-no-javascript.md) and
[ADR-0004](0004-css-21-scope-boundary.md): without JavaScript and without
flexbox or grid, many current pages render as jumbled boxes rather than as
clean documents.

## Context

That failure mode is worse than it sounds. "Plain" is fine — a page rendered
without the author's visual design is still readable. "Jumbled" is not: absolutely
positioned navigation lands on top of body text, flex containers collapse to
stacked full-width blocks in arbitrary order, and the result reads as a broken
browser rather than as a deliberate one.

ADR-0008 does not help here. Fonts fix glyphs; they do not fix layout.

The important observation is that **the engine can know when it is out of its
depth.** We parse CSS we do not implement — `display: flex` and `display: grid`
are tokenised and cascaded like any other declaration; we simply have no layout
algorithm for them. So "does this page's layout depend on features we lack?" is a
question we can answer by inspection, not by guesswork. That is a much stronger
position than content heuristics, and it is available for free during cascade.

## Decision

After cascade and before layout, classify the document into one of three states.
The classification is automatic; the user is told which state was chosen and can
override it.

**1. Renderable.** Layout depends only on CSS 2.1. Lay it out normally. This is
the path for the era's web and for the large fraction of the modern web that is
still ordinary block-and-inline markup.

**2. Too modern.** The document has real content, but a significant share of it
sits inside containers whose layout we do not implement. Do not produce a layout
we know to be wrong — **re-render the document in reader mode**: extract the
content, discard the author's layout entirely, and present it with our own
document styling.

The share is measured by *text content governed by unsupported layout*, not by
element count — one flex container wrapping the whole page matters, fifty flex
containers in a footer do not. Initial threshold 40%, tuned against a corpus, and
recorded where it can be changed deliberately.

**2b. Too modern in its frame.** Amended after pointing the browser at
Wikipedia, which the share above cannot see and which no proportional measure
can. An article's prose really is ordinary flow: by volume the page is
overwhelmingly correct, and every share — of text, of boxes, of misplaced
blocks — reports it as fine. What is wrong is the structure *around* the prose,
which is a handful of containers holding almost no text.

So there is a second signal, and it is a count rather than a share, which is a
deliberate departure from the paragraph above. A container is counted when it
uses layout we do not implement **and holds two or more block-level children**:
those children are meant to sit side by side and will instead stack down the
page, which changes the page's shape rather than its contents. A container
holding one block, or holding only inline content, changes nothing when it is
ignored and is not counted.

Measured rather than guessed, which is what the "needs a corpus" note below
asked for. Row-forming containers at a 1000px viewport:

| page                         | rows | unsupported containers |
|------------------------------|------|------------------------|
| `example.com`                |    0 |                      0 |
| `info.cern.ch`               |    0 |                      0 |
| `news.ycombinator.com`       |    0 |                      0 |
| `motherfuckingwebsite.com`   |    0 |                      0 |
| `gnu.org`                    |    0 |                      8 |
| this repository's `era-page` |    0 |                      0 |
| **Wikipedia, an article**    | **16** |                **484** |

Every page that should keep its author's layout scores zero — `gnu.org`
included, which has eight flex containers and not one of them a row. The
threshold is **4**: well clear of zero, well under sixteen. Six pages is a small
corpus and this will want revisiting with a larger one, which is the same
caveat the share carries.

**3. No content without JavaScript.** The document has near-zero text content
and a non-trivial number of `<script>` elements — an SPA shell such as an empty
`<div id="root">`. Reader mode cannot help; there is nothing to extract. Show an
honest "this page requires JavaScript" state naming the reason. Do not show a
blank page, and do not show an empty reader view, both of which read as a crash.

Reader mode therefore moves from **M5 to M3**, and the detection hook lands in
**M2** with layout. It is no longer a feature that polishes the browser; it is
what makes it work at all on the modern web.

### Never silently

PLAN.md §7 already sets this rule for the AI-slop layer: *surface a signal, never
silently hide a page.* The same rule applies to rendering mode. When the browser
re-renders a page as a document, it says so, in the chrome, with a control to
force the raw layout. A browser that quietly changes how it renders is a browser
you cannot trust to be showing you the page.

## Consequences

- The main user-visible cost of ADR-0003 and ADR-0004 is contained. Modern pages
  become readable documents instead of broken layouts — which is arguably the
  browser's most honest possible output, given it renders documents by design.
- Detection is exact rather than heuristic for state 2, because it is derived
  from our own capability set. Only the *threshold* is a judgement call.
- State 3 turns the worst failure — a blank window — into an explanation.
- Reader-mode quality becomes load-bearing much earlier than planned. Content
  extraction is now on the critical path for M3 rather than a refinement in M5.
- M3 grows. M5 shrinks to content-quality signals and blocklists.
- Some pages will sit near the threshold and flip between modes on small
  changes. The override control matters, and the threshold needs a corpus behind
  it rather than a guess.
- This does not weaken ADR-0004's boundary. We still do not implement flexbox or
  grid; we detect that a page needs them and choose a different, honest output.
