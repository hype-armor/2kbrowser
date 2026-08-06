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

## Consequences

- The single highest-leverage rule in the project: most ad and tracker blocking,
  for one policy check and no ongoing maintenance.
- Sites that legitimately use a CDN for images or stylesheets will render
  incompletely. This is the cost, and it is why the rule needs a visible,
  per-site override in M3 rather than being silently absolute.
- The zero-third-party-request budget in `tests/budgets` is enforceable as a
  test rather than a hope, once the network stack exists in M1.
- Marking HTTP honestly is a chrome requirement (M3), not a nice-to-have.
