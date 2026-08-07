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
text decorations; tiled background images; external stylesheets, including
`@import` chains and `@media` blocks; and legacy
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
table layout, dashed and dotted borders painting solid, and
`background-position`.

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

The URL bar is editable: Ctrl+L (or F6, or clicking it) focuses it with the
URL selected, Enter navigates, Escape gives up. A bare host gets `https://`,
because that is what typing `example.com` means.

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
decoding, URL parsing, and the whole render pipeline; `cargo run -p fuzz` soaks
for as long as you leave it. The first soak found three panics reachable from
an ordinary stylesheet — `font-size: 0`, `font-size: 99999px`, and
`margin: 1e40px` — each of which stopped the browser. All three are fixed, and
each has a regression test where the bug was rather than where it surfaced.

The process boundary M4 needs is built but not yet switched on. `crates/sandbox`
runs the renderer in a child process — the parent keeps the chrome, the network,
and the disk, and a page rendered across the boundary is byte-identical to one
rendered without it (ADR-0012). Why bother, when `unsafe` is forbidden: the
libraries that meet a hostile page *first* have roughly 460 `unsafe` sites
between them. Forbidding it describes our discipline, not our attack surface.

The engine is of its era; nothing protecting it is (ADR-0013). TLS 1.0 and 1.1
are refused outright — not marked, not per-site, not behind a confirmation — so
old sites that offer nothing newer do not load, and that is the intended
outcome. There is no relaxed-certificate escape hatch. Roots come from Mozilla
rather than the platform, so a corporate root in the system store cannot quietly
read this browser's traffic. All of it is asserted in tests rather than
inherited from a library default, because "happens to be true" does not survive
a dependency bump.

> **Not safe for browsing untrusted sites.** The OS sandbox primitives —
> Landlock and seccomp, the App Sandbox, AppContainer — are not applied yet, so
> a renderer child is an ordinary process that merely happens to be separate.
> The window still renders in-process, and subresources still load inside the
> child rather than being asked for over the pipe. Until that is done this is a
> tool for its authors.

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
rather than passing:

```sh
cargo build --release && cargo run --release -p budgets
```

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).
