# 2kbrowser — Plan

> A web browser without the slop.

Status: **proposal**. Nothing here is built yet. This document exists to get the
direction right before any code is written.

---

## 1. Thesis

Most of what makes the modern web unpleasant is delivered by JavaScript.
Cookie walls, newsletter modals, notification prompts, autoplay video, infinite
scroll, layout that reflows under your cursor, tracking beacons, ad auctions —
almost none of it survives in a browser that does not run scripts.

So the central bet of this project is:

**2kbrowser renders HTML and CSS as the web did around the year 2000. It does
not execute JavaScript.**

That is not a limitation we plan to grow out of. It is the product. It makes
all four kinds of slop fall out of one decision:

| Slop | How the engine addresses it |
| --- | --- |
| **Browser bloat** | No JS engine means no JIT, no sync account, no extension host, no AI sidebar, no sponsored tiles. There is nowhere to put the bloat. |
| **Page-level junk** | Consent modals, popups, autoplay, and infinite scroll are script-driven. They simply do not run. |
| **Resource weight** | No JIT, no JS heap, CPU rasterisation. Tens of MB of RAM, not hundreds. |
| **AI-generated content** | Not solved by the engine — needs its own layer (§7). It is the hardest of the four and is deliberately scheduled last. |

The sites that break without JS are, to a striking degree, the same sites that
generate the slop. Meanwhile Wikipedia, MDN, most documentation, most blogs,
most static-site-generator output, HN, lobste.rs, and much news either work
unmodified or degrade to readable HTML. **The constraint is the filter.**

### Why not "a tiny JS subset"

Settled: **no JavaScript at all.** The middle path is worse than either end.
Sites feature-detect. A browser that runs *some* JS advertises that it runs JS,
gets served the script path, and then fails in confusing, silent, page-specific
ways. A browser that runs none gets served `<noscript>` content and static
fallbacks, which is the behaviour we actually want. Partial JS support is the
one option that gets the downsides of both.

There is no JS milestone on this roadmap. If that ever changes it will be an
explicit, per-site, off-by-default escape hatch backed by a real engine
(QuickJS) — never a homegrown subset — and it will be a new decision, not a
deferred one.

---

## 2. What "2k" means: a specification with a finish line

The name is read as the web of ~2000, and that turns out to be the most
important engineering decision in this document — because it converts the
project's biggest risk into a bounded problem.

A from-scratch engine chasing the modern web can never finish. CSS grows every
year; you are behind permanently by construction. But the 2000s-era web is
approximately **CSS 2.1**, and CSS 2.1 is a *completed specification*. It is
frozen. It has an official test suite. It will never gain a feature again.

That gives this project something almost no browser engine has: **a finish
line.** "Done" is a state we can actually reach and then maintain, rather than
a horizon that recedes.

Concretely, the layout scope is:

- **In:** the CSS 2.1 box model, block, inline, **floats**, **tables**,
  positioned layout, and **quirks mode** (essential — pages of this era were
  authored against it, and getting quirks wrong misrenders them badly).
- **Out:** flexbox and grid. Not "later" — out of scope. The 2000s web laid out
  with tables and floats, and supporting both *plus* modern layout is how the
  scope quietly becomes unbounded again.

This also fixes the era's less glamorous requirements, which are real work and
easy to forget:

- **Legacy character encodings.** windows-1252, ISO-8859-*, Shift_JIS, GB2312,
  EUC-KR. Pages of this era frequently declare the wrong one or none at all, so
  encoding sniffing is required, not optional. `encoding_rs` handles this.
- **Framesets.** `<frameset>` and `<frame>` were everywhere. A period-correct
  browser needs them; a modern one would not.
- **Animated GIFs**, and only the era's image formats: GIF, JPEG, PNG.
- **Plain HTTP.** Much of the surviving old web is http:// only. Needs a
  deliberate policy (§4), not an accident.

One consequence worth stating: this is a browser for reading the web, current
and archived, as documents. Modern web *applications* are out of scope by
construction — which is the same statement as "no JavaScript," arrived at from
the other direction.

---

## 3. What we write vs. what we take

A from-scratch engine does not mean from-scratch everything. Some areas are
tarpits with no product upside, and are where CVEs live:

**Do not write these.** Hostile-input parsing and text are solved problems that
punish amateurs:

- **HTML parsing** — the spec *is* an error-recovery algorithm, not a grammar.
  Use `html5ever`. Note the modern parser is *correct* for old content: the
  HTML5 parsing algorithm was reverse-engineered from how browsers already
  handled exactly this era's markup.
