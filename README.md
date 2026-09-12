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

## What it looks like

Real pages, screenshotted from the running browser. Regenerate them with
`scripts/screenshots.sh`.

![Hacker News in 2kbrowser](docs/images/hacker-news.png)

Hacker News, today, with no JavaScript. Most of the web that is worth reading
looks like this without scripts — the constraint is the filter, not a
limitation to work around. The bar says nothing, because an ordinary page over
HTTPS has nothing to say.

![The first website in 2kbrowser](docs/images/first-website.png)

The first website, still up, still plain HTTP. The bar marks it *not encrypted*
— never the reverse, because decorating the secure case teaches people to look
for a signal whose absence is easy to miss (ADR-0006).

![A modern page re-rendered as a document](docs/images/document-fallback.png)

A page built with layout this engine does not implement. Rather than render it
as jumbled boxes and let you think the browser is broken, it says what it did
and offers you the author's layout anyway (ADR-0009).

![Every state of the chrome bar](docs/images/chrome.png)

Every state the bar can be in, drawn by `cargo run -p shell --example
chrome-strip` — which is also how it is reviewed, since a headless test can
compare pixels and a person cannot compare descriptions.

## Status

**M2 — it renders the era's web.** HTML is fetched over HTTP(S) or read from
disk, parsed into an arena DOM, cascaded through a CSS 2.1 subset, laid out,
shaped against bundled Liberation faces, and rasterised on the CPU.

Working: the cascade with selectors, specificity, and inheritance; the box
model with borders and backgrounds; inline layout with per-span styles and
Unicode line breaking; floats; tables with automatic column sizing, `colspan` and `rowspan`,
`cellspacing`, and **both border models** — including
`border-collapse: collapse`, where adjoining borders resolve into one line
centred on the grid line between them, which is what a Wikipedia infobox or
wikitable is built out of; table captions, which sit outside the table's
border box on whichever side `caption-side` names; `visibility`, where a
hidden box draws nothing and keeps every pixel of its room — and a span inside
it can still ask to be visible and come back out; `text-transform`,
`letter-spacing`, `text-indent` on a block's first line, `min-height` and
`max-height` — applied in the order §10.7 gives them, so a box asked for both
at once takes the minimum — `clip`, where every side of the `rect()` is an
offset from the box's top-left corner rather than an inset from the far edges,
the one thing about that property which is easy to get backwards — and
`z-index`, so that two overlapping positioned boxes land in the order their
author asked for rather than the order they happen to be written in; images,
including ones sitting in a line; `background-position`, including the
percentage form, which aligns a point on the image with the same point on the
box rather than offsetting from the corner; relative and absolute positioning;
framesets; quirks-mode value parsing; the presentational attributes the era's
markup actually used (`bgcolor`, `align`, `<font>`, `border`, and `width` on an
image — including `width="100%"`, which is how a page drew a rule across a
column or held a layout open with a spacer GIF); list markers;
text decorations; tiled background images; every CSS 2.1 border style —
dotted and dashed runs stretched to start and end flush with the corners rather
than leaving half a dash there, and `double`, `groove`, `ridge`, `inset` and
`outset` lit from above and to the left, which is what made the era's grey
buttons look like buttons; the `font` shorthand, which is how the era's
stylesheets actually set type — `font: bold 12px Arial` — and which until
recently parsed as nothing at all, taking the line height down with it;
generated content, where `::before` and `::after`
take a string or an `attr()` and bracket the element's own content — in both
spellings, since CSS 2.1 writes one colon and the era's pages use it;
external stylesheets, including
`@import` chains and `@media` blocks — width queries included, so a page's
desktop rules apply at a desktop width instead of being dropped; and legacy
character encodings, which most of the surviving old web needs — a page in
windows-1252 read as UTF-8 is replacement characters where every accented
letter and curly quote should be.

Rendering is deterministic across Linux, macOS, and Windows, checked by
reference tests against one shared baseline set — verified, not assumed: all
three have rendered it byte for byte in CI, and so has a fourth target,
aarch64 Linux, against the same baselines.

The window opens on a virtual display in CI and is checked to survive
(`scripts/smoke-window.sh`) — which catches a panic or a bad index, though not
"does it look right". Everything with a testable shape lives outside the event
loop, and the rendering it drives is covered by the reference tests.

