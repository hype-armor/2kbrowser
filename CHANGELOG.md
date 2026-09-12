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
