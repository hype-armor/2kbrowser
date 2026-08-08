# ADR-0015: Accept this computer's roots, and say when they were needed

Status: accepted

Narrows the root-store position stated in `crates/net/src/tls.rs` and assumed by
[ADR-0013](0013-modern-security-only.md). Answers
[issue #11](https://github.com/hype-armor/2kbrowser/issues/11).

## Context

The browser verified certificates against Mozilla's root store and nothing else.
The reasoning was written down and is still right as far as it goes: it keeps
behaviour identical across the three platforms the way ADR-0005 asks of
rendering, and it means a root installed in the system store cannot silently
read this browser's traffic.

It had a second consequence nobody wrote down. On a machine behind an
intercepting proxy — which is most corporate networks, and some home ones with
antivirus — *every* HTTPS site fails, because every chain is signed by the
proxy's own authority rather than by anyone Mozilla publishes. The browser did
not degrade there. It stopped working.

That is not a security posture. A browser nobody can use protects nobody, and
the person behind the proxy is not made safer by being unable to read anything:
they open a different browser, one that trusts the proxy without mentioning it.

The reported symptom was the whole browser being unusable. The underlying
question is narrower: *when the only thing vouching for a site is something
installed on this computer, what should happen?*

## Decision

Try Mozilla's roots first. On exactly one failure — nothing in that store signed
the chain — try again against this computer's own trust store. If the second
attempt succeeds, the connection is marked in the chrome for as long as the page
is on screen:

```text
local certificate — readable in transit
```

Three things make this narrower than "trust the platform store".

**The public roots are still first, and still the default.** A site that
verifies normally never touches the local store, costs no extra connection, and
is never marked. The marking therefore means something: it appears only where
something local is actually standing in the way.

**Only one failure is retried.** `UnknownIssuer` — nobody I trust signed this —
and nothing else. An expired certificate is expired whoever signed it; a
certificate for the wrong name is for the wrong name. Retrying those against a
wider set of signers would be shopping for someone willing to say yes, which is
the opposite of what this is for.

**Verification stays on.** The second attempt changes *who may sign* and nothing
else: same provider, same protocol versions, same refusal to skip checking.
There is still no relaxed-certificate escape hatch, and ADR-0013 stands
unamended — TLS 1.0 and 1.1 remain refused outright.

## Consequences

**The marking is the whole justification, so it has to be visible.** Trusting a
local root silently would make an intercepted page and an ordinary one look
identical, which is precisely what this project refuses to do everywhere else:
plain HTTP says *not encrypted*, a legacy-TLS refusal says so in words, a
re-rendered page says it was re-rendered. Certificate provenance is the same
kind of fact and gets the same treatment. It outranks the rendering-mode notice,
because who can read the connection matters more than how the page was laid out,
and it applies whichever mode the page ended in.

**It cannot tell a proxy from an attacker, and does not pretend to.** A
corporate proxy, an antivirus, and someone on the network with a certificate
installed on this machine all look the same from here. The wording says what is
true of all three — something local signed this and can read it — rather than
guessing which.

**It costs a second connection, on the failure path only.** A genuinely broken
certificate is now checked twice before being refused. That is latency spent
where the page was not going to load anyway.

**A new dependency in the TLS path.** ureq's `platform-verifier` feature pulls
in `rustls-platform-verifier`, which is the rustls project's own crate and uses
each platform's native verifier rather than reimplementing one. It is reached
only after the public roots have refused.

**The property ADR-0013 wanted is weakened, deliberately, and by exactly this
much:** an intercepting proxy can now read this browser's traffic *if* the
reader ignores a notice in the chrome saying so. Before, it could not read it at
all, and the reader could not read anything either. That trade is the decision.