The CSS 2.1 suite has been run against it: **2385 of 4821 reference tests pass,
49.5%**, with no panics across roughly ten thousand renders. That is an upper
bound rather than a score — a reftest passes when both sides look the same, and
an engine that ignores a property draws both sides the same way.
`cargo run --profile conformance -p conformance` does it; the suite is not
vendored.

The figure usually moves in small amounts and is worth reading that way.
Collapsed borders were worth **eight** tests, seven of them named for the thing
they test — and a rise of eight is the weakest evidence in this file, which is
why the reference tests below carry committed baselines instead.

It has twice moved by a lot, and both times the cause was the harness rather
than the engine. The first run reported 20.6% and was wrong three times over
before it was worth anything. Then 30.4% became 35.5% when the suite's
stylesheets started reaching the engine at all: almost every test here is
XHTML, wrapping its CSS in `<![CDATA[ … ]]>`, and read as HTML — which is how
this browser reads everything, and how every browser read XHTML served as
`text/html` — that wrapper makes CSS error recovery swallow the whole
stylesheet. Chromium does the same with the same bytes; it differs only because
a `.xht` file off disk sends it down its XML parser.

The half of that worth dwelling on is not the 590 tests that started passing.
It is the **342 that started failing** — pairs with the wrapper on *both* sides,
where two lost stylesheets left two pages of unstyled prose matching each other
perfectly. They had been counted green from the beginning. A reftest cannot
tell "identical" from "identically blank", so nothing here would ever have
found them; they turned up only because a fix aimed at something else made them
move. See PLAN.md — the harness's own bugs have been more instructive than the
figure every time.