- **CSS tokenisation** — likewise spec-defined. Use `cssparser`.
- **Character encoding detection and decoding** — `encoding_rs`.
- **TLS** — never. `rustls`.
- **Font shaping, line breaking, bidi** — the single hardest part of a renderer.
  Use `cosmic-text` (which layers `rustybuzz` + `swash` + Unicode line breaking).
- **Image decoding** — `image`, restricted to GIF, JPEG, and PNG.

**Do write these.** This is the actual project:

- DOM tree (arena-allocated, index-based — not `Rc<RefCell<…>>`)
- CSS 2.1 cascade: selector matching, specificity, inheritance, computed values
- Box tree and layout: block, inline, floats, tables, positioning, quirks mode
- Display list and painting
- Resource loading, cache, and network *policy*
- Browser chrome and input handling
- The slop layer

**Language: Rust.** This program parses hostile bytes from the open internet as
its entire job. Memory safety is not a preference here. Rust also has the
specific crates above, which is not true of the alternatives.

**Rendering: CPU-first**, via `tiny-skia` into a `softbuffer` surface on `winit`.
GPU rasterisation is a later optimisation, not a dependency. "Fast on a
ten-year-old laptop" is the constraint, and that laptop's GPU drivers are the
least reliable thing about it.

### Cross-platform rendering is nearly free, and we should exploit it

Linux, macOS, and Windows are all first-class from M1, with reference tests
running on all three in CI.

Normally that would triple the pixel-baseline maintenance burden, because
platform font rasterisers disagree about essentially every glyph. It does not
here: we shape with `rustybuzz` and rasterise with `tiny-skia` on the CPU, so if
we **bundle our own fonts and never touch the system font rasteriser**, output
is deterministic across platforms. One set of baseline images, valid everywhere.

This is a deliberate constraint with a real cost — the browser will not use your
system fonts, and will not look native — and it buys correctness testing on all
three platforms for the price of one. For a document renderer that is the right
trade. Recorded as ADR-0005.

**This is now measured rather than argued.** As of `986e602` all three
platforms rendered the shared baseline set byte for byte in CI, macOS included.
That was the assumption the whole approach rested on and the one that would
have been expensive to discover was false; it holds.

Which fonts to bundle is ADR-0008: metric-compatible substitutes for the faces
pages actually name (Liberation Sans/Serif/Mono for Arial, Times New Roman, and
Courier New; Gelasio for Georgia), backed by Noto for pan-Unicode coverage
including CJK and colour emoji. Metric compatibility matters more than it
sounds: substituting different advance widths changes line breaking, and
therefore layout. Coverage matters more still — without it, pages in most of the
world's scripts render as tofu.

---

## 4. Policy defaults

These are the "without the slop" decisions that live in code rather than UI:

| Default | Rationale |
| --- | --- |
| **Zero third-party requests** | One policy rule eliminates essentially all advertising and tracking, with no filter lists, no subscriptions, and no update treadmill. The highest-leverage line of code in the project. |
| **First-party, session-only cookies** | Persistent cross-site identity is the mechanism the slop economy runs on. |
| **No JS** | §1. |
| **Plain HTTP allowed, clearly marked** | Much of the old web is http:// only; refusing it would gut the browser's purpose. But it is unauthenticated and tamperable, and the chrome must say so plainly rather than hiding it. Never silently upgrade or silently downgrade. |

### Budgets

Resource weight is one of the four goals, so these are **enforced in CI as
failing tests**, not aspirations in a README:

| Budget | Target | Measured by |
| --- | --- | --- |
| Release binary size | ≤ 20 MiB | CI check on artifact |
| Bundled font payload | ≤ 64 MiB | CI check on artifact |
| Total distribution | ≤ 84 MiB | CI check on artifact |
| Cold start to first paint | ≤ 150 ms | benchmark on reference page |
| RSS rendering a Wikipedia article | ≤ 100 MB | instrumented run |
| Third-party network requests | 0 | network policy test |

Numbers are first drafts. The point is that they exist, are measured, and
regressions break the build.

The font payload has its own budget rather than counting against the binary
(ADR-0008). Real Unicode coverage — CJK and colour emoji especially — costs tens
of megabytes, and trimming it to fit a smaller number would render much of the
web as tofu boxes. Fonts are memory-mapped and load lazily, so the payload costs
install size rather than RAM or startup time. Install size growing roughly
fourfold is the accepted cost, recorded rather than glossed.

---

