# ADR-0018: A cache that outlives a page, partitioned by site

Status: accepted

Answers issue #18, which was filed as the analysis so that it existed before
anyone built the thing.

## Context

#14 gave the parent a cache of fetched subresources. It lives on `Conversation`,
on the worker thread that does the fetching, and is dropped with the session.
That scope makes re-rendering cheap — which is what a resize is, and what most
of the cost of one was — and it makes a page that uses the same spacer forty
times fetch it once.

It does nothing for the two cases a reader would name first: going back to a
page you were just on, and the images a site reuses across its own pages.

Extending it is the obvious next step and was deliberately not taken in passing,
because a cache that outlives a page is the first exception to a rule this
codebase states repeatedly — *a page's leftovers never outlive the page*. That
rule is why the child process is torn down between pages and why the per-page
cache is where it is. An exception to it is an ADR's worth of argument.

### What this browser has already disarmed

Two risks dominate discussions of shared caches, and both are largely closed
here by decisions already made.

**Cache probing and supercookies.** The classic attack needs two things: a cache
entry reachable from two different top-level sites, and a way to measure a load
and report the answer. ADR-0006 refuses third-party subresources by default, so
a page can normally only cache things from its own host. ADR-0003 removes the
measurement outright — no `performance.now()`, no `fetch`, no beacons, nothing
that can time a load or send the result anywhere. This is why Chrome and Firefox
needed cache partitioning in 2020–21 and why this browser largely would not.

The exception is ADR-0006's own per-site exception. A host a reader has allowed
becomes reachable from more than one site, and a shared cache keyed on the URL
alone would make it a cross-site identifier — the exact shape the third-party
rule exists to remove, rebuilt by consent.

**Credentialed responses.** Nothing here sends cookies or an `Authorization`
header, so "cached someone's bank statement" is weaker than in an ordinary
browser. A token in a query string is still a plausible way for sensitive bytes
to outlive the page that asked for them.

### What gets worse

**The policy-before-cache ordering becomes far more load-bearing.**
`Conversation::fetch_all` applies ADR-0006 to every URL *before* the cache is
consulted, never after, and there is a test for it because that ordering
survived the first round of deliberate breaks without anything asserting it. Per
page, getting it wrong costs one page. Across pages it becomes *any page served
anything any earlier page fetched* — including `file:` bytes read by a local
document, which `Refusal::LocalFile` exists specifically to prevent.

**`Cache-Control` is not merely unhandled — nothing in the tree parses it.**
Confirmed while writing this: zero references outside one comment, and
`net::Fetched` carries only `content_type` off the response. Tolerable while the
cache dies with the page; across pages it means retaining a response a server
explicitly said not to store.

**Trust provenance would be dropped.** `net::Fetched` carries `trust`, and
ADR-0015 has the chrome mark a page verified only against this computer's own
roots as readable in transit. A cache that keeps bytes without their trust could
serve a resource fetched through an intercepting proxy to a later page showing
no marking at all — turning a marking this project treats as load-bearing into a
lie.

**Scheme confusion.** `Origin::is_same_site` compares *host only*:
`!self.host.is_empty() && self.host == other.host`. So `http://example.com` and
`https://example.com` are the same site to the policy. The cache keys on the
full URL string, which is correct today; the risk is a later change normalising
the scheme out of the key, at which point an attacker on plain HTTP can poison
an entry a secure page then uses.

## Decision

Build it, in memory, on six terms. Each is a term rather than a nicety: a cache
that drops any one of them is a different and worse decision than this one.

**1. In memory, for one run.** On disk is a persistent record of what a person
has read, against a README that says bookmarks are the only state this browser
keeps between runs. That is a separate decision and a much larger one; it is not
taken here and this ADR does not license it.

**2. Keyed on the pair (document site, full URL including the scheme).** Not on
the URL alone. Partitioning closes the allowlist supercookie by construction
rather than by argument, and it is the same shape ADR-0006's exception already
has — that ADR settled that an exception is a *pair*, for exactly this reason,
and a cache keyed the other way would undo it. The cost is that a host shared by
two sites is fetched once for each, which the third-party default makes rare.

**3. Consulted only behind the policy check.** The existing ordering, kept, with
its existing test — and a new sibling in which the second document is both a
different origin *and* a different scheme, because host-only same-site means an
origin test alone would not catch a `file:` leak.

**4. Trust travels with the entry and is served back with it.** Not in the key:
an entry fetched through a proxy should not be fetched a second time, it should
be *reported* honestly. Keying on it would trade a lie for a redundant request
and fix nothing.

**5. `no-store` is honoured, and so is `Pragma: no-cache`.** The second matters
more here than it would in a modern browser and is the reason this term names
two headers rather than one: this browser is aimed at the servers of the
HTTP/1.0 era, where `Pragma` is what they actually send. Parsing stops there —
`max-age`, `Expires` and revalidation are a freshness model, and a cache that
lives for one run does not need one.

**6. Reload bypasses it.** That is what reload means. Without it the single
control a reader has over staleness does nothing, which is worse than having no
cache.

Mechanically: it replaces the per-page cache rather than sitting beside it — one
cache, partitioned, rather than two stores holding the same bytes. Eviction is
least-recently-used against a byte cap, because something that outlives pages
needs a policy where stop-when-full was fine for one page's lifetime. The lock
is taken to look up, released, the fetch made, and taken again to insert: two
concurrent misses on the same URL do the work twice, which is waste rather than
error, where holding the lock across a fetch would serialise the concurrency
#15 added.

## Consequences

- The rule "a page's leftovers never outlive the page" now has exactly one
  exception, and it is written down here rather than discovered in the code. The
  child process is still torn down between pages; this is about the parent.
- Memory is spent permanently rather than per page. The budget harness reports
  26.8 MB against a limit of 100, and the per-page cache is already 8 MiB; the
  replacement is sized against that headroom and is reported by the same
  harness, so the cost shows up as a number rather than as a belief.
- `net::Fetched` grows a field for what the response said about storing it, and
  the HTTP path reads two more headers. Both are already available on the
  response; nothing new is parsed off the wire.
- The partition key means the cache is less effective than an unpartitioned one
  by exactly the amount that unpartitioned one would have been dangerous.
- A future disk cache inherits none of this. It would need its own ADR, and the
  argument would have to start from the README sentence this one preserves.
