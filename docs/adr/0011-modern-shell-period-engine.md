# ADR-0011: A modern shell around a period engine

Status: accepted

Decides the question filed as [issue #1](https://github.com/hype-armor/2kbrowser/issues/1),
and sets the shape of M3.

## Context

The engine is deliberately of its era: CSS 2.1, no JavaScript, tables and
floats, framesets, `bgcolor` and `<font>` (ADR-0003, ADR-0004). That raises an
obvious question about everything *around* the page — the tab strip, the URL
bar, the back button — and it is not obvious that the answer should match.

Two coherent positions:

**Period-authentic chrome.** Chiselled grey bevels, a status bar, a throbber.
It would be of a piece with the engine, it would be memorable, and the project's
name invites it.

**A modern shell.** Chrome that behaves the way a person's hands already expect:
familiar keyboard shortcuts, a URL bar that behaves like every other URL bar,
sensible focus handling, a window that respects the platform.

The case against period chrome is not that it is unappealing. It is that the
engine's constraints are *load-bearing* and the chrome's would not be. Refusing
JavaScript removes cookie walls and tracking; refusing modern layout gives the
project a finish line (§2). A period tab strip removes nothing and buys nothing
— it costs the user fluency in exchange for a joke, and it makes the era feel
like the point when the era is only the means.

There is also a real accessibility cost. `AccessKit` integration is scheduled
for M4, and hand-drawn period widgets are exactly the kind of thing that has no
accessibility tree, no platform focus behaviour, and no high-contrast story.

## Decision

**The chrome is modern; the engine is period.** The two are held to different
standards on purpose.

Concretely, for M3:

- Familiar interaction. Standard shortcuts, standard focus order, standard
  text-selection behaviour in the URL bar. Nothing is renamed for period effect.
- Keyboard-first, because that is the fastest interface, not because it is
  retro.
- Native window behaviour: platform title bar, platform-appropriate scrollbars,
  a window that tiles and resizes like any other.
- Restraint is expressed as *absence*, which is what §1 already claims — no
  sponsored tiles, no feed, no account prompt, no onboarding, no AI sidebar.
  That is the aesthetic. It does not need bevels.

The place the era does show through is the viewport, and only there.

## Consequences

The browser will look unremarkable, and that is the intent: the surprise should
be what it does not do, not what it looks like.

`AccessKit` (M4) has a fighting chance, since standard widgets and standard
focus handling are what an accessibility tree is built from.

This closes off "the whole application is a period piece" as a direction. If it
is ever revisited it will be as a theme over the same widget behaviour, not as a
separate interaction model — a decision about paint, not about how the thing
works.

One thing this ADR does *not* decide: how the browser presents a page it
re-rendered as a document (ADR-0009). That notice is chrome, so it follows this
decision in style, but what it says and how the override works is UX design
belonging to M3 itself.