## 5. Repository shape

```
crates/
  net/      fetch, HTTP cache, TLS, cookie policy, request policy
  dom/      arena tree, html5ever integration, encoding sniffing
  css/      CSS 2.1 parsing, selector matching, cascade, computed style
  layout/   box tree; block, inline, float, table, positioned, quirks
  paint/    display list, tiny-skia rasterisation
  text/     shaping and line-breaking wrapper over cosmic-text
  shell/    window, chrome, tabs, input, navigation
  slop/     filtering, reader mode, content heuristics
fonts/      bundled fonts (see §3 — determinism depends on these)
tests/
  ref/      reference tests: render → PNG → compare against expected
  css21/    the official CSS 2.1 test suite, tracked as a pass-rate metric
  budgets/  size, memory, and startup budget enforcement
docs/adr/   architecture decision records
```

Reference-test infrastructure lands in **M1**, not later. A renderer without
pixel regression tests rots silently, and retrofitting them is much harder than
starting with them.

The CSS 2.1 test suite is the project's north-star metric — a frozen target with
a published pass rate. Track the number from the first milestone that can pass
any of it, and the roadmap becomes measurable rather than vibes.

---

## 6. Milestones

Each milestone ends in something you can run and look at.

### M0 — Foundation
Cargo workspace; CI on Linux, macOS, and Windows; ADR directory; budget harness
skeleton. ADRs for the decisions in this document, in particular no-JS, CSS 2.1
as the scope boundary, and bundled-fonts-for-determinism.
*Done when:* CI is green on all three platforms and budgets are measurable.

### M1 — It renders a document
HTTP(S) fetch → `html5ever` → DOM → a small CSS subset → block layout → text via
`cosmic-text` → `tiny-skia` → a window. Reference-test harness running on all
three platforms against a single shared baseline set.
*Done when:* a hand-written HTML page and `example.com` render recognisably, and
the three platforms produce identical pixels.

**Status: done.** `2kbrowser render https://example.com` produces a correct
page, the three platforms agree pixel for pixel in CI, and `2kbrowser open`
shows it in a scrollable window (verified by hand; CI has no display).

The gap it carried into M2 — inline content shaped as a single styled run — is
now closed: see M2 item 2 below.

### M2 — It renders the era's web
The bulk of the engine work, ordered by how much of the 2000s web each unlocks:

1. Real cascade and selector matching; full box model; backgrounds and
   **borders** — *done.*
2. **Inline layout with correct line breaking** — *done.* Differently-styled
   spans share line boxes, break as one paragraph, carry their own colour and
   size, and collapse whitespace across run boundaries.
3. **Tables** — *mostly done.* Automatic column sizing from cell content,
   `colspan`, row groups, and declared widths. Missing: `rowspan`, collapsed
   borders, fixed layout, `border-spacing` parsing
4. **Floats** — *done.* Placement on both sides, stacking, line boxes that
   narrow beside them, `clear`, and containers that enclose their floats
5. **Images** — *done.* Fetched, decoded, sized from intrinsic or declared
   dimensions, floatable. Links, scrolling, and hit testing remain
6. **Positioned layout** — *done.* Relative shifts, absolute placement against
   the nearest positioned ancestor, `top`/`right`/`bottom`/`left`, shrink-to-fit
   widths. **Quirks mode** — *started;* unitless lengths and hash-less hex
   colours parse, other quirks outstanding. **Framesets** — *done*

*Done when:* a Wikipedia article, a typical blog, Hacker News, and a handful of
Internet Archive captures from ~2000 are pleasant to read. This milestone takes
longer than all the others combined; expect the schedule to be dominated by
items 2 and 3.

### M3 — Browser chrome, and the document fallback
Tabs, URL bar, history, back/forward, bookmarks, find-in-page. Keyboard-first.
This is where "without the slop" becomes visible as UX rather than as an
absence — no sponsored tiles, no feed, no account, no onboarding. The HTTP
transparency requirement from §4 lands here.

**Reader mode also lands here rather than in M5** (ADR-0009), because it is what
makes the browser work on the modern web at all. When a page's layout depends on
features we do not implement, the engine detects that during cascade and
re-renders the page as a document instead of producing a layout it knows to be
wrong — telling the user it did so, with a control to force the raw layout.
*Done when:* it is the browser you reach for to read something, and modern pages
are readable rather than jumbled.

