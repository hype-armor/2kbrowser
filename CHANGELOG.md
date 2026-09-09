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

Nothing yet. Changes land on `next-release` and are summarised here as they go,
rather than reconstructed from the log on the morning of a release.

## 0.1.0

The first tagged release, and the first thing here anybody could point at. It
covers everything the repository had built before it could make a release at
all, none of it carrying a version, so this section says where the browser
stands rather than what moved this week. Every release
after it is a week's worth of change, which is the shape the rest of this file
takes.

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
