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

## Unreleased

**The browser has an icon**: a beige-box computer with a lit screen, which is
what it is for. It appears wherever a program appears outside its own window —
a dock, a task bar, an alt-tab list — and until now this one appeared there as
whatever blank rectangle the desktop uses for a program that never said.

Drawn rather than shipped. A picture would be one file per size, six things to
keep in step, every one a binary nobody reviews; this is a page of geometry on a
1024-unit grid, rendered at whatever size it is handed through the rasteriser
the pages already go through. No new dependency, no asset pipeline, and it is
reviewed by looking at it — `cargo run -p shell --example app-icon` draws it at
every size a desktop asks for, the way `chrome-strip` draws the bar.

Below about thirty pixels it draws heavier. At the grid weight a sixteen-pixel
icon's outlines are four tenths of a pixel and antialias into a grey wash — the
drawing is all still there and nobody can see it, which is the failure every
icon set in existence solves by drawing the small sizes differently. The drive
slot drops out below twenty-four pixels for the same reason from the other
direction: under three pixels wide it is a smudge beside the screen rather than
a detail, and a detail nobody can resolve is noise.

The case is painted white rather than left transparent. The artwork is line work
on white and its white is load-bearing — it is the computer's shell, not the
page behind it — so leaving it out would give a dark task bar a teal outline
with a screen floating inside it, which is a different drawing.

