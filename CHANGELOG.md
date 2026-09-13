# Changelog

Changes collect on `next-release` and ship to `main` about once a week. Each
release gets a section here before it merges, and the tag is made from it
afterwards — so `git show v0.1.0` says what the release was for.

Written as prose, in the same voice as the commit messages: what changed and
**why**, for a reader who was not there. A list of commit subjects is something
`git log` already produces, and reading one has never told anybody what a
release was about.

Releases before the first entry below are not recorded here. The repository
made no releases until this file existed, and inventing boundaries for work
that shipped without them would be tidier than it is true — `git log` is the
record for everything earlier.

## 0.2.0

A release about the engine rather than the browser around it. **The CSS 2.1
conformance suite went from 30.1% to 66.1%** — 1451 of 4821 reference tests to
3188 — which is more than the suite had passed in the whole of the project
before it.

Almost none of that came from making rendering *better*. It came from things
that were absent altogether, each failing wholesale rather than in detail:
`::first-letter`, `inline-block`, counters, generated content, tables built
from `display` values rather than from tag names, and the adjacent sibling
combinator, which was dropped entirely and is in dozens of the suite's shared
reference files. A property that is not implemented fails every test that
mentions it, and there is no partial credit to be had.

One of the larger jumps was not engine work at all but a fix to the harness,
and it is written up below with the direction that matters rather than the
flattering one.

The browser also answers the pointer now: text selects and copies, the
right-hand button opens a menu, and Ctrl with the wheel zooms.

**Two table-structure bugs, both found by drawing a picture rather than by the
suite.** Making a demo page of the newly supported CSS turned up a table that
rendered as one table where two were written, and then a strip of cells that
rendered as nothing.

The grid walk descends through plain wrappers on purpose, so that a row inside
a `<div>` inside a table is still that table's row. It had no stop at a *nested
table*, so an outer grid swallowed an inner one whole. Markup hides this: a
nested `<table>` lives inside a `<td>`, and cells are read by the row loop,
which never recurses. It appears the moment either table is built out of
`display` values.

And §17.2.1's anonymous row: a `display: table-cell` whose parent is not a row
gets one generated around it, together with the cells beside it. Without that
the grid finds no rows at all and the cells are drawn by nobody — a strip of
navigation built this way was simply absent. This is the first piece of
§17.2.1 the engine has; the rest of the anonymous-box rules are still missing.

Neither moves the conformance number: 3188 of 4821 before and after, with
nothing newly passing and nothing newly failing. The suite's table tests do not
write either shape.

**An `<iframe>` is a box.** It was not a replaced element here at all, so one
laid out as nothing and left a hole in a page built around it. It is 300x150
now — §10.3.2 and §10.4's size for a replaced element with no intrinsic
dimensions, which is every iframe here, since this engine loads no document
into one.

The trap in that pair of numbers is that they are *two defaults and not a 2:1
ratio*. Modelling them as an intrinsic size looks right on the both-auto case
and is wrong the moment one axis is given: an iframe an inch tall came out
192px wide instead of 300. The distinction is now in the signature.

**3180 of 4821 reference tests to 3188**, with 9 newly passing and 1 newly
failing. The one is `inline-replaced-height-005`, which was passing by
accident: its iframe asks for a percentage height this engine cannot resolve,
and the box used to collapse to the 20x20 broken-image default, which happened
to be small enough to hide behind the green square the test checks against.

**Counters.** `counter-reset`, `counter-increment`, `counter()` and
`counters()` (§12.4), with the self-nesting scope the spec describes — an
instance created by a reset covers the element, its *following siblings*, and
all of their descendants. That middle clause is the one easiest to read past,
and getting it wrong is not subtle: it decides whether the second chapter of a
document is numbered 2 or 1.

Until now `counter()` in a `content` value dropped the whole declaration, since
a pseudo-element showing half of what was asked for looks deliberate. That
turned out to be the larger half of the generated-content failures.

**A `::before` on an inline element reached nothing at all.** Generated boxes
were bracketed around the *block's* content, so a `::before` on a span inside
it was never gathered as an inline run and never walked as a block child: it
simply was not on the page. A span is where a stylesheet usually puts one, so
every list numbered with `counter()` on inline elements came out blank — which
is how this was found, by a counter test that produced the right numbers on one
line and nothing at all on the other.

