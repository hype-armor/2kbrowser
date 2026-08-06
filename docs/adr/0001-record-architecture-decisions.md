# ADR-0001: Record architecture decisions

Status: accepted

## Context

PLAN.md argues for a set of decisions that are unusually load-bearing, and
several of them are constraints rather than features — things the project has
decided *not* to do. Constraints erode quietly. Without a record of why a
boundary exists, the boundary reads as an oversight, and the natural thing to do
with an oversight is fix it.

PLAN.md is a living document and will be rewritten as the project moves. The
decisions themselves should not be, because the reasoning behind a settled
decision is what makes it defensible six months later.

## Decision

Record architecture decisions as short, numbered, immutable documents in
`docs/adr/`, in the format popularised by Michael Nygard: Status, Context,
Decision, Consequences.

An ADR is never edited once accepted. A decision is changed by writing a new ADR
that supersedes it, and marking the old one superseded. The trail of superseded
ADRs is the point — it shows what was believed and why it changed.

## Consequences

- Every decision in PLAN.md that constrains future work gets an ADR.
- "Why can't we just add flexbox?" has a written answer (ADR-0004) instead of
  depending on whoever remembers the conversation.
- Reversing a decision requires articulating what changed, which is a useful
  amount of friction — enough to prevent drift, not enough to prevent progress.