**An image that did not arrive leaves a box, not a hole** (#118). It says
`Load image`, because that is what pressing it does. Until now a page whose
pictures were all on a CDN rendered as a screenful of gaps with nothing to
distinguish a refused image from a dead server — which is exactly the report
that became #109: a browser working precisely as designed being
indistinguishable from a broken one.

Pressing it does one of two things, and only the parent can tell which is
right. If the policy refused the picture, retrying would refuse it again, so
the site panel opens instead with the host it wanted one press from being
allowed — that is the question actually standing between the reader and the
picture. If the image merely failed — a server that was down, a connection that
dropped — the renderer child is dropped and the page rendered again, which is
what makes the retry a retry: that child remembers the failure on purpose, so a
broken image is not re-fetched on every resize.

The box deliberately does not say "blocked". A refusal and a failure are the
same shape on the wire (ADR-0012) so that a compromised renderer cannot use a
page to probe what the user has allowed, and the placeholder is drawn on the
renderer's side. The chrome, which does know, is where the reason lives.

Sized by measurement rather than a threshold. A great deal of the era's markup
is 1x1 spacers and 10px bullets holding a table layout open, and drawing
anything in those would turn a page of invisible scaffolding into a page of
smudges — which is the failure mode of every broken-image icon that ever
shipped. So a box too small to outline gets nothing, one too small for the
words gets the outline, and the words appear only where they measurably fit.
The first version used a fixed minimum, got 80x30 wrong, and clipped the label
at both ends in the size half the era's thumbnails are.

Checked against a placeholder before the link under it: an `<img>` inside an
`<a>` is the era's whole navigation, and a button whose press was swallowed by
the link beneath it would be a button that does nothing. The link is still
reachable from its caption and from the keyboard.

Only where the page is rendered as authored. A document rendering has already
thrown the author's layout away *because* it was not serving the reader, and it
drops the gaps a missing picture leaves along with it (ADR-0009) — a reading
view of an article is the one place a row of empty boxes is not an improvement
on nothing.

And only for `<img>`. An `<iframe>` is a replaced element too and has no image
by nature, so the first version grew a `Load image` button on every empty frame
on the page. The conformance suite caught it — fourteen tests of §10.4's
replaced-element sizing, every one of them built out of `<iframe>` elements
used as plain boxes and none of them about images at all.

**A site can be allowed to load from a host** (#118). ADR-0006 refuses
off-origin subresources by default and names the per-site override as the
reason that default is allowed to be as absolute as it is. Until now there was
no override, so the escape hatch the ADR leans on did not exist — a page whose
images were on a CDN simply rendered without them, for ever, with nothing a
reader could do about it.

The padlock left of the URL opens the list. What this page asked for and did
not get is at the top, each line one press from `allow`; what this site has
already been allowed is under it, each line one press from `revoke`. Both
directions in one place on purpose: an allow-list that only grows is one a
reader stops being able to reason about, and "I let this through once to see
the images" becomes permanent by accident.

**An exception is a pair** — this site may load from that host — and not a bare
host. The bare-host version is the obvious one and it is wrong: a reader who
allows a font CDN because one site will not lay out without it has said
something about that site, and putting the host in the browser's good books
everywhere hands it to every other page on the web. That is a cross-site
identifier reassembled by consent, which is the precise mechanism the rule
exists to remove. An override that quietly rebuilds what the default removes is
not an override. Local files share one key, because they are already one origin
to this policy.

Kept in `sites.tsv` beside the bookmarks, one pair per line, editable in
anything. A permission list nobody can read is a permission list nobody audits.
It is the second piece of state this browser keeps between runs and that cost
is paid deliberately: a permission that did not survive the window closing
would be granted again on every visit, and a prompt asked often enough stops
being a decision and becomes a reflex.

Granting one drops the renderer child rather than re-rendering in it. The child
holding the page also holds what it fetched, refusals included — it remembers
them as failures so a broken image is not retried on every resize — so the
newly allowed host would never actually be asked for. The document itself is
not fetched again.

**The padlock reverses what ADR-0006 said**, and the ADR now records the change
of mind rather than being left to contradict the code. The position was that
the chrome marks only the exception, because decorating the secure case teaches
people to look for a positive signal whose absence is easy to miss. That is
still right about *words* and the words have not changed — HTTPS says nothing,
and nothing anywhere says "secure". What changed is that the bar now has a
control for what a site may load from, and a control needs somewhere to be: one
that appeared only on pages with something to decide would be missing on
exactly the page a reader goes looking for it on, the one whose images did not
arrive. So the padlock is on every page, shut on HTTPS and open otherwise.

Drawn from rectangles and an ellipse, not set as text: ADR-0008 bundles four
Liberation families and none has U+1F512, so a padlock asked for as a glyph
would draw as the hollow box the reload arrow once did. The insecure state is
an open shackle rather than a struck-through lock, because the display list has
no primitive that can draw a diagonal. A dot above its shoulder says this page
has something in the panel worth opening.

The padlock takes 26px from the URL's share of the bar, so on a narrow window
`11 blocked from 3 sites` now elides to `11 blocked from 3 …`. That is the
designed degradation — the count is front-loaded so what goes is the least of
it — and the breakdown it loses is the first thing the padlock beside it opens.

**The address of the link under the pointer**, in the bottom-left corner
(#139). Every browser has had this since before it had tabs, and it is not
decoration: a link's text says whatever its author wanted it to say, and only
the address says where it goes. Without somewhere to read that, the only way to
find out where a link leads is to follow it — which is a browser handing the
question back.

It matters more here than in a browser with JavaScript rather than less. This
one refuses off-site subresources and says when a page is not encrypted, both
of which are about *which host you are dealing with*, and the link about to be
clicked was the one place that question went unanswered.

Over the page rather than in a row of its own, because a strip of chrome that
is empty almost all the time would cost every page a line of height. Capped at
three quarters of the window and elided with the same marker the URL bar uses —
a long address is not worth more than the page it would be lying across, and a
reader cannot move a strip that follows their own pointer. It comes down when
the pointer leaves the link, when it leaves the window, and it is recomputed
rather than blanked when the page relayouts, so a window drag does not make it
flicker.

Checked by `scripts/window-clicks.sh` rather than by `cargo test`, which is the
only honest place for it: the unit tests pin where the strip goes and what it
says, and none of them can prove the event loop asks for one or that a redraw
happens without a click to force it. A strip drawn into a buffer nobody
presents is a passing test and an invisible feature.

**The third-party rule says what it did** (#118). ADR-0006 has refused
off-origin subresources since the first commit and has never once mentioned
it, which is the half of the feature that was missing rather than a polish
item: a page missing a third of its images because its CDN was refused looks
exactly like a page whose CDN is having a bad afternoon. The bar now says how
many subresources a page asked for and did not get, and from how many sites —
`4 blocked from 2 sites`, beside the URL, where the *not encrypted* marker
already lives and alongside it rather than instead of it.

It is counted twice over, in two places that answer different questions. The
process-wide counter is the other side of the pair the budget harness has
always measured: "no third-party request left this process" and "no
third-party request was ever made" read the same at zero, and only one of them
is evidence, so the budget now asserts three issued-zero *and* three refused
rather than a zero that a loader which had stopped resolving `src` attributes
would also produce. The per-page record is what the chrome reads, and it counts
distinct resources rather than requests — a page naming one tracking pixel in
forty places is missing one thing, not forty.

The record is assembled on the parent's side of the renderer boundary and never
sent across it. A refusal and a failure are deliberately the same shape on the
wire (ADR-0012): the child has no business knowing which it got, because
telling it would hand a compromised renderer a way to probe what the user has
allowed. So it is read from the session rather than carried on the rendered
page, and it is rebuilt on every render rather than accumulated — a resize asks
for the same subresources again, and a reader watching the number double while
they widened a window would be right not to believe it.

This is #118's first two parts. The third — a per-site exception, so the rule
has the escape hatch ADR-0006 names as the reason it is allowed to be absolute
— is still to come.

## 0.4.0

A release about boxes this engine never generated. CSS 2.1 says several exist
that nothing here was building: §17.2.1's anonymous tables, the root element's
own box, the strut that gives a line its height, and the anonymous block an
inline element leaves behind when a block is put inside it. An absent box does
not read as a bug. It reads as a page that rendered — a little short, a little
flat, nothing a reader would think to report — which is why most of these had
been wrong since before there was a changelog to record them in.

Right-to-left text is the other half, and the opposite kind of absence: not a
box that was missing but an algorithm, and the one PLAN.md had been listing as
unimplemented since M2 opened.

**The CSS 2.1 conformance suite went from 66.9% to 76.0%** — 3226 of 4821
reference tests to 3665, with no panics across roughly ten thousand renders.
0.2.0 moved further and this file said plainly that the harness was why; this
time fifteen of the 439 are the harness, learning to read an XHTML file's own
encoding declaration, and the rest are the engine.

Six tests were lost, across five of the changes below — right-to-left text
cost two and four others cost one apiece. Every one of the six was passing
because *both* sides of the pair were equally wrong: a reference that spells
its expected result with three non-breaking spaces matches a test that loses
them, for exactly as long as the engine loses them too. Each is recorded with
the issue that carries it rather than quietly absorbed, and PLAN.md lists them
beside the deviations that were already there.

**An anonymous table sits beside a float** rather than on top of it. §9.5 says
a table may not overlap one, and a table §17.2.1 generated is a table like any
other — the child walk asks this of every box with a formatting context of its
own, and the branch that places an inferred table places its own box, so it had
to be told to ask too. No conformance test moves either way; the case is a run
of orphan `display: table-cell` boxes after a float, which the suite does not
cover and Chromium draws beside it.

**A box with a formatting context of its own does not overlap a float**
(§9.5). Its border box narrows and moves beside the float; where it cannot fit
beside it, it goes below. **Worth 15 conformance tests against nothing lost.**

This is the whole visible difference between a plain `<div>` beside a float and
one with `overflow: hidden`. The first has its *lines* shortened and keeps a
full-width box, so its background runs underneath the float. The second has the
box itself shortened, so the background stops where the float begins — which is
how a two-column layout was built out of a float and an `overflow: hidden`
before anybody had flexbox, and this engine drew the second exactly like the
first.

Two halves, and the second is the one that is easy to leave out. Such a box
cannot see the floats *inside* it either, so it gets a formatting context of its
own rather than the one it sits in — without which its inline content is shifted
by the float's width a second time, on top of the shift its own box already
took.

Whether it fits is decided by its min-content width plus its horizontal margins,
which can be negative: a box pulled left by `margin-left: -50px` occupies fifty
pixels less than it declares, and a test of exactly that is what caught the
margins being left out. The band is measured at the box's top rather than over
its whole height, which is not known until it has been laid out — and laying it
out is what the answer is for.

**An absolutely positioned root element takes its offsets** (§10.1). Its
containing block is the initial one — the viewport — and `layout_block` applies
a *relative* shift itself, but absolute placement is a parent's business and
the root element has no parent to do it. So `html { position: absolute;
left: 100px }` moved nothing at all. Two conformance tests, nothing lost.

**A table's `height` is a minimum** (§17.5.3), not its height. Where the rows do
not fill it the excess is shared out among them. **Worth 27 conformance tests
against nothing lost.**

It was ignored outright, so a `<table height="200">` was as tall as its text.
That shape is what a great many of the suite's own *reference* files are built
out of — a cell with `vertical-align: bottom` holding an image at the foot of
a two-hundred-pixel box is how a reference draws "a green rectangle above a
blue stripe" without using the property under test — which is why a rule about
tables was worth twenty-two tests in the backgrounds chapter.

How the excess is distributed is left undefined by §17.5.3. It goes in
proportion to the heights the rows already have, which is what browsers do, or
evenly when they have none to be in proportion to. A percentage height is left
alone: it resolves against the table's containing block height, which is
usually `auto`, and §10.5 then makes the percentage behave as `auto` — which is
what leaving it alone produces.

**A stylesheet decides its own encoding** (§4.4), which is a different
question from a document's and was being answered with a document's rules.
**Worth 18 conformance tests against nothing lost.**

A stylesheet has no `<meta>` and no locale to fall back on, so the
declarations it does carry are the whole of what a browser has. §4.4 puts them
in this order: the transport's `charset` parameter, then a byte-order mark or
the `@charset` rule at the very start, then whatever the link said — a
`<link charset>` or the charset on an `@import` — then the encoding of the
document that referred to it, and only then an assumption of UTF-8.

None of the middle three existed here. An external stylesheet went through the
document decoder, which looks for a `<meta>` a stylesheet cannot have and then
assumes windows-1252. A sheet declaring `@charset "shift-jis"` was read as
windows-1252, so a selector spelled in Japanese matched nothing and the rule
was silently dropped.

`@charset` is read only at byte zero and only in the one spelling §4.4 allows
— `@charset "…";`, a single space, no comment before it. Anywhere else it is
an ordinary at-rule, which is what keeps a stylesheet from redecoding itself
out of the inside of a string.

The referring document's encoding is read from the `<meta>` the document
carries, because by the time the shell sees a page the bytes are gone. That
loses the two steps above a `<meta>` — a byte-order mark and the transport's
header — so a page that declared its encoding only there hands its stylesheets
the era's default instead. Which is exactly what they were handed before any of
this existed, so it is a gap rather than a regression, and it only matters for
a stylesheet that declares nothing itself.

**The root element is a box of its own.** The walk started at `<body>`, so
everything `<html>` declared about its own box was dropped:
`html { border: solid blue }` drew nothing at all and `html { margin: 1in }`
moved nothing. **Worth 11 conformance tests against 1 lost.**

It is laid out now, with the body inside it, and the extra level costs the
reference baselines exactly nothing — a page that says nothing about its root
element gets a box with no margin, no border and no padding, which is a
pass-through.

Two halves of §14.2 came out of it, both about the background the root sends to
the canvas.

**Propagation is all or nothing.** The condition is "if the computed value of
`background-image` on the root element is `none` *and* its `background-color`
is `transparent`", and this engine asked the two questions separately and sent
each answer to the canvas on its own. So a root with a colour and a body with a
tile put the tile on the canvas, where §14.2 leaves it on the body's own box —
a different rectangle, showing a different part of the tile.

**And the tile is positioned against the root, not the window.** §14.2 paints
it over the whole canvas but places it "as if it was painted for the root
element alone", so the offsets are measured from that element's padding box.
Measured from the window, `background-position: -2em -2em` on a root with a
one-em margin and a one-em border puts the tile off the canvas instead of at
its corner.

The one test lost is `floats/float-root`, recorded as #128, and it was passing
because neither side floated. Its reference floats the body, which works now;
the test floats `:root`, which does not — the root is laid out directly rather
than as a child, and a float is applied by a parent's walk over its children.
Chromium honours a float there. No real page writes one.

**A non-breaking space no longer collapses.** §16.6.1 collapses spaces, tabs
and newlines; `&nbsp;` is none of them. It is a character with a width, and the
whole point of writing one is that it survives. **Worth 17 conformance tests
against 1 lost.**

The cause was `char::is_whitespace`, which answers Unicode's White_Space
question and so says yes to U+00A0. Four places asked it: the collapsing pass,
the block's leading and trailing trim, the state carried across run boundaries,
and the intrinsic-width split that decides a column's minimum. So
`x&nbsp;&nbsp;&nbsp;y` came out with one space, `&nbsp;Heading` lost its
indent, and `a&nbsp;b` was measured as two words a column could take apart.

This matters more for the pages this engine is for than for anything modern.
`&nbsp;` is how the era indented a paragraph, spaced a row of navigation links
and held an empty table cell open — the repository's own `era-page` fixture
opens with one, and its heading has been four pixels out of place for as long
as there has been a baseline. Checked against Chromium, which puts it where the
new baseline does.

The one test lost is `generated-content/content-175`, recorded as #126. It was
passing because both sides were equally wrong: its reference ends in three
non-breaking spaces that used to collapse away. They are kept now, and the test
side is still short, because a `white-space: pre` run's trailing spaces are
taken by the same block-level trim. Keeping *those* was tried and measured — it
fixes nothing and breaks two, since the line break beside them then draws a
second empty inline box, which needs §9.4.2's rule that a line box holding
nothing generates none.

**A pseudo-element carries its own counters** (§12.4), and every one of CSS
2.1's counter styles is spelled. **Worth 30 conformance tests against 1 lost.**

§12.4's own example is a heading numbered from a `::before` that increments
the counter itself:

```css
h1::before { content: "Chapter " counter(chapter) ". "; counter-increment: chapter }
```

The number it prints is the one *after* that increment, which one pass over the
style cannot produce: `content` is resolved against the counters as they stand,
and the increment is a property of the style being resolved. So a
pseudo-element that declares a counter operation is computed twice — the first
pass read only for the operation, the second kept — and nearly none of them do,
so nearly none of them pay for it.

Two rules came with it. `::after` is computed after the element's children
rather than beside `::before`, because its box comes after the element's
content and so does anything it does to a counter. And an operation on a box
nobody generates has no effect: a `::before` with no `content`, or one told
`display: none`, counts nothing — without which four tests that check exactly
that went the other way.

The missing counter styles are `decimal-leading-zero`, `lower-greek`,
`armenian` and `georgian`. Greek is twenty-four letters and not twenty-five:
final sigma is a positional form of the same letter, and counting it would
number two items sigma. The other two are additive like Roman but without the
subtractive pairs — 1996 is one letter per non-zero digit, largest first — and
past the top of each system there is no notation at all, so the number is
written in digits rather than as a wall of letters. All four spell list markers
and `counter()` alike, since §12.4.3 takes the values `list-style-type` does.

The bundled Liberation faces carry neither Armenian nor Georgian (ADR-0008), so
those two number correctly and draw nothing. That is the font's coverage and
not the numbering, which is why the reference fixture leaves them out and their
tests name the letters by codepoint instead.

The one test lost is `generated-content/counters-root-000`, recorded as #124. A
pseudo-element's `counter-reset` is scoped here the way §12.4.1 describes an
element's — the element, its following siblings, their descendants — and
browsers do something narrower that four probes against Chromium could not
reduce to a rule. Twenty-two tests need the reading here; one needs the other.

**A short page is no longer mistaken for an empty one.** ADR-0009's third
state — "this page has no content without JavaScript" — fired on any document
with a script and fewer than two hundred characters of text. That is a measure
of *length*, and the ADR asks for near-zero *content*: a table of twenty-six
one-letter rows is a hundred and fifty characters and a complete rendering, and
replacing it with "this page requires JavaScript" is a lie about a page already
on the screen.

There are two measures now and both must agree: how much text there is, and how
many elements carry any. The shell the state exists for has none of the second
— an empty `<div id="root">`, or the one `<noscript>` line a framework's
template ships with. Each measure alone gets a page wrong, which is why neither
replaced the other: the count would call a long article in three paragraphs
near-empty, and the character total called a short complete page a shell.

**The conformance harness reads the XML declaration's encoding.** Not an engine
change and it does not claim to be one — the same bridge as the CDATA
unwrapping beside it, and for the same reason. The suite is XHTML, an XHTML
document declares its encoding in its prologue, and this engine parses
everything as HTML, where `<?xml … ?>` is a bogus comment that HTML5's prescan
deliberately does not read. Chromium does not read it either when the same
bytes arrive as `text/html`.

A hundred and two files in the suite carry non-ASCII bytes under such a
declaration. Every one was being decoded as windows-1252, so `À` reached the
engine as `Ã€` — and a test that uppercases it cannot match a reference that
spells it out. That is the harness losing the document, not the engine
mis-rendering it.

The two together are worth **21 conformance tests against nothing lost** —
fifteen for the encoding, two for the classification, and four that needed
both, which is the whole `text-transform-bicameral` family bar the three that
want locale-aware casing.

**Anonymous tables** (§17.2.1): a run of table-internal boxes with no table
above them gets one generated around it, and is laid out as the table it was
describing. Together with the width rule below, **worth 25 conformance tests
against nothing lost.**

What a page writes is `display: table-cell` on three spans, or a
`display: table-row` and its cells, with no `display: table` anywhere. The
engine used to lay each one out as an ordinary block, so three cells that
belong side by side came out stacked. They are collected into a run now — the
run ends at the first thing that is not table-internal, which is why a
paragraph between two pairs of cells produces two tables rather than one — and
the run is handed to the table code as the children of a box that has no
element behind it. That is the whole shape of the change: `build_grid` and
`layout_table` take a child list instead of reading one from a node, and
everything downstream is unchanged.

One of §17.2.1's rules is still missing: the anonymous *cell* that goes around
a row's child that is not a cell. A run that yields no cells is therefore left
alone rather than wrapped, because a table built from it would swallow its
content — `<span style="display: table-row"><span>aaa</span></span>` still
renders as the word `aaa`, which is what it looked like before any of this.
Without that guard it rendered as nothing at all.

**`left` and `right` together size an absolutely positioned box** (§10.3.7).
With both offsets given and `width: auto` the two edges are pinned and the
width falls out of the equation; this engine shrank the box to fit its content
and then placed its left edge, so the right offset did nothing.

It surfaced as a table one pixel too wide. A reference file that stretches a
table with `left: 1px; right: 1px` had it two pixels wider than the test it
was the reference for, which put every column a fraction of a pixel out and
failed the comparison on antialiasing alone.

A replaced element is the exception (§10.3.8): its `auto` width comes from the
intrinsic size, and narrowing the basis resolved `<img width="50%">` against
the gap between the offsets instead of against the containing block.

**An inline element holding a block is broken around it** (§9.2.1.1), instead
of being laid out as a block. **Worth 31 conformance tests against 1 lost.**

The old shape put the element's background and border around the block child,
made it fill its container where two fragments are each as wide as their own
text, and applied vertical margins an inline box does not have.

What makes it tractable is that the split is a matter of *style*, not of new
machinery. A fragment gets the element's style with the sides it does not own
zeroed — the start side on the first, the end side on the last, nothing at
either break — and the room reserved on the line and the border painted later
both read that same style, so neither has to be told a split happened.

Three things had to agree about it, and each announced itself by losing a box:

- **The float pass.** A split element no longer has a layout pass of its own,
  so a float inside it was reached by nothing and vanished. It descends now.
- **Document order.** The walk hands the container a *grandchild* — the block
  that broke the element open — and `precedes` looked for it among the
  container's children, found nothing, and read that as "does not precede". A
  float declared beside such a block was never placed before it and fell
  through to the end of the container.
- **Inheritance.** The text inside a split element is gathered as a child of
  the *container*, so a `<span style="color: black">` broken around a block
  drew its halves in whatever colour the container had.

A fourth was not a bug but a judgement. The anonymous block a stretch lands in
takes the split element's style rather than the container's, because the line's
strut comes from it: `<font size="2">` broken around an `<hr>` — the era's own
markup, and what turned this up — otherwise spaces a sidebar's links out by the
difference between the two fonts. Arguably not what §9.2.1.1's anonymous boxes
inherit. It is what every browser draws.

The one test lost, `box-display/block-in-inline-001`, is one **Chromium fails
too** — checked directly, same rectangle. It was passing here by the usual
accident: the old block-shaped rendering happened to match a reference the
correct shape does not.

**A box is measured one inline stretch at a time.** The words before a block
child and the words after it can never share a line, and the intrinsic width
was gathering the whole box's inline content in one sequence and measuring it
as though they could — reporting a box wide enough for both.

§9.2.1.1's shape is where it showed. An inline element holding a block is laid
out as a block here, so `Line 1<div>Line 2</div>Line 3` in a table cell asked
for the width of `Line 1Line 3`: very nearly double what a browser gives it,
and the cell came out twice the size beside an identical one built out of
divs. That is `box-display/block-in-inline-001`, which now matches Chromium to
the pixel.

One conformance test, nothing lost, and the fix is not really about
block-in-inline at all — it is about any block container with mixed children,
which is most of the era's markup.

The rest of §9.2.1.1 is still outstanding and PLAN.md now says which part: the
geometry is right, the box is not. An inline element's background and border
wrap the block child instead of stopping either side of it.

**Right-to-left text**, which PLAN.md has been listing as unimplemented since
M2 opened: `direction`, `unicode-bidi`, and the reordering itself. Worth **21
conformance tests** against 2 lost.

The algorithm is `unicode-bidi`'s — the crate `cosmic-text` already depends on,
so the tree gains no new code — and what had been missing was everything around
it. Shaping happened per word, so each Hebrew word came out right and the words
came out in the wrong order; the fix is to run the analysis over the whole
inline formatting context and reorder each line's segments by the levels it
returns. Segments are split at level boundaries as well as at line-break
opportunities, because `AAA<RLO>BBB` is one unbreakable word and two
directions.

An inline box's own borders are not characters and do not reorder like them.
They attach to the ends of the box in the box's own direction, so a `<span>`
that reordering cuts in two draws two fragments with a border on the outside of
each end and nothing at the join — and a nested box's side stays inside its
parent's, which is a plain left-to-right case that broke first and caught the
tie-break the wrong way round.

Whitespace turned out to need a level of its own. The space between two Hebrew
words travels with them; the space between the last Hebrew word and the English
after it is a neutral at the paragraph's direction and belongs past the whole
Hebrew run. Same character, same place in the source, two different answers.

One piece of UAX #9 is written here rather than taken: Latin under an explicit
override. The shaper does not act on the formatting codes — handed
`<RLO>abcdef<PDF>` it draws two blank boxes and six letters forwards, measured
before believed — so such a run is shaped forwards and then mirrored, with rule
L4's brackets swapped by shaping the mirrored characters and letting the shaper
find the glyphs.

`text-align` gains a `Start` value, because §16.2's initial value is "left if
`direction` is `ltr`, right if it is `rtl`" — not a value a stylesheet can name
and so one the enum has to hold, resolved where it is used rather than in the
cascade, since `text-align` and `direction` inherit separately.

And a pre-existing bug this uncovered: `text-align: right` measured from the
edge of the *box* rather than the edge of the room the line actually had, so a
right-aligned line beside a float was pushed past it. Lines carry their own
available width now.

Two tests lost, both right-to-left and both needing block-level work that is
not here: a box split across lines by a `<br>`, and §10.3.3's over-constrained
margin, whose one-line version moved every absolutely positioned and replaced
box as well (8 recovered against 37 lost) and was backed out.

**Line boxes have a strut now** (§10.8.1), and quirks mode takes it away again
on a line with no text — which is the quirk the era's sliced-image tables were
built on.

The strut was half there: a line started at the block's own line height with an
ascent of `font_size * 0.8`. That is near enough for deciding how *tall* a line
is and nowhere near enough for deciding how far below the baseline it reaches,
which is the same number subtracted from the line height. So an image sitting
alone in a table cell had no descender space under it at all, in either mode.
With the face's own metrics and half-leading — the extra a `line-height` asks
for split above and below the content rather than hung underneath it — the
image lands on the same pixel row Chromium puts it on.

Doing only that would have made things worse, not better, for the pages this
engine is for. Quirks mode is where the era lives, and there a line box holding
no text has no strut: the cell is exactly as tall as the picture. A sliced
image with a hairline gap under every tile is not a near miss, it is the page
visibly coming apart. Both modes are now pixel-exact against Chromium, and both
have a reference fixture.

**Net one conformance test, the wrong way** — two recovered, three lost — and
worth saying why rather than burying. All three losses are a test and a
reference that used to be *equally* wrong and now differ, with our side of each
having moved toward Chromium: `floats-124` puts its green band on row 8 where
Chromium puts it on 7 and the reference is still on 9;
`line-breaking-font-size-zero-001` is 98 rows against Chromium's 100, up from
92. Holding the line boxes wrong to keep three pairs agreeing with each other
would be the wrong trade.

Nine reference baselines grow by between one and forty-two pixels, all of them
in standards mode and all of them around images and form controls, which is
where descender space belongs.

**`font-variant: small-caps`**, another of the properties PLAN.md has been
listing as parsed and then ignored since M2 opened — and the `font` shorthand
now keeps the `small-caps` it had been accepting and throwing away.

Synthesised, because there is nothing to ask for: the bundled Liberation faces
carry no small-caps variant. Lowercase letters are uppercased and shaped at 0.7
of the size, which is the number Chromium synthesises at — measured off a
rendering (a 100px `x` in small caps beside a 100px `X` gives cap heights of 46
and 65) rather than argued about from first principles. Our glyphs now land on
the same pixel rows as Chromium's.

Two things it deliberately does not do. It does not shrink the line: a word of
nothing but lowercase keeps the full-size ascent and line height, or a
paragraph of small caps would read as one set in a smaller font. And it does
not change the text — the glyph offsets still point at what the author wrote,
so a search for "word" finds a word drawn as WORD.

No conformance change, and none was available: CSS 2.1's suite has no
`font-variant` test at all. This one is for the pages, not for the number.

**Two things absolutely positioned boxes were measured against, neither of
them what CSS 2.1 says.** Worth **43 conformance tests** between them, with
nothing lost the other way, and both showed up as a box in the wrong place
rather than as anything recognisably about containing blocks.

§10.1 says the containing block a positioned ancestor establishes is its
*padding* box. This used its border box, so `left: 0` landed on top of the
border rather than inside it and a percentage was measured against a box two
paddings too narrow. That is the larger half: 38 tests, most of them the
`absolute-*-height` and `absolute-*-width` families, which put a border on the
container precisely because it is the thing that tells the three boxes apart.

The other half is the box with no positioned ancestor at all, whose containing
block is the initial one — the page. Two corrections were missing there. A
box's own margins move it inside its parent and were never counted, so
`position: absolute; top: 0; left: 0` came out at the body's 8px margin instead
of the page corner. And a top margin is not final until it has finished
collapsing: a `<p>` with `margin-top: 1in` pushes the body down an inch *after*
the box has been placed against it, and took the box along.

New `containing-blocks` reference fixture holds all three: the page corner, the
inside of a border, and a percentage of a padding box.

**`list-style-position`**, which PLAN.md has been listing as parsed and ignored
since M2 opened. An `inside` marker is not a box in the list's padding but the
first inline box of the item's *own* content, which is why it could not be a
tweak to the existing marker: only something on the line pushes the first
line's text along and leaves the rest where they were, so the second line of a
wrapped item comes back under the marker instead of beside it. That is the
whole visible difference between the two values, and it is what the new
`list-style-position` reference fixture holds.

The marker run carries the item's font and colour and none of its box. Cloning
the item's style whole gave it the margin too, and §8.4 then charged that to
the line — a list item with `margin-left: 1in` bought a second inch of inline
edge and came out an inch too wide. Eleven `list-style-position-applies-to`
tests failed on exactly that and said nothing about markers.

**`counter(c, square)` prints a square.** §12.4.3 supports every
`list-style-type`, the glyph ones included; this printed nothing for `disc`,
`circle` and `square`, with a doc comment confidently citing the section it was
contradicting. The suite could not catch it, because the test's reference is
built out of `list-style-position: inside` markers that this engine also drew
nowhere — a blank matched a blank, and the pair passed. Fixing the marker is
what made the blank on one side go away.

Five conformance tests together, nothing lost in the other direction.

**`inherit` is a value now**, which it was not before — the cascade read it as
a length, a colour or a font family name, failed to parse it, and dropped the
declaration. §6.2.1 makes it universal: every property takes it, including the
forty-odd that do not inherit on their own, and it means the parent's
*computed* value rather than a re-parse of what the parent declared. So a child
of an element sized `font-size: 50%` that says `font-size: inherit` gets the
parent's resolved pixels, not half of its own.

It is handled once, ahead of the per-property parsing, because there is nothing
for a property's own parser to say about a value that is a copy. A shorthand
spreads across every longhand it covers, so `border: inherit` takes the width,
the style and the colour, and `border-left-width: inherit` takes exactly the one
field its name points at. Worth **50 conformance tests** with nothing lost in
the other direction — the `-inherit-` tests exist across nearly every property
group, which is why one small change in the cascade moves that many rows at
once.

**`word-spacing`, `outline` and `text-align: justify`**, three of the properties
PLAN.md has been listing as parsed and then ignored since M2 opened. And `ex`,
a CSS 2.1 length unit that parsed as nothing at all — `outline-width: 0ex` left
a medium outline standing where the suite asked for none.

`word-spacing` adds its length at every space, which for preformatted text means
at *each* of a row of them. `outline` (§18.4) is a ring drawn outside the border
box that takes up no room: not a fifth border, since it is the same on all four
sides and does not influence layout — an outline that moved the page could not
be used to mark focus. `invert`, its initial colour, is taken as the element's
own; inverting what is underneath needs pixels that are not rasterised until
after the display list is built. `text-align: justify` (§16.2) stretches the
spaces until a line fills its box, on every line but the last — the last line of
a paragraph keeps its natural width, which is the difference between justified
text and a page of stretched fragments.

**And the measurement goes down: 3356 of 4821 to 3352.** Four newly failing and
none newly passing, which wants explaining rather than burying.

There are 174 `outline` tests in the suite and **every one of them was passing**
before this. They pass by drawing nothing on both sides — the same "identically
blank" trap as the CDATA pairs and the inline boxes before them — and 171 still
pass now that something is drawn, which is the real result. Of the four that
broke, two want an outline on a `display: table-column-group`, which generates no
box here; one wants `outline-width: inherit`, which is #87; and one is a
`word-spacing` subtlety about where the extra space falls relative to a span's
background. None of them is the property this entry is about.

Shipping a negative number is the honest option here. Holding the work back
would leave three M2 gaps open to keep a figure tidy, and the figure is supposed
to be evidence rather than a score.

**Three table gaps the plan had been carrying since M2 opened** — fixed layout,
`empty-cells`, and a `border-spacing` per axis. Together they are worth **85
newly passing tests and none newly failing**, 3271 of 4821 to 3356, which is the
largest single move since the suite started being measured in both directions.

**`table-layout: fixed`** (§17.5.2.1). The columns and the *first row* decide
the widths and nothing below them is measured, which is the point of the
property: a table whose widths are declared should not cost a pass over every
cell to find out what they already are.

Two things in it are easy to get wrong and both were caught by measuring rather
than by reading. A column takes the cell's **border box** — a `width: 80px` cell
with 24px of padding and a 36px border either side makes a 200px column, not an
80px one — and in the collapsing model the borders it adds are the *used* ones,
half of each grid line rather than what the cell declared. The first was worth 35
tests and the second 8.

And a fixed table that declared no width of its own does not stretch its columns
to the window: it is as wide as they asked to be, and a column that asked for
nothing falls back to what its content wants, exactly as it would under
automatic layout. Without that, `<col width="50">` twice over made a table the
width of the window.

**`empty-cells: hide`** (§17.6.1.1): a cell with nothing in it draws neither its
background nor its border, so the table's own shows through. It keeps its room —
the property decides what is painted, not what is laid out — which is why it is
applied to the finished box rather than to the style it was laid out with. The
collapsing model has no cell border to hide and CSS 2.1 says the property does
not apply there, so it does not.

**And `border-spacing` takes a length per axis.** Only the first was read, so
`border-spacing: 0 8px` — the rows spaced and the columns not — came out with
the axes the wrong way round on one of them. A band's background has to stop at
a gap on either axis now, which the row-group work of the previous entry had
only had to think about horizontally.

`cargo run -p gaps` is what caught the documentation drifting behind the code
here: three rows recorded as ignored started being honoured, and it refuses to
pass until README.md and PLAN.md say so too.

**`line-height: normal` comes from the font** (#89). It was a flat 1.2 times the
font size, applied in the cascade, where there are no fonts to ask. §10.8.1
leaves the value to the user agent and says it should be "based on the font",
and every browser answers with the face's own ascent, descent and line gap —
1.15 for Liberation Serif against the 1.2 used here, and further off for the
other two bundled families.

Three things had to change together, which is why this was filed rather than
done at the time.

**`normal` had to survive the cascade.** `line-height` is a `LineHeight` now
rather than a number of pixels: `Normal`, a unitless `Number`, or `Px`. The
first is resolved in `text`, where a face can be measured; `cosmic-text` will
not name the face it matched, so it is found by shaping one `x` and reading the
id off the glyph.

**A unitless number had to inherit as a number.** §10.8.1 says so, and the old
code resolved `1.2` to pixels immediately — so an `<h1>` at 32px inherited the
body's 19.2px line and its text would have overlapped. What stopped it was a
heuristic in `font-size` that recomputed the line height whenever it still
matched the parent's, as a way of asking "was that `normal`, inherited?". That
question has an answer now, and the heuristic is gone.

**And the UA sheet had to stop pinning it.** `body { line-height: 1.2 }` was
overriding `normal` on every element of every page, since it inherits. No
browser's UA sheet sets one. Removing it is what makes the rest of this visible.

The metrics are rounded to whole pixels, which is not cosmetic and is what took
the change from a net loss to a net gain. Every browser built on FreeType does
it: an ascent decides where a baseline sits, and a baseline on a half pixel is a
line of text rendered through a filter. It also stops a fraction of a pixel per
line accumulating down a page — forty lines of a third of a pixel is a line's
worth of drift by the bottom, and it lands differently depending on how many
lines came before it. Unrounded, seven `background-position-applies-to-*` tests
failed by exactly one pixel.

An inline box's content area (§10.6.1) is measured from the same face, which
retires two constants that were the bundled faces' metrics written down by hand
because nothing could reach the real ones.

**3264 of 4821 reference tests to 3271**, nothing newly failing — and a page of
five paragraphs now has its baselines on exactly the same rows as Chromium's,
which is the check worth more than the seven tests. Every reference baseline
moves, because every line on every page does.

**The reader gutter is made of padding** (#91). A page that asks for
`body { margin: 0 }` gets eight pixels anyway, because those pages were written
for a window with browser chrome around it and taken literally they put the
first letter of every line against the glass. That floor was applied to the
*margin*, which is outside the background — so a `body { margin: 0; background:
navy }` page came out navy with a pale frame around it, which is the same "looks
like a bug" the gutter exists to avoid, one step further out.

The compensation for that was the box holding the page carrying the body's
background out to the window on its behalf. Right when that background is the
canvas's, and wrong when the root has one of its own: `html { background: purple
}` with a navy body came out navy to the window edge instead of navy in a purple
field. Padding is inside the background, so with the floor on the padding the
background reaches the glass by itself and §14.2 needs no exception — the
holding box paints no background at all now, which is what #76 wanted and could
not have.

The floor is on the distance from the glass rather than on the margin alone, so
a page that spent it on padding has already met it and one that spent half needs
only the other half.

3261 of 4821 reference tests to 3264, with nothing newly failing. The blast
radius is smaller than it looks: every reference fixture is unchanged, and all
three pages in README.md's screenshots render byte-identically, because a page
has to ask for *less* than the gutter before any of this is reachable.

**A table has six background layers, and now paints five of them** (#86).
§17.5.1 makes a table the table, its column groups, its columns, its row
groups, its rows and its cells, superimposed in that order, with a background in
a lower layer showing through wherever the ones above it are transparent. Only
two of the six were painted: the table and the cells, plus rows, which had a box
of their own because `<tr bgcolor>` striping is how the era made a table
readable. A `<tbody>`, a `<col>` or a `<colgroup>` with a background painted
nothing at all.

**35 newly passing tests and none newly failing**, which is a great deal more
than the four the issue was filed on. Twenty-one of them are in `backgrounds`,
where the suite works through every background property against every element it
applies to, and a table part is a lot of those elements.

§17.6.1 decides the *shape*, and this is where a browser and a naive
implementation part company: in the separated model the gaps between cells show
the table's own background and nothing else, so a band covers the cell areas it
spans and stops at every gap. Rows had been filling the gaps too — the UA sheet
gives every table 2px of `border-spacing`, so this was visible on any striped
table that had not set `cellspacing="0"`.

Splitting a band into one box per cell is right for a colour and wrong for an
image, which is the interesting part. A background image is positioned against
the band as a *whole* — §17.5.1 makes a band one box that the gaps cut holes in,
not a box per cell — so cutting the image up draws it once per cell, and
`tbody { background: url(x) top right no-repeat }` drew four squares where the
suite asks for exactly one. Four tests said so. The colour is painted on the
cell areas and the image on the band, which is right for both except that the
image still bleeds into the gaps: clipping one box to several rectangles is a
shape the display list cannot express. Reachable only on a table that asked for
spacing *and* put an image on a band, and less wrong than drawing it four times.

A `table-layers` reference fixture covers all of it, and was compared against
headless Chromium before it was blessed.

## 0.3.0

A release about the half of the box model that only ever worked for blocks.
README.md has been listing "the box model with borders and backgrounds" under
what works since the beginning, and it meant block boxes; an inline one drew
nothing and took no room, which is why a highlighted phrase came out as plain
text. That is the bulk of what is below, and it brought two older bugs up with
it.

**The CSS 2.1 conformance suite went from 66.1% to 66.9%** — 3188 of 4821
reference tests to 3226 — which is a small number in front of a large change,
and the direction it is small in is worth the paragraph.

49 tests newly pass and **11 newly fail**. Every one of the 11 was passing
because *neither* side of the pair drew anything: their references express the
expected result with an inline background or border, so with those unimplemented
both sides rendered nothing and matched perfectly. A reftest cannot tell
"identical" from "identically blank" — the same trap the harness's CDATA fix
turned up by the hundred in 0.2.0, and the reason the number here is measured in
both directions rather than reported as a total. Each of the 11 now names a real
gap and each is filed: table row-group backgrounds (#86), `inherit` as a value
(#87), `direction: rtl` (#88), `line-height: normal` taken from the font rather
than a constant (#89), and two that were already recorded.

The rest of the release is a fieldset that looks like a fieldset, a radio button
you can tell from a checkbox, and two places where the engine and the instrument
measuring it were wrong in the same direction at once.

**An inline box is a box** (#46, #71). A `<span>` with a background, padding or
a border drew none of them and reserved no room for any of them: a highlighted
phrase, a tinted `<code>`, a coloured label, a pill — all ordinary markup, all
coming out as plain text with the words after it wrapping in the wrong place.
README.md listed "the box model with borders and backgrounds" under what works
without saying it meant *block* boxes.

An inline box is not one rectangle, which is why this waited so long. §8.4 gives
it one fragment per line it crosses, with the horizontal margin, border and
padding on the first fragment and the last — they belong to the whole box,
however many lines it is broken over — and the stretch in between running edge
to edge. The line breaker now takes those two sides as unbreakable runs of their
own, so the room is reserved where the text is measured rather than added at
paint time, and a fragment is measured over everything *inside* the box rather
than over the runs that name it: `<span class=hl>a <b>b</b> c</span>` is one
yellow stretch and not two with a hole where the bold is.

The vertical padding and border are drawn and do not enter the line's height
(§10.6.1), so a phrase with 4px of padding overflows its line rather than pushing
the lines around it apart — which is what browsers do and why inline padding is
something authors use sparingly. The box is as tall as its font's content area
and not as tall as its line box, so a double-spaced paragraph highlights its
phrases at the size of the words instead of in bands that touch.

A pseudo-element is an inline box like any other, so `div::before { content: "";
background: gold; padding: 0 8px }` draws a gold block. That technique drew
nothing here, and the reference fixture for generated content had a line
recording it as a known gap — which was filed against generated content and was
never about generated content at all.

**Two bugs came out from under it**, both of them invisible while inline boxes
drew nothing.

A space at the start of an inline box survived collapsing. A paragraph whose
source breaks the line and indents before `<span> text` kept the space inside the
span, which §16.6.1 removes for being at the start of a line. It cost four pixels
nobody could see until a border drew a box for the space to sit inside. The cause
was a run that collapses to *nothing* being read as "no space here" rather than
"still whatever preceded it".

And a negative border width was clamped to zero instead of ignored. CSS 2.1
makes `border-top-width: -1pt` an invalid declaration, which leaves the initial
`medium` standing — a visible border, not an absent one. While in there,
`thin`/`medium`/`thick` now work as longhand values; they were understood only
inside the `border` shorthand, so `border-top-width: thin` silently stayed
medium.

**3202 of 4821 reference tests to 3225**, with 34 newly passing and 11 newly
failing. Every one of the 11 is a test that was passing because *neither* side
drew anything — the "identically blank" pairs the harness's CDATA fix turned up
by the hundred. They now name real gaps: a background on a `display:
table-row-group` box is not painted (#86, 4 tests), `inherit` is not implemented
as a value (#87, 2), `direction: rtl` is not (#88, 3), `word-spacing` is not (1,
already recorded in README.md), and downloadable `@font-face` fonts are not (1,
outside the scope in ADR-0004).

Two more tests were lost to something the work found rather than caused:
`line-height: normal` is a flat 1.2 here where a browser takes the font's own
ascent and descent, so an inline box's content area and the line box around it
disagree by a pixel and a half where they should be the same height (#89).

**A radio button is round and a `<legend>` sits in its group's rule** (#32,
#33). Both were drawn wrong for the same reason: the display list had rectangles
and nothing else, so a radio was a square — indistinguishable from the checkbox
beside it — and a fieldset's rule ran unbroken behind a legend that sat above the
group. The shape of a radio is not decoration: it is what tells a reader "one of
these" from "any of these", which is the difference between two questions. So
there is an ellipse in the display list now, used by exactly two boxes — the
radio and the dot inside a checked one — rather than the beginning of
`border-radius`, which CSS 2.1 does not have.

The legend needed layout rather than paint. HTML's rendering section, not CSS
2.1, puts it *in* the rule, which means the rule is painted as two pieces with
the legend's own box sizing the gap, and the legend is lifted to straddle the
fieldset's top border. It shrinks to fit first — a full-width legend would cut
the whole rule away and leave the box open at the top — and the room it gave up
in flow is reclaimed, so the group's contents start below the legend rather than
below where the legend used to be. The fieldset then moves down by the half of
the legend standing above it, so nothing above the group is trodden on.

The conformance number is unchanged at 3202 of 4821: CSS 2.1's suite has no
fieldset in it, and would not be where this showed up if it had.

**A propagated background covers the canvas rather than the content** (#76).
§14.2 puts the root element's background — or the body's, in its place — over
the *entire canvas*. This put it there and then painted it a second time on the
anonymous box that holds the page, which is as tall as the content rather than
as tall as the window. With a colour that is an opaque rectangle of exactly the
same colour and nobody could see it. With a background image it is an opaque
rectangle over the top of the image: a page with a `no-repeat` tile taller than
its own text lost everything below the last line, and the era's tiled pages lost
the tile everywhere the text did not reach. The `era-page` reference fixture had
been recording that as its expected output.

The body no longer repaints the colour on its own box either, which is the same
sentence of §14.2 — when the body's background is what reached the canvas, its
own background properties take their initial values.

**And the conformance harness measures a window instead of a canvas.** It
rendered each document to its content height and padded the rest of the 800x600
comparison with *white*, which a browser does not do: a short page with
`html { background: green }` is a green window everywhere and was a green strip
above white here. Both sides of a reftest are usually short in the same way, so
this was mostly neutral — it bit where the two sides differ in height or the
canvas has a colour of its own.

The two changes had to land together, and that is the whole reason #76 sat
filed rather than fixed. Made separately, each is worth **net zero**: the harness
change alone wins `backgrounds/background-root-001` and loses
`floats-clear/margin-collapse-clear-017`, and the §14.2 fix alone wins
`css1/c45-bg-canvas-000` and loses the same one. Together: **3225 of 4821 to
3226**, one newly passing and none newly failing. The issue asked for
`margin-collapse-clear-017` to be settled against a real browser first; it was,
and headless Chromium draws the full ruler that neither of our two sides was
drawing.

One newly passing rather than two, because the §14.2 fix is applied only where
the background actually propagated. The reader gutter tops up the body's margin
to keep text off the glass, and the box holding the page carries the body's
background out past it so the gutter is not a pale frame around a coloured page —
which is right when that background is the canvas's and wrong when it is not.
`c45-bg-canvas-000` is the wrong case and still fails; #91 says what it needs,
which is the gutter made of padding rather than margin and is a wider change than
this one.

**The conformance harness renders each document through a font store of its
own** (#31). One store was held for the whole run of eleven thousand documents.
Its shaping cache stops inserting at a cap, and `cosmic-text`'s own
`FontSystem` loads faces and remembers fallback matches as it goes — so what a
store held when a given test ran depended on every test before it, and a result
could turn on the order of the walk rather than on the document. A change
confined to table layout once flipped `text/bidi-flag-emoji-02`, which contains
no table.

The number is unchanged at 3202 of 4821, which is the reassuring half of the
answer: the measurement was not distorted, it was only *able* to be. It costs
3.4 seconds on a 13-second run, because the faces are embedded and load lazily
— a fresh store is about twenty microseconds.

**And `cargo run -p budgets` says when the browser it spawns is out of date**
(#64). That command builds the budget harness, not `target/release/2kbrowser`,
so the parent could be the new code and the child on disk whatever was built
last. The run then failed with "renderer sent a malformed message", which reads
as "this change broke the wire format" and means "the binary on disk is from a
different change". It warns now, before anything measures. A warning and not a
failure: the check is a heuristic over file times, and one that can stop a run
has to be right every time.

**An absolutely positioned box is moved by its margins** (#30). §10.3.7 puts
the margins in the equation it solves, so `margin-left: 40px` moves the box
forty pixels whether `left` is a length or `auto`, and `right` is measured from
the *margin* edge. This read the offsets alone, and the margin the probe layout
had already applied was then overwritten by the answer — so every absolutely
positioned box sat flat against its containing block's content edge.

**And positioned boxes paint in the order they were written.** Appendix E step
8: positioned descendants with `z-index: auto` paint in document order.
Absolutely positioned boxes were appended after every in-flow child, so an
absolute box always covered a relative sibling however the source was written.
Invisible until two of them overlap — and then wrong every time. The margin fix
is what made them overlap, which is how this was found.

**A rule takes the width and thickness its markup asks for** (#39). `<hr
width="50%">`, `<hr width="200">` and `<hr size="8">` were all ignored: every
rule came out full width and a pixel tall. A half-width centred rule under a
heading is one of the most characteristic things about a page of this era, and
`size` is how one drew a heavy divider.

A narrowed rule is centred, which is what a browser does with no `align` at
all, and `align="left"` or `"right"` moves it — on an `<hr>` that attribute
moves the rule itself rather than aligning text, which is the one element where
those two readings differ.

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
