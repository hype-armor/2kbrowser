# ADR-0014: The Windows sandbox comes through a dependency

Status: accepted

Narrows how ADR-0002's `unsafe` prohibition and ADR-0007's dependency posture
interact when the two point in opposite directions. Applies to the Windows half
of ADR-0012.

## Context

ADR-0012 puts the renderer in a separate process and says the child gets no OS
access of its own. On Linux that is a seccomp filter the child installs on
itself. Windows has an equivalent — an AppContainer — and it does not work the
same way.

An AppContainer is not self-applied. The parent creates a package profile,
derives its SID, builds a `SECURITY_CAPABILITIES` structure, attaches it to a
`PROC_THREAD_ATTRIBUTE_LIST` with
`PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`, and passes that to
`CreateProcessW` through `STARTUPINFOEX`. There is no call a running process
can make to put itself in one. So the code has to exist on the parent's side of
the boundary, and all of it is Win32 FFI.

That collides with ADR-0002. `unsafe_code = "forbid"` is set at the workspace
level, and `forbid` cannot be overridden by an `#[allow]` in source — relaxing
it means editing `Cargo.toml`, which was the point. The options were:

1. **Relax the lint for `crates/sandbox`.** Then the crate that exists to
   contain hostile code is the one crate allowed to write `unsafe`, and the
   several hundred lines holding the sandbox together would be unsafe code
   written by us, reviewed by us, and — since none of us runs Windows as a
   development platform — tested by one person on one machine.
2. **A separate crate whose manifest does not inherit the workspace lints**,
   holding only the Win32 calls. The reviewable-diff escape hatch ADR-0002
   describes. Same unsafe code, same reviewers, but quarantined.
3. **Use an existing wrapper** and keep `forbid` intact everywhere.

## Decision

Option 3, with `rappct`, pinned to an exact version.

`rappct` is a Windows AppContainer/LPAC toolkit: profile creation, capability
SIDs, ACL grants, and `CreateProcess` with the security-capabilities attribute
and an inherited-handle list. Its public API is entirely safe, so nothing in
this workspace writes `unsafe` and ADR-0002 stands unamended.

The container the renderer runs in is built with **no capabilities at all**.
Capabilities are the holes deliberately left in an AppContainer —
`internetClient` for outbound sockets, the library capabilities for folders —
and the renderer needs none of them, because every resource it wants is a
request the parent answers over the pipe. That makes the Windows confinement
categorically stronger than the Linux one: seccomp here is a denylist, so a
syscall nobody named is allowed, whereas an AppContainer refuses by default and
grants only what is named.

## Consequences

**This is the weakest link in the dependency set, and saying so is the point.**
ADR-0007 asks what a dependency would cost to replace and who maintains it.
`seccompiler`, the Linux equivalent, is AWS's, maintained for Firecracker.
`rappct` is a young crate with one author. It is a real trust decision and not a
routine one.

Three things bound it. It is pinned with `=`, so a compromised later release
does not arrive on a `cargo update`. It is roughly 3,500 lines, which is small
enough to read — the FFI layer, where soundness actually lives, is about 600 of
them, and was read before this was adopted: owned handles that close exactly
once, heap-stable structures held alive across `CreateProcessW`, and `SAFETY`
comments that say the right things. And it sits on the *parent's* side of the
boundary, where a bug means an unconfined renderer rather than a new way into
the process — a failure mode identical to the one we already ship on macOS.

**If it stops being maintained, option 2 is still there.** The API surface used
here is four calls. Replacing them with a quarantined crate of our own is a
known amount of work, not an open-ended one.

**The confinement is reported, not assumed.** `Renderer::confinement()` returns
what the parent actually achieved, the browser says so once at startup when it
achieved nothing, and `2kbrowser --confine-selftest` builds a container and runs
the probes *inside* it, because a container that installs successfully and
confines nothing would pass any check written from the outside.

**A failure to build the container is not fatal.** The renderer is spawned
unconfined and the reason is printed. A browser that refuses to render anything
because it could not build a sandbox is a browser nobody can use to find out
why.

**The profile is persistent.** `CreateAppContainerProfile` writes a registry
entry and a folder under `%LOCALAPPDATA%\Packages`, and the executable's ACL
gains a read-and-execute grant for that package SID — without it the container
cannot read the binary it is meant to run. Both are side effects on the user's
machine, both are made once, and the grant is to this package alone rather than
to `ALL APPLICATION PACKAGES`, which would open the file to every sandboxed
program on the machine.
