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

**2kbrowser renders HTML and CSS. It does not execute JavaScript.**

That is not a limitation we plan to grow out of. It is the product. It makes
all four kinds of slop fall out of one decision:

| Slop | How the no-JS engine addresses it |
| --- | --- |
| **Browser bloat** | No JS engine means no JIT, no sync account, no extension host, no AI sidebar, no sponsored tiles. There is nowhere to put the bloat. |
| **Page-level junk** | Consent modals, popups, autoplay, and infinite scroll are script-driven. They simply do not run. |
| **Resource weight** | No JIT, no JS heap, CPU rasterisation. Tens of MB of RAM, not hundreds. |
| **AI-generated content** | Not solved by no-JS — needs its own layer (§6). It is the hardest of the four and is deliberately scheduled last. |

The sites that break without JS are, to a striking degree, the same sites that
generate the slop. Meanwhile Wikipedia, MDN, most documentation, most blogs,
most static-site-generator output, HN, lobste.rs, and much news either work
unmodified or degrade to readable HTML. **The constraint is the filter.**

### Why not "a tiny JS subset"

The middle path is worse than either end. Sites feature-detect. A browser that
runs *some* JS advertises that it runs JS, gets served the script path, and then
fails in confusing, silent, page-specific ways. A browser that runs none gets
served `<noscript>` content and static fallbacks, which is the behaviour we
actually want. Partial JS support is the one option that gets the downsides of
both.

If JS becomes necessary, the right shape is an explicit, per-site, off-by-default
escape hatch backed by a real engine — not a homegrown subset. See M6.

---

## 2. What we write vs. what we take

A from-scratch engine does not mean from-scratch everything. Three areas are
tarpits with no product upside, and are where CVEs live:

**Do not write these.** Hostile-input parsing and text are solved problems that
punish amateurs:

- **HTML parsing** — the spec *is* an error-recovery algorithm, not a grammar.
  Use `html5ever`.
- **CSS tokenisation** — likewise spec-defined. Use `cssparser`.
- **TLS** — never. `rustls`.
- **Font shaping, line breaking, bidi** — the single hardest part of a renderer.
  Use `cosmic-text` (which layers `rustybuzz` + `swash` + Unicode line breaking).
- **Image decoding** — `image`, and support fewer formats rather than more.

**Do write these.** This is the actual project:

- DOM tree (arena-allocated, index-based — not `Rc<RefCell<…>>`)
- CSS cascade: selector matching, specificity, inheritance, computed values
- Box tree construction and layout: block, inline, then tables and flexbox
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

---

## 3. Budgets

"2k" is read here as a budget ethos: the browser should be small enough that its
size is a stated number rather than an emergent one. These are **enforced in CI
as failing tests**, not aspirations in a README:

| Budget | Target | Measured by |
| --- | --- | --- |
| Release binary size | ≤ 20 MB | CI check on artifact |
| Cold start to first paint | ≤ 150 ms | benchmark on reference page |
| RSS rendering a Wikipedia article | ≤ 100 MB | instrumented run |
| Third-party network requests | **0 by default** | network policy test |
| JIT / dynamic codegen | none | absence of a JS engine |

Numbers are first drafts — the point is that they exist, are measured, and
regressions break the build. If a budget needs to move, that is a deliberate,
reviewed change.

The zero-third-party-requests default deserves emphasis: **one policy rule
eliminates essentially all advertising and tracking**, with no filter lists, no
subscriptions, and no update treadmill. It is the highest-leverage line of code
in the project.

---

## 4. Repository shape

```
crates/
  net/      fetch, HTTP cache, TLS, cookie policy, request policy
  dom/      arena tree, html5ever integration
  css/      parsing, selector matching, cascade, computed style
  layout/   box tree; block, inline, table, flex
  paint/    display list, tiny-skia rasterisation
  text/     shaping and line-breaking wrapper over cosmic-text
  shell/    window, chrome, tabs, input, navigation
  slop/     filtering, reader mode, content heuristics
tests/
  ref/      reference tests: render → PNG → compare against expected
  budgets/  size, memory, and startup budget enforcement
docs/adr/   architecture decision records
```