**3064 of 4821 reference tests to 3180**, 63.6% to 66.0%, with 116 newly
passing and none newly failing.

**A table is what `display` says it is, not what the tag says.** The grid was
built by looking for `tr`, `td` and `th` elements, so a table written the way
CSS defines one — `display: table` on a div, `table-row` on its children —
produced an empty grid and rendered as nothing. That is not an edge case in the
suite: it is how most of its table tests are written, and how a page that is not
from the era builds a table. The structure now comes from the UA stylesheet,
which is what gives `<tr>` its `display: table-row`, so era markup reaches the
same place by a different road and both work.

`table-caption` and `table-column-group` became real display values with it,
and §9.7's blockification grew the rest of its table: a floated or absolutely
positioned `table-row` is a block, because a row taken out of the flow is no
longer part of any table and a box that kept the display would be looked for in
a grid that no longer holds it.

**`border-spacing`'s initial value was wrong, and it was wrong invisibly.** It
was two pixels. §17.6.1 says zero; the two pixels are the HTML user-agent
sheet's rule for the `table` *element*. With every table in the era's markup
being a `<table>`, the difference never showed — and it showed on every table
built from `display` values, as a 2px gap nobody asked for, on exactly the
tests that were about to start running. Moving it to the UA sheet is most of
the 328.

**2736 of 4821 reference tests to 3064**, 56.8% to 63.6%, with 331 newly
passing and 3 newly failing. The three are all the same missing thing —
anonymous table boxes (§17.2.1), including a `::before` with `display:
table-cell`, which needs pseudo-elements to be able to join a grid. An orphan
table-internal box with no table above it is laid out as an ordinary block
rather than dropped, which is the cheap half of §17.2.1 and keeps such a box
visible.

**`::first-letter`.** The single largest absence the conformance suite was
measuring: 339 of its 4821 tests — 7% of the whole — are one family,
`first-letter-punctuation`, sweeping character by character through which
punctuation a first letter drags along with it. All of them failed, and none of
them was really about punctuation: the pseudo-element was not implemented at
all.

It is now, and the punctuation rule with it. §5.12.2 does not say "the first
character": it says the first letter together with any preceding or following
punctuation — Unicode's `Ps`, `Pe`, `Pi`, `Pf` and `Po` classes — so `")T)est"`
puts `")T)"` in the box, and a combining accent stays with the letter it sits
on. The class tables are generated and vendored rather than taken as a
dependency, at four kilobytes for a question asked about some five hundred
codepoints; that they will age is written down where they live.

Two things about it were not obvious and were checked rather than assumed.
**The box goes on the first letter of the first formatted line, which need not
be in the element the rule matched**: with `<div><p>First</p>…`, `div` styles
the paragraph's `F`. Headless Chromium settled that one after a test written
from the spec disagreed with the implementation. And **the rule nearest the
text wins** — a `::first-letter` on both a div and its paragraph is the
paragraph's, or an ancestor's would arrive later and overwrite it.

**2385 of 4821 reference tests to 2736**, 49.5% to 56.8%, with 351 newly
passing and none newly failing.

One gap worth naming rather than leaving to be found: the box takes the
pseudo-element's style whole, so with `<p><b>Bold</b>…` the first letter loses
the `<b>`. CSS 2.1 makes the box a child of the innermost inline box around the
letter; getting that right needs the cascade run again with a different parent,
at a point where layout has no cascade.

**`inline-block` is laid out.** It was the largest thing CSS 2.1 asks for that
this engine did not do, and the quietest: an inline-block was laid out as plain
`inline`, so its width, height, border and background were dropped and an empty
one — the spacer idiom — vanished entirely. It is a box now, sized by its own
content the way §10.3.9 says (as wide as it wants, no wider than the room on the
line, never narrower than its longest unbreakable word), placed on the line as
one atom, and hung from the baseline of its own last line rather than from its
bottom edge (§10.8.1), so a caption under a thumbnail sits level with the
sentence it belongs to.

`vertical-align` came with it. It existed here for table cells only, and the two
contexts disagree about what "not stated" means — CSS's initial value is
`baseline`, which an inline-block needs, while a cell wants `middle`. The
initial value is now `baseline` and a cell's `middle` comes from the UA sheet,
which is where the HTML specification puts it anyway. `top`, `middle` and
`bottom` on an atomic inline box are honoured; raising and lowering *text* off
the baseline — a `<sub>`, a `<sup>` — still is not.

