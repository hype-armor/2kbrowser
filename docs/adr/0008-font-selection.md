# ADR-0008: Font selection

Status: accepted

Extends [ADR-0005](0005-deterministic-rendering.md), which established that
fonts are bundled and the system rasteriser is never used, but did not say which
fonts. This ADR answers that, under the requirement that text render correctly
on the *modern* web as well as the archived one.

## Context

Font choice for a browser is not a matter of taste. Three requirements, in
descending order of how badly getting them wrong shows:

**1. Coverage.** A page in Japanese, Arabic, Hindi, or Hebrew renders as rows of
tofu boxes without a face covering its script. So does any page using emoji,
which on the modern web is most of them. This is the difference between "plain"
and "broken," and it is the requirement that dominates the size question below.

**2. Metric compatibility.** Pages name concrete fonts —
`font-family: Arial, Helvetica, sans-serif` is still ubiquitous. Substituting a
face with different advance widths does not merely change the typeface; it
changes line breaking, and therefore layout. Metric-compatible substitutes
render the author's intended layout with a different typeface, which is the
correct failure mode. Non-metric substitutes reflow the page.

**3. The CSS 2.1 generic families.** `serif`, `sans-serif`, `monospace`,
`cursive`, and `fantasy` must all resolve to something.

Everything bundled must also be redistributable alongside GPL-3.0-or-later. SIL
OFL 1.1 and Apache-2.0 both are.

## Decision

Bundle the following. All are SIL OFL 1.1.

| Role | Font |
| --- | --- |
| Arial / Helvetica, metric-compatible | Liberation Sans |
| Times New Roman, metric-compatible | Liberation Serif |
| Courier New, metric-compatible | Liberation Mono |
| Georgia, metric-compatible | Gelasio |
| Pan-Unicode sans fallback | Noto Sans |
| Pan-Unicode serif fallback | Noto Serif |
| CJK | Noto Sans CJK |
| Emoji | Noto Color Emoji |
| Remaining scripts | Noto Sans {Arabic, Hebrew, Devanagari, Thai, …} |

Generic family resolution: `sans-serif` → Liberation Sans, `serif` → Liberation
Serif, `monospace` → Liberation Mono, each falling back through Noto for
uncovered codepoints. `cursive` and `fantasy` resolve to `sans-serif` for now;
CSS 2.1 requires them to resolve, not to be distinct.

Two gaps are accepted rather than solved: **Verdana and Tahoma have no free
metric-compatible clone**, so pages naming them will reflow, and the
cursive/fantasy mapping is a placeholder. Both are tracked as issues.

### The size conflict, and how it is resolved

Genuine coverage is large. Noto Sans CJK is roughly 20 MiB on its own and Noto
Color Emoji roughly 10 MiB; the full set lands somewhere around 45–60 MiB. That
does not fit the 20 MiB binary budget, and shrinking coverage to fit it would
defeat the requirement this ADR exists to satisfy.

The budget is therefore **split rather than raised**:

| Budget | Limit |
| --- | --- |
| Release binary | ≤ 20 MiB (unchanged) |
| Bundled font payload | ≤ 64 MiB (new) |
| Total distribution | ≤ 84 MiB (new) |

Fonts ship as a data payload beside the binary, not embedded in it. This is not
an accounting trick: PLAN.md's resource-weight goal is about being fast on an
old laptop, which is a statement about RAM, CPU, and startup time. Fonts are
memory-mapped and faces load lazily, so a 60 MiB payload costs a few hundred KiB
of resident memory for a typical Latin page and does not touch cold-start time.

What it does cost is **install size, which grows roughly fourfold**. That is a
real regression against the project's minimalist framing and is stated here
rather than buried, so that a future reader sees it was chosen, not overlooked.

## Consequences

- Modern pages render with correct glyphs across scripts and emoji instead of
  tofu, and pages naming Arial, Times New Roman, Courier New, or Georgia lay out
  as their authors intended.
- Determinism from ADR-0005 is preserved: these are our font files, rasterised
  by us, identical on all three platforms.
- Install size grows to roughly 84 MiB worst case. Per-script optional payloads
  would reduce this and are a plausible later refinement.
- Bundling must reproduce each font's OFL copyright and licence notices in the
  distribution.
- Whether fonts are vendored in git or fetched at build time against pinned
  SHA-256 checksums is unresolved and tracked as an issue; ~60 MiB in git
  history is permanent, and a build-time fetch makes builds require network.
- Implementation lands in M1 with the text stack. Bundling fonts before there is
  anything to render with them would only slow CI.