Known to be missing or wrong, rather than hidden: an inline box paints no
background, padding or border, so a highlighted phrase or a tinted `<code>`
comes out as plain text (issue #46) — "the box model with borders and
backgrounds" above means *block* boxes; `overflow` is understood
only for its effect on formatting contexts, and content that overflows a box is
not clipped — it is drawn, and the canvas is now grown to hold it, which it was
not until a `height: 0` box turned out to be losing its text off the bottom
edge; an invalid selector does not invalidate its rule, so
`[1digit], div { color: red }` styles the `div` where a browser would style
nothing; a caption wider than its table, which overhangs it rather than
widening a wrapper box this engine does not have, so the table sits a little
left of where a browser puts it; a float, which grows the block
that contains it instead of hanging out below its bottom edge as §10.6.3 says
(issue #41), so a container wraps its float where a browser lets it overhang;
`empty-cells`, which is parsed by nobody here and so is ignored in the
separated model where it applies — it is correctly ignored in the collapsing
one, where CSS 2.1 says it does not; where two collapsed borders *cross*, which
CSS 2.1 leaves undefined and which this engine settles by giving the corner to
the wider of them rather than mitring it diagonally as browsers do; fixed
table layout; and raising or lowering *text* off the baseline, so a `<sub>` or a
`<sup>` sits level with the words around it.

`inline-block` used to head this list. It is laid out now: sized by its own
content (§10.3.9), placed on the line as one atom, and hung from the baseline of
its own last line (§10.8.1), with `vertical-align` deciding where on the line it
hangs. It no longer counts as layout this engine cannot do, so a page built on
inline-blocks keeps the author's layout instead of falling back to a document
(ADR-0009). What is still missing around it: an inline box's own border and
padding take up no room on the line, so text after a bordered `<span>` does not
wrap where it should; and an `<iframe>` has no intrinsic size, so one with
`width: auto` comes out empty rather than 300x150.

A page can also fall back because its *frame* is unsupported while its prose is
not — a Wikipedia article is the case, where the text is ordinary flow and the
columns and navigation rows around it are not. No share can see that, since by
volume such a page is almost entirely correct, so a second signal counts the
containers that would have arranged a row of blocks and now stack them instead.
Every page in the corpus that should keep its author's layout scores zero of
those; a Wikipedia article scores sixteen.

**Forms are drawn but do not work.** Every control has a box now — text and
password fields, buttons, checkboxes, radios, `<textarea>`, `<select>` and the
rule around a `<fieldset>` — sized in the era's own units, since `size`, `cols`
and `rows` count characters and lines rather than pixels, and a field follows
the font it is set in. A password field shows bullets and never its value.
**Nothing can be typed into, clicked, or submitted**, and that is the stopping
point rather than an oversight: a control that draws correctly makes the page
read correctly, and interaction is separate work with a separate risk. Two
things a browser draws and this does not: a radio button is square, because the
rasteriser has rectangles and no rounded primitive, and a `<legend>` sits above
its group rather than breaking the rule it is written into.

And a list of properties that parse and are then ignored, which is longer than
this file used to admit: `word-spacing`, `font-variant`, `outline`,
`text-align: justify`, `list-style-position`, `list-style-image`,
`clip`, `position: fixed`, which behaves as `absolute` and so scrolls with the
page, the system font keywords (`font: menu` and its siblings), which name a
font of the host platform's that this engine has no way to ask for and so
leave the page's own styling standing, the second value of `border-spacing`, `direction` and everything else
about right-to-left text, and the parts of generated content that need state
this engine does not keep: `counter()`, `counters()`, `open-quote` and its
family, and `url()` in `content`. Each of those drops the whole declaration
rather than showing part of what the author asked for, which would look
deliberate.

Most of those were found by rendering era-typical markup beside a real browser
and comparing, which is worth recording because nothing already here could have
found them. The conformance suite cannot: a reftest passes when its two sides
render alike, and content missing from both sides still matches. The reference
baselines cannot either, since they only cover what somebody thought to write a
fixture for. Both are good at catching a rendering that *changed*; neither can
find one that was never there.

So the list above is now checked rather than remembered. `cargo run -p gaps`
renders each property twice — once with the declaration and once without — and
reports the ones that change nothing, alongside the elements that put nothing
on the canvas. Every row carries the answer this file claims, and CI fails when
they disagree in **either** direction: a property that starts working makes
this list untrue just as surely as one that stops, and the point is that the
list cannot go quietly stale again. It needs no browser, because "did this
declaration change any pixel" is not a question that needs one.

A document rendering also drops runs of line breaks, keeping one. Using
`<br><br><br><br>` as a margin is how a great deal of the era's markup did its
spacing, and how every WYSIWYG editor has done it since; reproduced faithfully
once the author's stylesheet has been thrown away, it turns a page into a
column of gaps with the occasional sentence in it. One break is kept, because a
single one between two lines is almost always meant — an address, a verse, a
signature. The author's own layout is left exactly as written: that is the
rendering where what they asked for is honoured.

Pages of any length work. The renderer paints a *band* — a few windows' worth
of rows around where you are reading — rather than the whole document, and the
rows ahead of you are asked for before you reach them, on a thread, so ordinary
scrolling never waits. The parse, the cascade, and the layout all stay done
between bands, so moving down a long page costs only the pixels. A band is
exactly the rows it names from a whole-page render, which is asserted at both
levels: against the rasteriser directly, and across a real process boundary.

Resizing the window is a different matter, because a resize changes the width
everything was laid out for and a band cannot help. A drag delivers a resize
per frame and laying a page out again costs several frames' worth, so the
events are recorded and acted on once the queue drains rather than serviced one
apiece. On a long page that is the difference between 97 renders and 4, and
between a window that answers again 28 seconds after the drag stops and one
that answers in under a second — measured, on a 119 KB page, by sending 96
resizes and timing how long until a click was noticed.

A render itself got cheaper too. Shaping text is most of what layout costs, and
a document asks for the same short strings over and over — the words in its
navigation, the labels in its tables, every *the* on the page — each of which
was shaped from nothing every time. A store now remembers what it has shaped,
which is safe here only because ADR-0005 already demands the engine be
deterministic: identical input must produce identical output, so remembering an
answer can change how long a page takes and cannot change how it looks. The
reference tests are what hold that, comparing rendered pages against baselines
byte for byte. Laying out Hacker News went from 37.7ms to 7.5ms, and rendering
it whole from 61.0ms to 28.2ms.

The parent also remembers what it has fetched, for as long as the page lasts,
so re-rendering no longer sends every stylesheet and image back to the network
— and one HTTP agent is kept for the process rather than built per request,
which is what makes a connection pool worth having.

Subresources are fetched several at a time rather than one after another. The
child asks for as many as it knows about at once — every image on the page,
once the cascade has run — and the parent fetches up to six of them together,
so a page waits for the longest of its round trips instead of the sum of them.
Against a server answering in 50ms: twenty-four images in 214ms rather than
1.23s. Six at a time and not more, because the bound is about the server at the
other end as much as about this process.

A stylesheet is still asked for on its own, and honestly cannot be otherwise:
an `@import` is only discoverable once the sheet importing it has arrived.

Links work in the window: click to follow, Alt+Left/Right or Backspace for
back and forward, and the cursor changes over a link. `2kbrowser links <page>`
prints every link rectangle and where it leads, and
`cargo run -p shell --example link-map` draws them over the page.

There is a chrome bar. It shows the URL, marks plain HTTP as unencrypted
(never the reverse — decorating the secure case teaches people to look for a
signal whose absence is easy to miss), and says when a page was re-rendered as
a document, with a control to overrule that and see the author's layout
(ADR-0009). An ordinary page over HTTPS says nothing at all.
`cargo run -p shell --example chrome-strip` draws every state of it.

That control goes both ways, and the second direction is not the first one
inverted. On a page sent to the fallback it says *as authored* and gives back
what the author wrote. On an ordinary page it says *simplify* and hands the
page the reader sheet anyway — which is a different request, not the absence of
the other one, because a page that renders fine has no fallback to return to.
That is what wanting a plain view of a busy but working page amounts to, so the
control is now on every page rather than appearing only where the browser had
already made a decision. Classification still runs either way, so the bar goes
on saying how much of the page actually needed newer layout; on a page you
simplified yourself that is usually none of it, and reporting *0%* is the
honest answer rather than an awkward one. Both directions are forgotten on
navigation: they are decisions about a page, not settings.

Five controls, a URL, and a warning do not always fit in one bar, so the bar
says which of them it had to cut. Text that did not fit ends in an ellipsis
rather than simply stopping, because `https://example.com/behi` does not look
cut — it looks like a page whose path is `behi`, and an address bar is the last
place in a browser that should be quietly approximate. What a URL is guaranteed
is its origin: the scheme and the host are what a reader is being asked to
trust, so the path is what gives way. The warning still outranks the URL for
whatever is left over, and its wording is front-loaded so that being cut costs
it the least.

The URL bar is editable: Ctrl+L (or F6, or clicking it) focuses it with the
URL selected, Enter navigates, Escape gives up. A bare host gets `https://`,
because that is what typing `example.com` means.

Reload is the third button and Ctrl+R, and unlike back and forward it is never
greyed out — a page that failed to load is exactly when it is wanted, and that
is the moment the other two are least use.

Ctrl+Shift+D switches the chrome between light and dark. Only the chrome: the
page below it is the author's, and repainting their colours is a decision about
someone else's document, which is the kind of thing ADR-0009 requires this
browser to say out loud rather than do quietly.

Find-in-page is on Ctrl+F: matches highlight as you type, Enter and F3 step
through them (Shift to go back), and the bar counts them. A match already on
screen is not scrolled to — moving the page under someone who can see what
they were looking for is disorienting.

Tabs work: Ctrl+T opens one beside the current tab, Ctrl+W closes it,
Ctrl+Tab and Ctrl+1..9 switch, middle-click opens a link in a new one. The
strip only appears once there is a second tab — above a single tab it would be
a row of chrome that says nothing the URL bar has not already said. A tab is
named by the page's `<title>`, or by its URL when it has none.

Bookmarks are on Ctrl+D, and the bar's rightmost control says whether the page
you are on is saved. Ctrl+B opens the saved list — as a page, in a tab, because
the browser already knows how to show a document with links in it and a
bookmarks *panel* would be a second piece of interface with its own scrolling
and its own bugs. `2kbrowser bookmarks` prints the same list. It is stored as a
tab-separated file under your config directory: a few kilobytes, editable in
anything, and the only state this browser keeps between runs.

Links can be followed without a pointer: Tab walks them in document order,
Shift+Tab goes back, Enter follows, Escape drops the focus. The focused link is
outlined rather than tinted, so it does not read as a find match — both can be
on screen at once and they mean different things — and a link that wraps across
a line break is outlined in every piece, because it is one link.

M3 is complete, and M4 has started with fuzzing. `cargo test` runs a short pass
that mutates the reference fixtures into the HTML parser, the CSS parser, image
decoding, URL parsing, the process-boundary protocol, and the whole render
pipeline; `cargo run -p fuzz` soaks for as long as you leave it, and
`.github/workflows/fuzz.yml` leaves it for twenty minutes a target every night.
That last part is newer than the rest: for a while the only thing that ever ran
was the short fixed-seed pass, which re-runs inputs already seen and by
construction finds nothing new. A fuzzer nobody schedules is a regression test.

The first soak found three panics reachable from an ordinary stylesheet —
`font-size: 0`, `font-size: 99999px`, and `margin: 1e40px` — each of which
stopped the browser. A later one found a fourth: a glyph shifted far enough by
its own run offset that the rectangle built for it left `i32`, which the check
on the text origin did not catch because the origin was not where the glyph
landed. The first soak that ran for longer than a few seconds found a fifth in
about four minutes, and it is not ours: `html5ever` 0.39.0 indexes one past the
end of a string when a `<meta http-equiv=content-type>` element's `content`
attribute ends in the word `charset`. Forty-five bytes of HTML, and the renderer
is gone. It is fixed in upstream's `main` and unreleased, so `crates/dom` steps
between the tokenizer and the tree builder and defuses that one attribute value
on the way past — `catch_unwind` would have been useless, because the release
profile aborts rather than unwinds. All five are fixed, and each has a
regression test where the bug was rather than where it surfaced.

Two things the long soak found that were not bugs in the browser at all. The
image target had been mutating two PNGs, so the GIF and JPEG decoders had never
been handed a byte — a clean run over a decoder nothing reaches is not evidence
about that decoder, and the corpus now has one of each. And the harness was
reporting slow renders that could not be reproduced from the bytes it recorded:
it kept one font cache across millions of unrelated documents, which saturates
and then caches nothing, while a real renderer holds one page per process. Each
input is now measured against a fresh page's cache.

A hang is a finding too, and for a while it was the one kind the fuzzer could
not report: it times an input once it returns, and an input that never returns
is never timed — the harness hung along with it. A watchdog thread now names the
input and ends the run with its own exit status. It does not interrupt the stuck
thread, on purpose: nothing safe can, and a harness that could stop arbitrary
code at an arbitrary point is one whose findings nobody could trust.

**Everything renders in a separate process** — the window, `render`, and
`links` alike. The parent keeps the chrome,
the network, and the disk; a child parses, lays out, and rasterises, and asks
for every image, stylesheet, and frame it wants rather than fetching them
(ADR-0012). A page rendered across the boundary is byte-identical to one
rendered without it — verified on the era fixture at 0 differing bytes of
2.5 MB. Why bother, when `unsafe` is forbidden: the libraries that meet a
hostile page *first* have roughly 460 `unsafe` sites between them. Forbidding it
describes our discipline, not our attack surface.

The network moving to the trusted side is the improvement worth naming: a
renderer with no sockets cannot exfiltrate anything regardless of what it is
tricked into computing, and the third-party rule is now enforced in a process a
compromised renderer cannot reach.

The engine is of its era; nothing protecting it is (ADR-0013). TLS 1.0 and 1.1
are refused outright — not marked, not per-site, not behind a confirmation — so
old sites that offer nothing newer do not load, and that is the intended
outcome. The bar says so in those words rather than showing a network error: a
refusal nobody can recognise is indistinguishable from a bug, and a bad
certificate says something different again, because the two mean opposite
things. There is no relaxed-certificate escape hatch. Certificates are
checked against Mozilla's roots first; a chain nobody public signed is retried
against this computer's own trust store and, if that works, the bar says
"local certificate — readable in transit" for as long as the page is up
(ADR-0015). That is what makes the browser usable behind an intercepting proxy
without making an intercepted page look like an ordinary one. All of it is asserted in tests rather than
inherited from a library default, because "happens to be true" does not survive
a dependency bump.

The renderer is confined on Linux and Windows, by the two mechanisms those
platforms actually have — which are not the same shape. On Linux the child
installs a seccomp filter on itself before reading its first frame, and it is an
**allowlist**: everything not named is refused. What is named was measured
rather than guessed, by tracing real renderer children across every fixture, the
fuzzer's corpus, band and find requests, and subresources arriving over the
pipe. Rendering a page turns out to use *nine* syscalls — read a pipe, write a
pipe, ask for memory, seed the hash tables, exit — which is what makes an
allowlist practical here and would not be in a browser that opened its own fonts
or resolved its own hostnames. Failing uses a few more, so the panic, abort, and
stack-overflow paths were measured too. `scripts/renderer-syscalls.sh` is that
measurement, so it can be rechecked after a toolchain bump instead of trusted
(ADR-0016).

It was measured on two C libraries, because a set decided by the libc is not
evidence about the libc — and the second one earned its keep. Rendering is
identical under glibc and musl; *failing* is not. glibc's `abort` raises its
signal with `tgkill` and musl's with `tkill`, and a list naming only the first
did not hang a musl renderer — it made one die of `SIGSEGV` instead, so a panic
reported itself as a memory fault in a codebase that forbids `unsafe`. Both are
allowed now, and neither reaches further than the other.

And on two architectures, in CI, on every push: the aarch64 Linux job runs the
suite and the measurement rather than only cross-compiling. aarch64 needs
nothing x86_64 did not — its set is a strict subset — which is worth having as
evidence precisely because musl had already shown the set can differ.

On Windows the *parent* builds an AppContainer with no capabilities at all and
launches the child into it, because there is no call a running process can make
to put itself in one. Capabilities are the holes deliberately left in a
container, and this one has none. Both refuse by default; what is left between
them is a difference in kind, not strength — seccomp filters calls and is
installed by the process being confined, an AppContainer restricts resources and
is built by the parent, so nothing the child does can undo it. Both were reached
without writing any `unsafe` (ADR-0014).

On macOS the child applies a sandbox profile to itself, as on Linux, and the
profile is `(deny default)` with two exceptions: reading sysctls, and signalling
itself so that a panic can still abort. That one needed an exception to the
no-`unsafe` rule, because `sandbox_init` is a C function with no safe wrapper
anywhere. It is a single crate, a page long, holding one call and no policy, and
two tests keep it that way — one fails if it grows past 200 lines, the other if
any second crate stops inheriting the workspace lints (ADR-0017). Nothing that
parses a byte from the network is in it.

All three are checked from inside, in CI, on every push. A machine where the
sandbox cannot be installed is a skip on a laptop and a failure on a runner,
because a runner is where these claims get made.

A syscall the allowlist forgot returns `EPERM` rather than killing the process.
That is a deliberate softening: a call nobody measured costs a page, which the
parent already reports, rather than a renderer that dies where a reader sees it.

It still renders the era fixture byte-identically with all three of its images
arriving over the pipe, which is the check that the sandbox did not quietly
break the thing it protects. `2kbrowser --confine-selftest` confines a renderer
and reports what it can still reach — from inside, because a sandbox that
installs successfully and confines nothing would pass any check written from
the outside.

> **Not safe for browsing untrusted sites.** All three platforms confine the
> renderer now, and that is not the same as this being safe. **None of it has
> been reviewed by anyone but its authors** — not the sandboxes, not the parser
> fuzzing, not the TLS configuration. That is the reason this warning is here,
> and the only one that a further push cannot remove.
>
> The rest is specific and worth knowing. The Linux allowlist was measured on
> two libcs and two architectures, so a toolchain bump could still refuse
> something the renderer needs. On Windows, a machine that cannot launch the
> container falls back to an unconfined renderer, saying so on stderr and in
> `--confine-selftest` — a fallback that stayed silently broken in CI for weeks,
> because the test that should have caught it was skipping rather than failing.
> Only loopback is probed there; what rules out outbound is the capability set
> being empty, asserted directly. Until an outside reader has been through this,
> it is a tool for its authors.

## Building

Requires a stable Rust toolchain; `rust-toolchain.toml` pins the components.

```sh
cargo build --release
cargo test --workspace
```

## Rendering a page

```sh
2kbrowser render https://example.com --out page.png --width 800
2kbrowser render page.html --out page.png
2kbrowser open page.html            # click links; Alt+Left goes back
2kbrowser links page.html           # every link, and where you would click
```

Third-party requests are refused by default, so one policy rule removes
essentially all advertising and tracking with no filter lists (ADR-0006). Plain
HTTP is allowed, because much of the old web needs it, and is always marked as
unauthenticated rather than presented as secure.

When a page's layout depends on features this engine does not implement, it is
re-rendered as a document and told so — never silently (ADR-0009):

```text
Rendered as a document: 100% of this page's content uses layout this
browser does not implement.
```

## Reference tests and budgets

Reference tests render `tests/ref/fixtures/` and compare against
`tests/ref/baselines/`. After an intentional rendering change:

```sh
BLESS=1 cargo test -p reftests    # then review the new images before committing
```

Budgets are enforced in CI. Checks that cannot be measured yet report `PENDING`
rather than passing — which is how the memory number stayed honest until there
was something to measure. There is now: rendering the era fixture peaks at
around 27 MB across both processes, against a limit of 100, which is the
"tens of megabytes, not hundreds" claim in PLAN.md checked rather than
asserted. Linux only, because reading a peak from the kernel needs FFI on the
other two and ADR-0002 forbids it here.

```sh
cargo build --release && cargo run --release -p budgets
```

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).