### M4 — Hardening
Process/sandbox model, continuous fuzzing of the HTML, CSS, and image-decode
paths, TLS configuration review, `AccessKit` integration for screen readers.
*Done when:* we are willing to tell a stranger to browse untrusted sites with it.
**Until M4 lands, this is a tool for its authors, and the README should say so.**

### M5 — The slop layer
Content-quality signals surfaced in the UI. Optional community blocklists for
content farms. (Reader mode moved to M3 — see ADR-0009.)

---

## 7. The AI-slop problem

This is the one goal the architecture does not give us for free, and it is worth
being blunt: **reliable detection of AI-generated text does not currently exist.**
Published classifiers have false-positive rates that would be unacceptable in a
browser — wrongly flagging a human author is a real harm, and doing it silently
is worse.

So the plan deliberately avoids claiming a classifier. Instead, in order of
increasing risk:

1. **Structural heuristics, shown transparently** — boilerplate-to-content
   ratio, ad-slot density, template churn, publish-date patterns. These measure
   *content-farm mechanics*, not authorship, and they are explainable to the user.
2. **Community blocklists** — content farms are a small, well-known, slow-moving
   set. A curated list outperforms any on-device classifier and is auditable.
3. **On-device scoring** — only if 1 and 2 prove insufficient, and only as a
   visible signal the user can inspect and overrule. Never a silent block.

The governing rule: **surface a signal, never silently hide a page.** The user
decides; the browser explains its reasoning.

---

## 8. Risks worth stating plainly

- **The scope boundary will be under constant pressure.** CSS 2.1 gives us a
  finish line only if we defend it. Every "just add flexbox" is the compat
  treadmill coming back. If the goal quietly becomes "render the modern web,"
  the project has failed and should be a Chromium shell instead. This is the
  risk most likely to actually kill the project, because it kills it pleasantly.
- **Modern sites will look wrong, not just plain** — *mitigated by ADR-0009.*
  Without JS and without flexbox or grid, many current pages would render as
  jumbled boxes, which reads as a broken browser rather than a deliberate one.
  The engine now detects that its layout would be wrong and re-renders the page
  as a document instead. The residual risk is the detection threshold: pages
  near it will flip between modes, so it needs a corpus behind it rather than a
  guess, and the user needs the override.
- **M2 is most of the work.** Inline text layout and table layout are where
  engine projects stall. Table layout in particular is far more intricate than
  it looks.
- **Security is not free from Rust.** Rust removes memory-safety bugs, not logic
  bugs, not same-origin mistakes, not resource exhaustion. M4 is not optional
  before recommending this to anyone.
- **Accessibility is a correctness requirement**, not a feature. A browser
  without screen-reader support is not a browser. `AccessKit` from M4, and the
  DOM should be designed so retrofitting it is not painful.
- **Solo-maintainer risk.** Every dependency avoided is code we maintain
  forever. The "do not write these" list in §3 is the main defence.

---

## 9. Open questions

Resolved: the meaning of "2k" (§2), the JavaScript decision (§1), platform
priority (§3), and which fonts to bundle (§3, ADR-0008).

Everything still open is tracked as a GitHub issue rather than listed here, so
that it has one home and can be closed:

| # | Question | Blocks |
| --- | --- | --- |
| [1](https://github.com/hype-armor/2kbrowser/issues/1) | Period-authentic chrome, or a modern shell? | M3 |
| ~~[2](https://github.com/hype-armor/2kbrowser/issues/2)~~ | ~~Move reader mode earlier than M5?~~ Resolved: yes, M3 (ADR-0009) | — |
| [3](https://github.com/hype-armor/2kbrowser/issues/3) | Legacy TLS: marked downgrade, or unreachable? | M4 |
| [4](https://github.com/hype-armor/2kbrowser/issues/4) | Revisit dependency posture? | nothing |
| [5](https://github.com/hype-armor/2kbrowser/issues/5) | No metric-compatible clone for Verdana/Tahoma | nothing |
| [6](https://github.com/hype-armor/2kbrowser/issues/6) | `cursive`/`fantasy` resolve to sans-serif | nothing |
| [7](https://github.com/hype-armor/2kbrowser/issues/7) | Vendor fonts in git, or fetch with pinned checksums? | M1 |
| [8](https://github.com/hype-armor/2kbrowser/issues/8) | Per-script optional font payloads | release |

None of these block starting M1 except #7, which only needs answering when the
text stack lands rather than at the start of the milestone.

---

## 10. Immediate next step

M0 is a few hours: workspace, three-platform CI, ADRs, budget harness. It
commits to nothing beyond the language choice and makes M1 startable.

None of the open questions in §9 block it.
