# 2kbrowser — Plan

> A web browser without the slop.

Status: **under construction**. M0 through M3 are done: the engine renders the
era's HTML and CSS, the three platforms agree pixel for pixel, and there is a
browser around it — tabs, history, a URL bar, find-in-page, bookmarks, and the
document fallback. M4 is under way — fuzzing, the process boundary, a confined
renderer on Linux and Windows, and the TLS posture — and until it lands this is
a tool for its authors. See §6 for what each milestone covers and what is known
to be missing.

This document started as a proposal and is now also the record of what was
decided and why. Decisions that would be expensive to revisit are in
`docs/adr/` instead, one file each, and those are immutable — where this
document and an ADR disagree, the ADR is what was actually decided.

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
- **TLS** — never. `rustls`, and only ever `rustls`: TLS 1.0 and 1.1 are refused
  outright rather than offered behind a downgrade (ADR-0013). The engine is
  period; everything protecting it is present-day.
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

**The rasterising half of this is measured rather than argued:** all three
platforms produce byte-identical *glyphs* from the same input, macOS included.
That was the assumption the whole approach rested on and the expensive one to
discover false, and it holds.

The other half — everything around the renderer — needed the same scrutiny and
did not get it soon enough. Windows failed the image and frameset fixtures for
several commits before anyone looked, because `file://D:\a\page.html` has no
`/` in it and every relative subresource resolved to the root. Same input, a
different page on one platform, which is exactly what these tests exist to
catch. The lesson is that a green run is only evidence about the fixtures that
existed when it ran: the run this claim was first written from predated the
fixtures that load anything from disk.

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
3. **Tables** — *done.* Automatic column sizing from cell content, `colspan`
   and `rowspan`, row groups, declared widths, shrink-to-fit boxes, row
   backgrounds, and `border-spacing` including the `cellspacing` attribute.
   Missing: collapsed borders and fixed layout
4. **Floats** — *done.* Placement on both sides, stacking, line boxes that
   narrow beside them, `clear`, and containers that enclose their floats
5. **Images** — *done.* Fetched, decoded, sized from intrinsic or declared
   dimensions, floatable, and sitting *on* a line rather than interrupting it.
   Links, scrolling, and hit testing remain
6. **Positioned layout** — *done.* Relative shifts, absolute placement against
   the nearest positioned ancestor, `top`/`right`/`bottom`/`left`, shrink-to-fit
   widths. **Quirks mode** — *started;* unitless lengths and hash-less hex
   colours parse, other quirks outstanding. **Framesets** — *done*
7. **Presentational attributes** — *done.* `bgcolor`, `text`, `link`, `align`,
   `valign`, `hspace`/`vspace`, `<font>`, `background`, and the table
   attributes, at their own cascade origin between the UA sheet and author
   CSS. The era's markup keeps most of its styling here rather than in CSS, so
   without this these pages render as unstyled text
8. **Lists, decorations, rules, and forced breaks** — *done.* Markers with
   `<ol start>` and `<li value>`, `text-decoration` propagated per §16.3,
   underlined links via attribute selectors, `<hr>`, and `<br>`
9. **Tiled backgrounds** — *done.* `background-image`, `background-repeat`, the
   `background` shorthand and its reset, `<body background>`, and canvas
   propagation
10. **Stylesheets from elsewhere** — *done.* `<link rel=stylesheet>`, `@import`
   chains in the order CSS specifies, and `@media` blocks by media type.
   Feature queries are CSS 3 and do not apply
11. **Legacy character encodings** — *done.* Byte-order mark, `Content-Type`,
   a `<meta>` prescan, then windows-1252 — which most of the surviving old web
   needs and which the plan called required rather than optional

12. **Intrinsic sizing over whole subtrees** — *done.* A cell holding a nested
   table, an image, or a block is measured by what is inside it rather than by
   its text; declared column widths hold rather than being stretched; auto
   margins centre a block; and an inline element wrapping a block one still
   lays that block out

Known-wrong and recorded rather than hidden: collapsed borders, fixed table
layout, dashed and dotted borders painting solid, `background-position`, and
proper block-in-inline splitting — an inline element containing a block is
laid out as a block instead, which matches for the shapes that occur but is
not what CSS 2.1 §9.2.1.1 describes.

*Done when:* a Wikipedia article, a typical blog, Hacker News, and a handful of
Internet Archive captures from ~2000 are pleasant to read. This milestone takes
longer than all the others combined; expect the schedule to be dominated by
items 2 and 3.

