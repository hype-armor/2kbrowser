# ADR-0020: The third-party boundary is the site, not the host

Status: accepted

Supersedes the part of [ADR-0006](0006-network-policy-defaults.md) that defines
what "third party" means. Everything else that ADR decides — the default itself,
first-party session-only cookies, plain HTTP allowed and marked, forms, the
padlock — stands unchanged.

## Context

ADR-0006 refuses third-party subresources, and the implementation read "third
party" as *a different host*. That is the strictest available reading, and it
was the right one to start with: it needs no data, it cannot go stale, and it is
obviously correct about the case the rule exists for.

It is also wrong about ordinary sites. A page at `www.example.com` whose images
sit on `images.example.com` is one publisher serving one site under two names.
That arrangement predates the browser's whole target era — it is what a separate
image server was called before "CDN" was a word — and refusing it removes
nothing a tracker does while costing the reader the pictures. The reader is then
offered a permission prompt for a decision that is not really theirs to make:
"may this site load from itself?" is not a question with an interesting answer.

The counter-argument is the one that kept the strict reading in place for this
long. Host equality is a rule with no parameters, and every looser rule needs to
know where one owner's names end and the next one's begin — which is a fact
about the domain-name system rather than about any string, and the only complete
answer to it is the Public Suffix List: several thousand entries, maintained by
someone else, stale the day it is vendored. ADR-0006 exists in large part to
avoid subscribing to somebody else's list, and ADR-0007 asks a dependency to
justify itself. Swapping a filter list for a suffix list would be losing the
argument on a technicality.

What resolves it is that the two lists fail differently, and this one fails in a
direction that costs nothing.

## Decision

**Two origins are the same site when they share a registrable domain** — the
public suffix plus the label in front of it, which is the unit somebody buys.
Scheme and port remain irrelevant, as before.

Where the public suffix falls is decided by a small vendored list in
`crates/net/src/site.rs`, which is deliberately not the Public Suffix List:

| Part | What it holds |
| --- | --- |
| A rule | Under a country code that registers at the second level, a generic organisational label (`co`, `com`, `ac`, `ne`, …) is part of the suffix. Both halves are listed, so `bbc.co.uk` is right without `web.de` and `id.me` being wrong. |
| A list | Multi-label suffixes the rule does not catch — the free hosts, then and now, where a subdomain is a **stranger's** page rather than another of yours. |

**The list is allowed to be incomplete because its failures are asymmetric.**
Guessing a suffix *longer* than it is splits one site in two: a refused
subresource, and a padlock entry the reader can allow. Guessing it *shorter*
joins two sites into one: a request this browser exists not to make. Every entry
and the rule only ever lengthen a suffix, so a missing entry costs a permission
prompt and never a leak, and no future addition can open anything that is
currently shut. That is what makes a hand-kept list defensible here when a
filter list is not — a filter list that falls behind stops blocking things,
which is the failure that matters, and this one cannot.

Addresses are never split into labels. `10.0.0.1` and `20.0.0.1` would otherwise
share a registrable domain of `0.1`; two unrelated machines joined by
arithmetic that was never about them.

**An exception is now keyed by the site rather than by the host.** This widens a
grant made on `www.example.com` to `shop.example.com`, and that is not a
widening in substance: under the rule above those two are already first-party to
each other, so either could fetch the resource and hand it over without asking
anybody. Keying by host would only have made the reader grant the same
permission once per subdomain, and ADR-0006 already argues that a prompt asked
often enough stops being a decision. Lines written under the old key are read
through the site rule on load, so an existing `sites.tsv` keeps granting what it
says it grants.

**The cross-page cache (ADR-0018) stays partitioned by host.** It is now
narrower than the policy boundary, and deliberately: a partition finer than it
needs to be costs cache misses, and nothing else.

## What it moved

Measured before merging, over the stylesheet, image and frame references of 18
live pages — 1,000 references between them. 159 changed side, and **not one of
them was an advertising or tracking host**: they are `media.cnn.com`,
`assets.science.nasa.gov`, `c.arstechnica.com`, `static.theguardian.com` — a
publisher's own pictures, on a name the publisher owns. Everything the rule is
for stayed refused: `doubleclick.net`, `googlesyndication.com`, `adnxs.com`,
`scorecardresearch.com`, `adsafeprotected.com`, and the rest of that list.

The measurement also names the limit of this change. `ichef.bbci.co.uk` on
`bbc.co.uk`, `i.guim.co.uk` on `theguardian.com`, `thumb.wikimedia.org` on
`wikipedia.org` and `cdn.arstechnica.net` on `arstechnica.com` are all still
third party, because a publisher's *second domain* is not a subdomain and no
rule short of knowing who owns what could join them. Those are what the padlock
is for.

## Consequences

- Sites that serve their own assets from their own subdomains render. This is
  the common case the strict rule was wrong about, and it is why the rule is
  worth loosening at all.
- The project now vendors a list it has to think about, which ADR-0006 was
  written partly to avoid. It is bounded, it is about suffixes rather than
  about advertisers, and — the actual defence — it does not need to keep up with
  anything: an entry that is missing refuses a request.
- The boundary is no longer derivable from the two hosts alone, so
  `net::site` is the single place it is decided and everything asks that
  rather than comparing strings.
- A page can still be split from its own subdomains by an over-long suffix
  guess. The reader sees a refusal in the padlock and can allow it, which is
  the same recourse they have for every other refusal.
- `thumb.wikimedia.org` on `en.wikipedia.org` is still third party, because
  those are two registrable domains. Whether that pairing should be allowed is a
  question about *Wikipedia*, and the padlock is where it gets answered.
