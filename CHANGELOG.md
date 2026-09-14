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
