# ADR-0019: The accessibility tree crosses as data, and the parent builds the objects

Status: accepted

Answers issue #9, which was filed when M4 dropped AccessKit so that it was a
scheduled decision rather than a thing that quietly never happened.

## Context

A browser is close to the worst thing to ship inaccessible. And a document
renderer with no JavaScript is unusually *well* suited to accessibility: the
semantic tree is not fighting a framework that rebuilt the DOM three times
before paint. Headings are headings, links are links, and none of it moves after
load.

AccessKit is the only credible option in Rust — one tree of nodes translated to
UI Automation on Windows, NSAccessibility on macOS, and AT-SPI on Linux. The
alternative is three platform integrations written here, which is not a serious
proposal.

It wants a semantic tree. This project has exactly that: the DOM plus the box
tree. ADR-0012 put both in a sandboxed child, and the parent deliberately
receives only pixels, link rectangles, a title, the rendering mode, and the
small amount of form geometry #110 and #151 added. The document and the box tree
never cross, and that restraint is the point.

That leaves two shapes.

**Put AccessKit in the child.** Cheaper to write and wrong: the child is the
process that must not hold platform API handles. Registering with UI Automation
or AT-SPI from inside the sandbox defeats the sandbox, which is the one thing
ADR-0012 bought.

**Send the tree across.** The protocol grows from a pixmap and some rectangles
to an arbitrary-depth tree of nodes with text in it, and every byte of it is
chosen by the untrusted side. That is a substantially larger parsing surface in
exactly the place the wire format was written to be small and paranoid.

## Decision

The second: the tree crosses as data, and the parent builds the native objects
from it. On six terms.

**1. A closed set of roles.** The child names a role from an enum the wire
knows — heading, link, button, text field, list, table, cell, image and the rest
— never a string. An unrecognised discriminant is a `WireError::Unknown` and the
frame is refused, which is how every other enum on this wire already behaves. A
role that crossed as a string would be an attacker-chosen string handed to a
platform accessibility API.

**2. Bounds, with numbers that have reasons.** `MAX_FRAME` is 64 MiB and is not
enough on its own: at the size a small node encodes to, a legal frame could
carry on the order of a million of them.

| bound | value | why |
| --- | --- | --- |
| depth | 64 | The *parent* walks the tree recursively to build nodes, so depth is a stack-overflow vector in the trusted process. Measured: the era fixture is 14 deep across 185 elements, and the deepest document in the CSS 2.1 suite is 13. |
| nodes | 65 536 | The parent allocates one AccessKit node per entry. Measured: the era fixture yields 40, the largest fixture here 63. Three orders of magnitude of headroom, and a bounded, predictable allocation. |
| bytes per string | 4 KiB | A name is a label, not a document. The longest measured on any fixture is 430 bytes. |
| text in total | 1 MiB | Bounds the whole tree's text independently of how it is divided into nodes, so 65 536 names of 4 KiB is not a legal frame. |

A tree that exceeds any of them is refused rather than truncated: a truncated
accessibility tree is a page described wrongly, which is worse than a page
described not at all, and the reader has no way to tell.

**3. Built only when something is listening.** The tree is not sent with every
render. The parent asks for it when an assistive technology has actually
attached. A page costs nothing when nothing is using it — and, more to the
point, the new parsing surface is not exercised at all in the common case, which
is the cheapest possible mitigation for the risk this ADR is mostly about.

**4. Once per render, not incrementally.** ADR-0003 means this engine never
mutates a page after load. That removes the entire category of tree-update bugs
that dominate accessibility work in scripted browsers, and it is worth naming as
an advantage rather than treating the simpler design as a limitation.

**5. Its own fuzz target.** This becomes the largest attacker-chosen structure
that ever crosses the boundary. `tests/fuzz` already fuzzes the wire and already
carries a seed corpus; the tree gets a target of its own, with the bounds above
as the properties under test.

**6. The focus travels with it.** #151 gave the child a keyboard focus that is
already a three-state value on the wire. The tree names which node holds it, so
a screen reader follows the keyboard rather than guessing from geometry.

Left open deliberately: whether ADR-0009's document fallback produces a *better*
tree than the authored layout. It might — it is the page reduced to semantics —
and the honest way to find out is to build both and read them, rather than to
assume the authored path is always preferable.

## Consequences

- AccessKit is a new dependency, and it sits in the **trusted** process. That is
  the opposite of the usual direction for a new parser and is the reason this
  ADR spends most of its length on bounds. It falls inside ADR-0007's rule set
  rather than needing a separate argument, but it deserves the scrutiny that
  rule set implies.
- The wire stops being "a pixmap and some rectangles". That sentence appears in
  ADR-0012's consequences and in several module docs; it is superseded here for
  this one message, and those places should say so rather than quietly stop
  being true.
- Three platform integrations arrive at once through one crate, which is the
  whole reason for taking it — and means three platforms' worth of behaviour
  this project cannot test on every push.
- The bounds are load-bearing and arbitrary-looking. They are recorded with
  their measurements here so that raising one later is an argument against a
  number with a reason, rather than against a number somebody picked.
- Nothing about this changes what the child is allowed to do. It gains no
  handles, no syscalls, and no network; it gains one more question it can be
  asked and one more answer it can give.