### M3 — Browser chrome, and the document fallback
Tabs, URL bar, history, back/forward, bookmarks, find-in-page. Keyboard-first.
This is where "without the slop" becomes visible as UX rather than as an
absence — no sponsored tiles, no feed, no account, no onboarding. The HTTP
transparency requirement from §4 lands here.

**The chrome is modern; only the engine is period** (ADR-0011). The engine's
constraints are load-bearing — refusing JavaScript is what removes the slop —
and a period tab strip would remove nothing while costing the user fluency.
Restraint shows as absence, which is the point of §1, and does not need bevels.
The era shows through in the viewport and nowhere else.

Before any of the chrome, two pieces of engine work it all rests on: hit
testing — turning a point into the element under it — and link geometry, so a
link has a rectangle to click. Neither depends on what the chrome looks like.

**Reader mode also lands here rather than in M5** (ADR-0009), because it is what
makes the browser work on the modern web at all. When a page's layout depends on
features we do not implement, the engine detects that during cascade and
re-renders the page as a document instead of producing a layout it knows to be
wrong — telling the user it did so, with a control to force the raw layout.
*Done when:* it is the browser you reach for to read something, and modern pages
are readable rather than jumbled.

**Status: done.** Hit testing and link geometry first, then the chrome on top of
them: back and forward with a real history stack, an editable URL bar,
find-in-page, tabs with a strip that only appears once there are two, the
document-fallback notice and its override, the HTTP-transparency marker §4
requires, and bookmarks in a text file. The chrome is drawn by building a
display list and handing it to the same rasteriser the page goes through, so it
is not a second rendering path that can drift — and so it is tested headlessly,
which is where nearly all of its coverage comes from. What is *not* tested is
the event loop itself: CI has no display server, so key and pointer handling are
exercised by hand and a regression in them would not be caught by `cargo test`.

### M4 — Hardening
Process/sandbox model, continuous fuzzing of the HTML, CSS, and image-decode
paths, TLS configuration review.
*Done when:* we are willing to tell a stranger to browse untrusted sites with it.

