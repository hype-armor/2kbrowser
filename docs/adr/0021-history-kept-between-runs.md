# ADR-0021: A history that outlives the run

Status: accepted

## Context

This browser has kept two things on disk: bookmarks, and the site exceptions
(ADR-0006). Both are lists of decisions somebody made on purpose. A history is
not that — it is every page a person happened to open, whether or not they
meant to keep it, and it is the single most sensitive file a browser can write.

ADR-0018 walked right up to this and stopped. Its cross-page cache lives in
memory for exactly one run, and the reason is written down there:

> On disk is a persistent record of what a person has read, against a README
> that says bookmarks are the only state this browser keeps between runs. That
> is a separate and much larger decision, and this does not license it.

This is that decision, asked for directly (#197). The argument for it is the
ordinary one and it is a good argument: a browser that cannot tell you where
you were yesterday is missing something people use browsers for, and "look
through your tabs again" is not an answer. The argument against it is that
§1 says *no account, no sync, no profile*, and a browsing history is the thing
every one of those words is about.

Both are true. What decides it is not which argument is stronger but what the
file is allowed to be.

## Decision

**A history is kept, as a plain file, bounded, and inert.**

| | |
| --- | --- |
| **Where** | `history.tsv`, beside `bookmarks.tsv` and `sites.tsv` in the config directory. |
| **What** | When, the address, and the title. Nothing else — no referrer, no counts, no durations, no per-page state. |
| **How much** | The most recent 500 addresses. The oldest are dropped, not archived. |
| **One entry per address**, not per visit. The later visit replaces the earlier one. |
| **Never sent** | There is no code in this browser that could send it, and this ADR is what stops one being written. |

Four properties do the work, and each is a refusal rather than a feature:

**It is a file, in a format anybody can read.** The same reason the bookmarks
and the exceptions are: a record about you that needs a tool to inspect is a
record you do not really hold. `grep` works on it. So does deleting it.

**It is bounded.** 500 addresses is a few weeks of ordinary reading. A bound
rather than a rule about age, because a bound is the thing a reader can check —
the file cannot grow past it, whatever happens. A history with no ceiling
becomes a life story by doing nothing at all.

**It is one line per address.** Thirty entries for the page somebody keeps
going back to is not more truthful, it is less readable — and the question the
list exists to answer is "where was that page?", which needs the address once.
Dropping the repeat count is also dropping a signal about habits that nothing
here needs.

**Forgetting is as reachable as remembering.** Ctrl+Shift+H in the window,
`2kbrowser history --forget` on the command line, and deleting the file, which
is exactly equivalent. This is the same rule ADR-0006 applies to the exception
list, for the same reason: a list that only grows is one people stop being able
to reason about.

The list is shown as a **page**, not a panel — written out and loaded like any
other file, so back, forward, find and the links on it all work without a second
piece of interface having its own scrolling and its own bugs. The saved list
already works this way.

The browser's own generated pages are not recorded. They are rewritten every
time they are opened, so remembering them would say only that somebody pressed
Ctrl+B — and it would put the history list in the history list.

## Consequences

- §1's "no account, no sync, no profile" still holds, and is now doing more
  work than before: the history is the thing that would be *worth* syncing, and
  there is nothing here to sync it with.
- Three files now outlive a run rather than two. That is the real cost and it is
  stated rather than absorbed — the README says two, and this changes the count.
- A reader on a shared machine has a file worth knowing about. It is named
  plainly, it sits beside the other two, and the file itself says in a comment
  how to empty it.
- Timestamps are UTC, and computed here rather than through a date crate. Local
  time needs the zone database, which is a much larger thing to carry
  (ADR-0007) — and it would put the reader's timezone in a file that is
  otherwise about nothing but addresses.
- The next thing that wants to persist something has an argument to answer
  rather than a precedent to point at. This ADR is not a licence for a fourth
  file.
