#!/usr/bin/env bash
#
# Measures which syscalls a confined renderer actually makes.
#
# `crates/sandbox/src/confine.rs` filters the renderer with an allowlist, and an
# allowlist is only as good as the measurement behind it. This is that
# measurement. Run it after a toolchain bump, an allocator change, or a new
# dependency that gets anywhere near the child, and compare.
#
# It traces real renderer children — not a model of one — across:
#
#   * every reference fixture, which is the range of documents the engine
#     handles;
#   * the fuzzer's corpus, which is the range it survives;
#   * the isolation tests, which drive bands, find, a re-render at a new width,
#     and subresources arriving over the pipe;
#   * the ways a renderer fails, which no fixture reaches and which need calls
#     rendering never does — a panic, an abort, and a stack overflow from a
#     document nested deeply enough.
#
# Only calls made *after* the filter is installed are counted. Everything before
# it — the dynamic loader opening libraries, the runtime setting up — is not
# filtered and not relevant to what the filter must permit.
#
# Two lists come out. USED is what the allowlist has to contain. REFUSED is what
# the filter turned away, and reading it is the point: each line is either the
# sandbox working (`socket`, `openat`) or a gap worth closing (something the
# renderer needed and did not get). The tool cannot tell those apart. A person
# can.
#
# Takes an optional target directory, so the same measurement can be run against
# a different C library — which is not a nicety. The set is decided by the libc,
# so measuring one libc says nothing about libcs. Doing it twice is what found
# the only gap this list has had: glibc's `abort` raises its signal with
# `tgkill` and musl's with `tkill`, and the first version of the allowlist named
# only the one it had seen.
#
#   scripts/renderer-syscalls.sh
#   scripts/renderer-syscalls.sh target/x86_64-unknown-linux-musl/release
#
# Linux only — seccomp is the mechanism being measured. Skipped rather than
# failed where `strace` is missing, because a check that cannot run must not
# look like a check that passed.
set -euo pipefail

if [ "$(uname -s)" != "Linux" ]; then
    echo "SKIP: seccomp, and so this measurement, is Linux only"
    exit 0
fi
if ! command -v strace >/dev/null 2>&1; then
    echo "SKIP: strace is not installed"
    exit 0
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BUILT="${1:-target/release}"
BROWSER="./$BUILT/2kbrowser"
FAILURES="./$BUILT/examples/confined-failure"
if [ ! -x "$BROWSER" ] || [ ! -x "$FAILURES" ]; then
    if [ "$BUILT" != "target/release" ]; then
        echo "FAIL: $BUILT does not hold both binaries; build them for that target first"
        exit 1
    fi
    echo "building what is measured"
    cargo build --release >/dev/null
    cargo build --release -p sandbox --example confined-failure >/dev/null
fi
echo "measuring $BROWSER"

TRACES="$(mktemp -d)"
trap 'rm -rf "$TRACES"' EXIT

trace() {
    # -ff writes one file per process, which is how the renderer child is picked
    # out of the parent it was spawned from: the child's trace is the one
    # containing the `seccomp` call that installs the filter.
    #
    # Failures are expected — three of the things traced here abort on purpose —
    # so the exit status is discarded, and the whole call is wrapped so that the
    # shell's own "Aborted" notice does not read as this script going wrong.
    { strace -ff -qq -o "$TRACES/$1" "${@:2}" >/dev/null 2>&1 || true; } 2>/dev/null
}

echo "tracing renders of every fixture"
for page in tests/ref/fixtures/*.html; do
    trace "fixture-$(basename "$page" .html)" \
        "$BROWSER" render "$page" --out "$TRACES/out.png"
done

echo "tracing renders of the fuzzer's corpus"
for page in tests/fuzz/corpus/render/*.html; do
    [ -e "$page" ] || continue
    trace "corpus-$(basename "$page" .html)" \
        "$BROWSER" render "$page" --out "$TRACES/out.png"
done

if [ "$BUILT" = "target/release" ]; then
    echo "tracing bands, find, resize, and subresources over the pipe"
    # Through cargo so the tests find their fixtures; -ff follows into the
    # children it spawns, which is where the renderers are.
    trace "isolation" cargo test --release -p shell --test isolation -- --test-threads=1
else
    # `cargo test` would build for the host, so the children it spawned would be
    # the wrong binaries and the run would silently measure the default target
    # again. Skipped and said out loud rather than quietly wrong.
    echo "skipping the isolation tests: they build for the host, not $BUILT"
fi

echo "tracing the ways a renderer fails"
for how in panic abort overflow exit; do
    trace "failure-$how" "$FAILURES" "$how"
done

echo "tracing the self-test's deliberate probes"
trace "selftest" "$BROWSER" --confine-selftest

# Everything after the filter's installation, across every child that installed
# one. After, not from: the `seccomp` call that installs the filter is itself
# made before the filter is in force, so the allowlist does not need to contain
# it and counting it would say otherwise.
post_filter() {
    for trace in "$TRACES"/*; do
        [ -f "$trace" ] || continue
        grep -q 'seccomp(' "$trace" 2>/dev/null || continue
        awk '/seccomp\(/ { seen = 1; next } seen' "$trace"
    done
}

names() { grep -oP '^[a-z_0-9]+(?=\()' | sort -u; }

children=$(grep -l 'seccomp(' "$TRACES"/* 2>/dev/null | wc -l)
echo
echo "confined children traced: $children"
[ "$children" -gt 0 ] || { echo "FAIL: no child installed a filter"; exit 1; }

# Split by outcome rather than by name. A call the filter turned away was
# attempted, not used, and listing `socket` as something the renderer needs
# would invert the meaning of this whole exercise.
all=$(post_filter)
echo
echo "USED — succeeded, so the allowlist must contain these:"
echo "$all" | grep -v 'EPERM' | names | sed 's/^/  /'

echo
echo "REFUSED — the filter turned these away:"
refused=$(echo "$all" | grep 'EPERM' | names || true)
if [ -z "$refused" ]; then
    echo "  (nothing)"
else
    echo "$refused" | sed 's/^/  /'
    echo
    echo "  Each of these is either the sandbox doing its job or a gap in the"
    echo "  allowlist. Read them; the difference is not something a script knows."
fi
