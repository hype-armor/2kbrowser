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

## Status

**M2 — it renders the era's web.** HTML is fetched over HTTP(S) or read from
disk, parsed into an arena DOM, cascaded through a CSS 2.1 subset, laid out,
shaped against bundled Liberation faces, and rasterised on the CPU.

Working: the cascade with selectors, specificity, and inheritance; the box
model with borders and backgrounds; inline layout with per-span styles and
Unicode line breaking; floats; tables with automatic column sizing, `colspan` and `rowspan`, and
`cellspacing`; images,
including ones sitting in a line; relative and absolute positioning;
framesets; quirks-mode value parsing; the presentational attributes the era's
markup actually used (`bgcolor`, `align`, `<font>`, `border`); list markers;
text decorations; tiled background images; external stylesheets; and legacy
character encodings, which most of the surviving old web needs — a page in
windows-1252 read as UTF-8 is replacement characters where every accented
letter and curly quote should be.

Rendering is deterministic across Linux, macOS, and Windows, checked by
reference tests against one shared baseline set — verified, not assumed: all
three platforms have rendered it byte for byte in CI.

The window has been verified by hand on Linux. CI has no display, so its event
handling and blitting are not covered by automated tests — only its scroll
arithmetic is.

Known to be missing or wrong, rather than hidden: collapsed borders, fixed
table layout, dashed and dotted borders painting solid, `background-position`,
and `@media`/`@import`. Links are styled but not clickable — hit testing and
navigation are M3, along with the rest of the browser chrome.
See [PLAN.md](PLAN.md).

> **Not safe for browsing untrusted sites.** Sandboxing, parser fuzzing, and
> TLS review all land in M4. Until then this is a tool for its authors.

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
2kbrowser open page.html            # window: arrows scroll, Esc quits
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
rather than passing:

```sh
cargo build --release && cargo run --release -p budgets
```

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).
