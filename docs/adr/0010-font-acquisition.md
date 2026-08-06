# ADR-0010: Fetch fonts at build time, against pinned checksums

Status: accepted

Resolves the question left open by [ADR-0008](0008-font-selection.md) and
tracked as issue #7.

## Context

The full font payload runs 45–60 MiB, dominated by Noto Sans CJK and Noto Color
Emoji. Two ways to get it into a build:

**Vendor in git.** Hermetic, offline, reproducible forever — and permanent.
Every clone pays the cost for the life of the repository, and it cannot be
undone later without rewriting history. Git LFS relocates the cost and adds a
hard dependency for every contributor.

**Fetch at build time**, pinned by SHA-256. The repository stays small and the
pins preserve reproducibility, at the cost of builds needing network and
upstream URLs that rot.

The asymmetry is that the git option is irreversible and the fetch option is
not.

## Decision

**Fetch at build time against pinned SHA-256 checksums, with a vendored cache as
an escape hatch.**

- Each font is pinned by URL *and* content hash. A hash mismatch fails the
  build; it is never a warning.
- Before fetching, the build checks a local cache directory. A populated cache
  makes the build fully offline, so a fetch failure is a first-build problem
  rather than a permanent one.
- `fonts/` may hold vendored faces directly, and anything found there is used
  as-is without a fetch. This is not a special case — it is the cache, checked
  into the repository for the fonts small enough to justify it.

M1 already exercises the escape hatch: the Liberation core is ~4 MiB, small
enough that vendoring it outright is clearly correct, so it lives in `fonts/`
and no fetching happens at all. The fetch path is needed only when the Noto
coverage payload lands.

## Consequences

- The repository stays small, and the decision stays reversible: any font can
  later be promoted into `fonts/` by checking it in.
- A first build on a machine with no cache requires network. Mirrors are
  therefore a requirement of the fetch implementation, not a nicety — this
  development environment reaches `fonts.gstatic.com` but gets 403 from
  `github.com`, and a build that only knows about GitHub releases would fail
  here while passing on CI.
- Pinned hashes make a supply-chain substitution a build failure rather than a
  silent swap of what users read the web in.
- Fonts must move out of the binary before the coverage payload lands. M1
  embeds the Liberation core with `include_bytes!`, which is fine at 4 MiB and
  impossible at 60 MiB.