Two float bugs surfaced underneath. §9.7 makes a floated or absolutely
positioned box block-level whatever `display` said, which this engine did not
do, so `float: left; display: inline-block` — the ordinary way to write a
shrink-to-fit float — put the box on a line instead of against the containing
block's edge. And a float written inside an inline element
(`<span><div style="float: left">…</div></span>`) was reached by nothing at all:
not gathered as inline content, not walked as a block child, simply absent from
the page. Both are fixed.

Together: **2296 of 4821 reference tests to 2385**, 47.6% to 49.5%, with 97
tests newly passing and 10 newly failing. The ten are worth naming rather than
netting off, because each is a case where the inline-block is now *drawn* and
was previously not drawn at all — the reftest matched because both sides were
blank. What they point at: an inline box's own border and padding still take up
no room on the line, and an `<iframe>` has no intrinsic size, so one with
`width: auto` comes out empty instead of 300x150.

**Text selects, and copies.** Dragging across the page selects what the drag
crossed — in reading order, not the rectangle between the two points, so
dragging down the margin takes whole lines the way it does everywhere else.
Ctrl+C puts it on the system clipboard. A browser you cannot quote from is a
browser you cannot use for the thing people mostly use one for.

**A menu on the right-hand button.** Back, forward, reload, open in new tab,
copy link address, copy. Each entry appears only when it would do something:
no greyed-out rows, and no "Copy" with nothing selected. Ordered nearest-first
— what the pointer is on, then what is on the page, then what the tab can do —
with a test pinning that order, because a menu with "Reload" above "Copy link
address" makes the reader read the whole list every time.

**Ctrl and the wheel zoom.** In steps, and the page reflows to the window
rather than being scaled up afterwards: text is shaped at the size it is drawn,
so it is twice as sharp rather than twice as blurry, and a zoomed page still
fits the width it has.

**Four interface bugs, all of them things a reader would hit in the first
minute.** A window wider than the page drew the page twice. The links inside a
fallback rendering were not clickable. The new-tab button sat where the tab
strip covered it when only one tab was open. And a closed pipe — what a shell
does when you press Ctrl+C on a piped command — crashed the renderer rather
than ending its work.

**The reading view got its furniture back.** It falls back on a page whose
*frame* is unsupported while its prose is not — a Wikipedia article is the
case, where the text is ordinary flow and the columns around it are not. The
page's title is on it again, the blank lines pages use as spacing are dropped
rather than stacked, and the reader's colours win over colours written into the
markup: an author's dark text left standing over the reader's dark page is the
same bug upside down, and Wikipedia's taxobox carries its colours on every row.

**Six more properties off the gap list.** `clip` on an absolutely positioned
box, `min-width`, `max-height`, `overflow` clipping — content outside a
clipping box is no longer somewhere the page can be scrolled to — a percentage
width on an image, and the `font` shorthand. `max-height` came with the rule
that a negative length is *invalid* rather than clamped, which is CSS 2.1's
answer for every property that takes one.

**Three layout bugs that were each invisible from the outside.** The adjacent
sibling combinator `div + div` was dropped entirely, which is rare in
hand-written pages and everywhere in the suite's shared references — fixing it
moved 206 tests at once, none of them about selectors. A float with no block
child after it was never placed at all, because floats are held back and placed
when the next block arrives and nothing arrived. And the canvas was sized to
the root box rather than to what the page drew, so a `height: 0` box with text
in it had its text painted and then cut off by a canvas that ended above it —
cut off being indistinguishable from never drawn.

**Generated content.** `::before` and `::after` produce real boxes, with
strings and `attr()` in `content`. A form `content` cannot express drops the
whole declaration rather than half of it: a pseudo-element showing part of what
the author asked for looks deliberate.

**Width media queries, and a selector bug worth naming.** `@media` with a width
condition is answered against the real viewport. Underneath it, selector lists
were split on every comma — including commas *inside* `:is(…)` and `:not(…)`.
Wikipedia ships `… :is(p,table,thead + tbody) { display: none }`, and splitting
that left a bare `table` selector which matched **every table on the page** and
hid all of them, the infobox included. The pseudo-classes themselves were
always rejected correctly; the list simply never reached that check in one
piece.