Reference-test infrastructure lands in **M1**, not later. A renderer without
pixel regression tests rots silently, and retrofitting them is much harder than
starting with them. A curated subset of Web Platform Tests follows once layout
is real enough to pass any of them.

---

## 5. Milestones

Each milestone ends in something you can run and look at.

### M0 — Foundation
Cargo workspace, CI (build, test, clippy, fmt), ADR directory, budget harness
skeleton, and the decisions in this document recorded as ADRs.
*Done when:* CI is green on an empty workspace and budgets are measurable.

### M1 — It renders a document
HTTPS fetch → `html5ever` → DOM → a small CSS subset → block layout → text via
`cosmic-text` → `tiny-skia` → a window. Reference-test harness in place.
*Done when:* a hand-written HTML page and `example.com` render recognisably.

### M2 — It renders the readable web
The bulk of the engine work. Real cascade and selector matching, the full box
model, inline layout with correct line breaking, images, links, scrolling, hit
testing, backgrounds and borders, tables (the old web runs on them), then
flexbox.
*Done when:* a Wikipedia article, a typical blog, and Hacker News are pleasant
to read. This is the milestone that takes the longest by a wide margin.

### M3 — Browser chrome
Tabs, URL bar, history, back/forward, bookmarks, find-in-page. Keyboard-first.
This is where "without the slop" becomes visible as UX rather than as an
absence — no sponsored tiles, no feed, no account, no onboarding.
*Done when:* it is the browser you reach for to read something.

### M4 — Hardening
Cookie policy (first-party, session-only by default), a process/sandbox model,
continuous fuzzing of the HTML and CSS parsers, TLS configuration review,
`AccessKit` integration for screen readers.
*Done when:* we are willing to tell a stranger to browse untrusted sites with it.
**Until M4 lands, this is a tool for its authors, and the README should say so.**

### M5 — The slop layer
Reader mode as a first-class view. Content-quality signals surfaced in the UI.
Optional community blocklists for content farms.

### M6 — JS escape hatch (optional, and possibly never)
If and only if M1–M5 prove the no-JS thesis insufficient: embed QuickJS behind a
per-site, off-by-default toggle with a deliberately minimal DOM binding surface.
Revisited as a decision, not assumed as a roadmap item.

---

## 6. The AI-slop problem

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

## 7. Risks worth stating plainly

- **Web compatibility is a treadmill we will never win.** A from-scratch engine
  will not render the whole web, ever. This only works if incompatibility is
  positioned as intent rather than as a bug backlog. If the goal quietly becomes
  "render everything," the project has failed and should be a Chromium shell
  instead.
- **M2 is most of the work.** Layout and inline text are where engine projects
  stall. Expect the schedule to be dominated by it and plan the scope of tables
  and flexbox accordingly.
- **Security is not free from Rust.** Rust removes memory-safety bugs, not logic
  bugs, not same-origin mistakes, not resource exhaustion. M4 is not optional
  before recommending this to anyone.
- **Accessibility is a correctness requirement**, not a feature. A browser
  without screen-reader support is not a browser. `AccessKit` from M4, and the
  DOM should be designed so that retrofitting it is not painful.
- **Solo-maintainer risk.** Every dependency avoided is code we maintain
  forever. The "do not write these" list in §2 is the main defence.

---

## 8. Open questions

1. **What does "2k" mean?** §3 assumes a budget ethos. If it means 2000-era web
   aesthetics, table layout moves earlier and flexbox may be unnecessary. If it
   means a literal line-count budget, most of §5 needs rescoping.
2. **Confirm no-JS.** §1 argues against the "tiny subset" option. This is the
   single decision that most changes the project, and it should be explicit.
3. **Platform priority.** Linux-first is assumed. macOS and Windows are mostly a
   `winit` and packaging concern, but "first" determines where the polish goes.
4. **Dependency posture.** §2 takes a pragmatic line. A stricter
   near-zero-dependency stance is defensible but roughly doubles M1 and M2 and
   moves shaping and TLS in-house, which is not recommended.

---

## 9. Immediate next step

If the direction above is right, M0 is a few hours: workspace, CI, ADRs, budget
harness. It commits to nothing beyond the language choice and makes M1 startable.
