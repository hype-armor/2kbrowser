# ADR-0006: Network policy defaults

Status: accepted

## Context

Blocking advertising and tracking is conventionally done with filter lists:
large, frequently-updated rulesets maintained by third parties, matched against
every request. They work, but they are a subscription to someone else's
maintenance effort, they grow without bound, and they are an arms race.

There is a structural alternative. Advertising and tracking are almost by
definition *third-party* requests — the mechanism requires contacting a domain
other than the one in the address bar. A browser that simply does not make
third-party requests eliminates the category without knowing a single ad
domain's name.

This is not viable for a general-purpose browser, which would break CDN-hosted
assets across much of the web. It is entirely viable for a browser that renders
documents from the era before third-party asset hosting was universal.

Separately, much of the surviving old web is reachable only over plain HTTP.
Refusing it would gut the browser's purpose; pretending it is secure would be a
lie told in the one place users look for that answer.

## Decision

| Default | Rationale |
| --- | --- |
| **Zero third-party requests** | One policy rule eliminates essentially all advertising and tracking, with no filter lists and no update treadmill. |
| **First-party, session-only cookies** | Persistent cross-site identity is the mechanism the surveillance economy runs on. |
| **Plain HTTP allowed, clearly marked** | Needed to reach the old web. Unauthenticated and tamperable, so the chrome must say so plainly. Never silently upgrade, never silently downgrade. |

Each is a default, not a prohibition; per-site exceptions are a UI question for
M3. The defaults are what matter, because defaults are what almost everyone
runs.

### The exception, and what it is scoped to

Amended once M3 built it (#118). An exception is a **pair** — the site being
read and the third-party host it may load from — and not a bare host.

The bare-host version is the obvious one and it is wrong. A reader who allows
`fonts.example.net` because one site will not lay out without it has said
something about *that site*, not about that host; reading it the other way puts
the host in the browser's good books everywhere, so every other page on the web
can then reach it. That is a cross-site identifier reassembled by consent, and
it is the precise mechanism the rule at the top of this table exists to remove.
An override that quietly rebuilds what the default removes is not an override.

A `file:` document has no host to key on, so every local file shares one. They
are already the same origin to this policy, so this names what was true rather
than deciding anything new.

The list is a tab-separated file beside the bookmarks, and for the same reason:
a permission list nobody can read is a permission list nobody audits. Granting
and revoking are the same list in the same panel, because an allow-list that
only grows is one a reader stops being able to reason about — "I let this
through once to see the images" becomes permanent by accident.

### Marking the secure case, which this ADR used to forbid

Also amended, and this one is a change of mind rather than a detail. The
position was that the chrome marks only the exception — `not encrypted` on
plain HTTP, nothing at all on HTTPS — because decorating the secure case
teaches people to look for a positive signal whose absence is easy to miss.
That reasoning is still right *about words*, and the words have not changed:
HTTPS says nothing, and nothing anywhere says "secure".

What changed is that the bar now has a **control** for what a site may load
from, and a control needs somewhere to be. One that appeared only on the pages
with something to decide would be one nobody learns the position of, and it
would be missing on exactly the page where a reader goes looking for it — the
one whose images did not arrive. So there is a padlock on every page, shut on
HTTPS and open otherwise.

That is a positive signal on the secure case, and the original objection
applies to it. It is accepted on the grounds that the icon's job is to be a
stable target rather than a verdict, and that the verdict is still carried by
the words beside it, which still mark only the exception. If this turns out to
have been the wrong trade, the fix is to make the control a neutral shape on
every page rather than to hide it — a control nobody can find is worse than a
signal nobody reads.

Drawn from rectangles and an ellipse rather than set as text: ADR-0008 bundles
four Liberation families and none of them has U+1F512, so a padlock asked for
as a glyph would draw as a hollow box. The insecure state is an *open shackle*
rather than a struck-through lock, because the display list has no primitive
that can draw a diagonal.

## Consequences

- The single highest-leverage rule in the project: most ad and tracker blocking,
  for one policy check and no ongoing maintenance.
- Sites that legitimately use a CDN for images or stylesheets will render
  incompletely. This is the cost, and it is why the rule needs a visible,
  per-site override in M3 rather than being silently absolute. Both halves of
  that exist now: the bar says how much of a page was withheld, and the padlock
  opens the list it was withheld from.
- The exceptions are the second piece of state this browser keeps between runs,
  after bookmarks. That is a real cost against §1's "no account, no sync, no
  profile" and it is paid deliberately: a permission that did not survive the
  window closing would have to be granted on every visit, and a prompt asked
  often enough stops being a decision and becomes a reflex.
- The zero-third-party-request budget in `tests/budgets` is enforceable as a
  test rather than a hope, once the network stack exists in M1.
- Marking HTTP honestly is a chrome requirement (M3), not a nice-to-have.