**A measurement fix that moved the number more than any feature.** Almost every
test in the suite is XHTML and writes its stylesheet inside `<![CDATA[ … ]]>`.
Read as HTML — which is how this browser reads everything, and how every
browser read the XHTML the real web served — that wrapper makes CSS error
recovery swallow the whole stylesheet. The harness now unwraps it. The
direction that matters is not the gain: **342 tests stopped passing**, every
one a pair with the wrapper on both sides, two lost stylesheets and two pages
of unstyled prose matching each other exactly. A reftest cannot tell
"identical" from "identically blank".

**Forms are drawn.** An `<input>` is a void element with no content, so until now
it laid out as nothing at all: a search box was not an empty box, it was absent,
and a login form was a column of labels with no fields beside them. That reads as
a broken page rather than as a browser that cannot submit forms, which is the
honest thing for it to look like. Text and password fields, buttons, checkboxes,
radios, `<textarea>`, `<select>` both closed and as a list box, and the rule
around a `<fieldset>` all draw now, sized in the era's own units because that is
what the attributes say — `size`, `cols` and `rows` count characters and lines,
so a field follows the font it is set in. **Nothing can be typed into, clicked,
or submitted**, and that is a stopping point rather than an oversight: a control
that draws correctly makes the page read correctly, and interaction is separate
work with separate risk.

**Five properties the gap list had been carrying.** `visibility`, `text-indent`,
`text-transform`, `letter-spacing`, `min-height` and `z-index` were all parsed
and then ignored, which is the worst of both: a page that uses them renders
wrongly and nothing says so. They are honoured now. `letter-spacing` was the
instructive one — cosmic-text takes it in em rather than pixels, so the first
version was sixteen times too wide and the gap inventory still called it
"honoured", because an inventory detects *an* effect and not a *correct* one.

**A gap list that cannot rot.** `cargo run -p gaps` renders 41 small cases and
checks what the engine actually does against what README.md and PLAN.md claim it
does. It fails in *either* direction — a property that starts working while the
docs still call it missing is as much a defect in the record as one that breaks.
Neither existing harness could catch that: a reference test passes when both
sides render alike, and a baseline only covers a fixture somebody wrote.

## 0.1.0

The first tagged release, and the first thing here anybody could point at. It
covers everything the repository had built before it could make a release at
all, none of it carrying a version, so this section says where the browser
stands rather than what moved this week. Every release after it is a week's
worth of change, which is the shape the rest of this file takes.

**What it is.** A browser that renders HTML and CSS as the web did around the
year 2000 and does not execute JavaScript. The scope boundary is CSS 2.1, a
finished specification with an official test suite, so unlike an engine chasing
the modern web this one has a finish line to be measured against.

**Where it stands against that suite.** 1451 of 4821 reference tests pass, 30.1%,
with no panics across roughly ten thousand renders. That number is an upper
bound on nothing and a floor under nothing: it is what the suite says, run the
way `cargo run --profile conformance -p conformance` runs it, and README.md is
candid about the three ways the first attempt at measuring it was wrong.

**What it draws.** Block and inline layout with floats and positioning, tables
including the collapsing border model and captions, backgrounds and borders in
all eight styles, lists, the cascade with specificity and inheritance, text
shaped by cosmic-text with the properties that follow from it, images, frames,
and the presentational HTML the era's pages actually used. Pages built on
layout it does not implement are re-rendered as documents and told so, rather
than coming out subtly wrong in silence (ADR-0009).

**What guards it.** Reference tests against a single baseline set shared by
Linux, macOS, Windows and aarch64 (ADR-0005); the CSS 2.1 conformance suite; a
nightly fuzzing soak; budgets on binary size, memory, and third-party requests;
and `unsafe_code = "forbid"` across every crate but the one page that confines
the renderer on macOS. Nothing in this repository is trusted because it looks
right — it is trusted because something fails when it stops being right.

**What it does not do.** No JavaScript, by design and permanently. No form
controls: an `<input>` draws nothing at all, so a search box is absent rather
than merely inert. No fixed table layout, and a list of properties that parse
and are then ignored. README.md and PLAN.md both carry those gaps in full, and
are meant to be read as part of what this release is.