Screen-reader support was originally listed here and is **deferred to after
M4** — [issue #9](https://github.com/hype-armor/2kbrowser/issues/9). Not because
it matters less: because ADR-0012 landed first and moved the semantic tree to
the far side of a process boundary that deliberately carries only pixels.
Getting it across is a security design of its own, and doing it while the
boundary is still being built would mean doing it twice. It is also the only
item in this milestone that is not itself security work, which is what made it
the one to move.
**Until M4 lands, this is a tool for its authors, and the README should say so.**

**Fuzzing — done, and it found things.** `tests/fuzz` mutates the reference
fixtures into the HTML parser, the CSS parser, image decoding, URL parsing, and
the whole render pipeline. Written here rather than taken from `cargo-fuzz`:
that needs nightly and `rust-toolchain.toml` pins stable, and its great strength
— finding memory-unsafety — is not in play under ADR-0002. What is reachable
here is panics, hangs, and unbounded allocation, and a mutator finds those with
no instrumentation and no new dependency. It is a dumb mutator, not a
coverage-guided one, and will not find a bug behind a magic constant it has to
guess. `cargo test` runs a short fixed-seed pass; `cargo run -p fuzz` soaks.

Three panics in the first soak, all reachable from an ordinary stylesheet, all
fixed:

- `font-size: 0` — legal CSS and a common idiom, since it is how the gap
  between inline-blocks gets closed. Our line height derives from the font size
  and cosmic-text asserts a line height is never zero, so a stylesheet could
  stop the browser.
- `font-size: 99999px` — the outline rasteriser allocates a bitmap proportional
  to the em square and panicked rather than refusing. Glyph size is now capped.
- `margin: 1e40px` and its neighbours — `1e40` does not fit in an `f32`, so it
  computes to infinity, saturates the cast to `i32::MAX`, and panicked inside
  tiny-skia on the next addition. Geometry outside a drawable range is now
  skipped.

A later soak found a fourth, and it is worth recording because it is the same
family one layer in — which is what a corpus that keeps its findings is for. The
drawable-range check was on the *text origin*, and the origin is not where a
glyph lands: `glyph.x` and `glyph.y` are offsets within the run, so a stylesheet
that shifts a run far enough puts an in-range origin arbitrarily far from an
out-of-range glyph, and tiny-skia panics building the `i32` rectangle for it.
The check now sits at the position actually drawn at, and the sum that overflows
— `x + width` — is checked rather than assumed. Found by mutating the fixture
written for the first three.

**Known and not fixed: layout is slow on pathological input.** The fuzzer's
worst render is about 11x the slowest real fixture — a 9 KB document at 99 ms
in release, against a few ms for a normal page of that size. It is linear
rather than quadratic, so it is a poor constant rather than an algorithmic
hole, and it comes from long unbroken runs of characters inside nested tables
being re-measured once per table level. Not a denial of service on the evidence
so far, and not scheduled.

**The fuzzer cannot catch a true hang.** It times each input after the fact, so
something that never returns hangs the harness rather than being reported. The
renderer process below can be killed, which is where that gap closes for the
browser; the fuzzer itself still runs in-process and still cannot be
interrupted.

**Process model — decided and half-built.** [ADR-0012](docs/adr/0012-process-isolation.md):
the parent keeps the chrome, the network, and the disk; a child parses, lays
out, and rasterises. The reason is counted rather than assumed — roughly 460
`unsafe` sites sit between a hostile document and the process, in the libraries
ADR-0007 deliberately chose not to write here. "We forbid `unsafe`" describes
our discipline, not our attack surface.

Done: the boundary itself. `crates/sandbox` is the transport — spawning, a
length-prefixed protocol, the request/response conversation, and killing a child
that hangs or dies. It is generic over the rendering, which is what keeps it
below `shell`. A page rendered across a process is byte-identical to one
rendered without, proven against the real binary rather than a stub, and the
protocol is fuzzed like every other parser because a compromised renderer's last
move is to send the parent something malformed.

Also done since: **every subresource crosses the pipe** — images, stylesheets,
`@import` chains, and frames are requests the parent decides on, so ADR-0006's
policy is enforced where a compromised renderer cannot reach it. And **the
window renders out of process**, holding a `Viewport` over a live child rather
than a `Page`. So do `render` and `links`, which did not until later and were
the gap that mattered most in practice: `2kbrowser render https://example.com`
is the documented way to point this at a stranger's page, and it was handing
their HTML to the parsers in the process holding the network and the disk. A page rendered across the boundary is byte-identical to one
rendered without: verified on the era fixture, 0 differing bytes of 2.5 MB.

Two consequences worth recording rather than discovering later:

- **A page taller than one frame is clipped.** The canvas covers the whole
  document so scrolling costs a blit rather than a re-layout, and a frame is
  bounded so a compromised child cannot make the parent allocate without limit.
  At 800 pixels wide that allows roughly 20,000 rows. The honest fix is to
  render a band around the scroll position instead of the whole document, which
  changes how scrolling works and is not something to fold into the boundary.
- **Find and resize keep the child alive.** One *page* per process, not one
  message: a page's lifetime includes the questions asked of it while it is on
  screen, and the text those questions search never crosses. The child is killed
  when the page is replaced, which `Session`'s `Drop` makes a property of the
  type rather than of every caller remembering.

Not done, and the README must keep saying so:

**The syscall filter is on, on Linux.** The renderer applies a seccomp-bpf
filter before it reads its first frame — that frame carries the document, so it
is already attacker-influenced. No sockets, no opening files, no starting or
attaching to processes. Proven from inside: the binary confines itself and
reports, and the era fixture still renders byte-identically with all three
images arriving over the pipe.

It is a **denylist**, and that is a real weakening worth stating. An allowlist
refuses anything nobody named, including a syscall nobody thought about; it is
also what breaks a browser in the field, because the set a renderer touches is
decided by the allocator, the shaper, and the standard library and moves under
you on a toolchain bump. Denied calls return `EPERM` rather than killing the
process, so a legitimate call that meets the filter degrades into an error the
renderer already handles. An allowlist is the stronger end state and wants a
measured set of what the renderer actually uses.

**Windows is confined too, by a different mechanism.** seccomp is
self-restriction; an AppContainer is not. The parent creates a package profile,
builds a `SECURITY_CAPABILITIES` structure, and attaches it to `CreateProcess`
through `STARTUPINFOEX` — there is no call a running process can make to put
itself in one. So on Windows the confinement lives next to the spawn, in
`crates/sandbox/src/contain.rs`, and `confine::apply()` correctly does nothing.

The container is built with **no capabilities at all**, which makes it the
stronger of the two: capabilities are the holes deliberately left in an
AppContainer, and the renderer needs none because every resource it wants is a
request the parent answers. Reached without writing any `unsafe`, through a
pinned dependency rather than an ADR-0002 exception —
[ADR-0014](docs/adr/0014-appcontainer-through-a-dependency.md) records that
trade and does not pretend it is free.

The self-test had to change shape for it. On Windows the process that confines
and the process that is confined cannot be the same one, so it builds a
container and runs the probes inside. Two versions of that check were wrong
before Windows CI caught them, both in the same way — a check that could not
fail, dressed as a check that passed. The file probe used `temp_dir()` computed
independently on each side, and an AppContainer *redirects* the temp directory.
The network probe treated `ConnectionRefused` as proof, which is sound for
seccomp (it fails the syscall) and wrong for AppContainer (the firewall resets
the connection, so blocked and dead are indistinguishable). It now connects to a
listener the parent binds, and both sides echo what they aimed at.

Still not done:

1. **macOS is unconfined.** The App Sandbox is not implemented, and it is not
   stubbed to look done — the parent reports `Unavailable` and says so on
   stderr. A sandbox that claims to work and does not is worse than one that
   says it is missing. Unlike Windows there is no third way round ADR-0002:
   Seatbelt has no safe wrapper on crates.io, only raw FFI bindings, so it needs
   either a lint exception or a quarantined crate — and neither of us can test
   it. See [issue #4](https://github.com/hype-armor/2kbrowser/issues/4).
2. **Landlock is not used.** It would add filesystem confinement beyond
   "cannot call `open`", and `landlock_create_ruleset` returns `ENOSYS` on the
   kernel this was developed against, so it could not be tested. Filesystem
   access is denied at the syscall level instead, which covers opening but not
   every path to a descriptor.
3. **Only loopback is probed on Windows.** Loopback and outbound are separate
   AppContainer rules, and reaching the internet from a test is not something to
   depend on. What rules out outbound is the capability set being empty, which
   is asserted directly instead.

**A legacy-TLS refusal is legible — done.** ADR-0013 refuses TLS 1.0 and 1.1
outright, and until this landed the chrome showed that as an ordinary network
error, so an honest refusal read as a bug. The bar now says "refused: this
site's TLS is too old — needs 1.2 or newer", and a bad certificate says
something different, because the two mean opposite things to a reader.
Classified from the structured `rustls` error rather than from message text, so
it does not break when a `Display` impl nobody promised us changes. Tested end
to end without needing `openssl` on the machine: what an old server sends is a
seven-byte fatal `protocol_version` alert, so the test sends exactly those bytes.

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
  without screen-reader support is not a browser. It is now
  [issue #9](https://github.com/hype-armor/2kbrowser/issues/9), deferred to
  after M4 rather than dropped — ADR-0012 moved the semantic tree to the far
  side of a process boundary that carries only pixels, so getting it across is
  a security design of its own. The risk this bullet names is *exactly* the one
  that materialised: a milestone moved and accessibility was the thing that
  slipped. It is filed so that slipping again has to be a decision.
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
| ~~[1](https://github.com/hype-armor/2kbrowser/issues/1)~~ | ~~Period-authentic chrome, or a modern shell?~~ Resolved: a modern shell around a period engine (ADR-0011) | — |
| ~~[2](https://github.com/hype-armor/2kbrowser/issues/2)~~ | ~~Move reader mode earlier than M5?~~ Resolved: yes, M3 (ADR-0009) | — |
| ~~[3](https://github.com/hype-armor/2kbrowser/issues/3)~~ | ~~Legacy TLS: marked downgrade, or unreachable?~~ Resolved: unreachable — refused outright (ADR-0013) | — |
| [4](https://github.com/hype-armor/2kbrowser/issues/4) | Revisit dependency posture? | nothing |
| ~~[5](https://github.com/hype-armor/2kbrowser/issues/5)~~ | ~~No metric-compatible clone for Verdana/Tahoma~~ Resolved: accept the reflow | — |
| ~~[6](https://github.com/hype-armor/2kbrowser/issues/6)~~ | ~~`cursive`/`fantasy` resolve to sans-serif~~ Resolved: fall back, do not chase it | — |
| ~~[7](https://github.com/hype-armor/2kbrowser/issues/7)~~ | ~~Vendor fonts in git, or fetch with pinned checksums?~~ Resolved: fetch with pins (ADR-0010) | — |
| [8](https://github.com/hype-armor/2kbrowser/issues/8) | Per-script optional font payloads | release |
| [9](https://github.com/hype-armor/2kbrowser/issues/9) | Screen-reader support across the renderer boundary | after M4 |

None of these block starting M1 except #7, which only needs answering when the
text stack lands rather than at the start of the milestone.

---

## 10. Immediate next step

M4, hardening. The browser is now usable enough that the honest next question is
whether it is safe to point at something you did not write, and the answer is
no: there is no sandbox, the parsers have never been fuzzed, and the TLS
configuration has not been reviewed. Issue #3 blocks part of it; nothing else
in §9 does.

The engine's known gaps — collapsed borders, fixed table layout, dashed and
dotted borders, `background-position`, proper block-in-inline splitting — are
listed under M2 and are not scheduled. They are places the browser is wrong
rather than places it falls over, and none of them is what makes this unsafe.
