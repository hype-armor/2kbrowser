#!/usr/bin/env bash
#
# Drops this workspace's own build artifacts from `target/`, leaving the
# third-party ones.
#
# Run immediately before a CI job saves its cache. The thing worth carrying to
# the next run is the 318 crates.io packages in `Cargo.lock`: they take minutes
# to compile and change only when the lockfile does. The 14 workspace crates are
# the opposite — quick to build and different on every commit, so caching them
# stores a copy that the next run must invalidate anyway.
#
# The one-shot saving is small: a fresh CI build measured 3306 MiB before and
# 3141 MiB after, because 318 dependencies dwarf 14 crates, and the compressed
# archive GitHub actually stores is 441 MB on Linux against a 10 GB budget. What
# this is really for is the sweep at the bottom — without it the cache grows
# every time it is restored and saved, and that has no ceiling.
#
# Nothing here fails the job. It runs with `if: always()`, so a non-zero exit
# would turn a red build's diagnosis into two failures, or fail a green build
# over a caching detail. Problems are reported as warnings — visible in the run
# summary, and visible here rather than swallowed, which is the point: a trim
# that quietly stopped working would look exactly like a trim that had nothing
# to do.
#
# Safe to run when the build failed, and safe to run twice: every step tolerates
# what it was going to remove being absent already.
set -euo pipefail

cd "$(dirname "$0")/.."

warn() {
    if [ -n "${GITHUB_ACTIONS:-}" ]; then
        printf '::warning title=trim-target-dir::%s\n' "$*"
    else
        printf 'WARNING: %s\n' "$*" >&2
    fi
}

if [ ! -d target ]; then
    echo "no target/ to trim"
    exit 0
fi

before=$(du -sk target 2>/dev/null | cut -f1 || echo 0)

# Package names, not directory names — `tests/ref` builds `reftests`, and
# `cargo clean -p` wants what the manifest says. Asking cargo rather than
# hard-coding the list means a new crate is covered the day it is added.
if ! members_json=$(cargo metadata --no-deps --format-version 1 --offline 2>&1); then
    warn "cargo metadata failed, so no workspace crate could be identified and target/ is being left alone: $(printf '%s' "$members_json" | head -1)"
    exit 0
fi

if ! members=$(printf '%s' "$members_json" | jq -r '.packages[].name' 2>&1); then
    warn "could not parse cargo metadata (is jq present?), leaving target/ alone: $(printf '%s' "$members" | head -1)"
    exit 0
fi

if [ -z "$members" ]; then
    warn "cargo metadata named no workspace packages, which should be impossible; leaving target/ alone"
    exit 0
fi

# `--profile` covers `conformance`, which inherits release but is a separate
# directory. A package that was never built under a given profile is not an
# error — cargo reports "Removed 0 files" and exits 0 — so anything that does
# exit non-zero here is a real problem worth naming.
attempted=0
failed=0
failures=""

for package in $members; do
    for profile in dev release conformance; do
        attempted=$((attempted + 1))
        if ! output=$(cargo clean --offline --profile "$profile" -p "$package" 2>&1); then
            failed=$((failed + 1))
            failures="${failures}  ${package} (${profile}): $(printf '%s' "$output" | head -1)"$'\n'
        fi
    done
done

if [ "$failed" -eq "$attempted" ]; then
    warn "every one of the ${attempted} cargo clean calls failed — the cache is about to store this workspace's own artifacts in full. First few:"
    printf '%s' "$failures" | head -5 >&2
elif [ "$failed" -gt 0 ]; then
    warn "${failed} of ${attempted} cargo clean calls failed; those crates' artifacts will be cached and go stale:"
    printf '%s' "$failures" | head -5 >&2
fi

# Incremental state is disabled in CI (CARGO_INCREMENTAL=0), so these are only
# ever left by a local run that shares the directory. Cheap to be sure.
rm -rf target/*/incremental target/package target/tmp

# The conformance profile belongs to the CSS 2.1 harness, which no CI job runs.
# Restoring it would cost an upload on every save to hold artifacts nothing in
# this workflow reads.
rm -rf target/conformance

# `deps/` never forgets. A cache that is restored, added to, and saved again
# accumulates one artifact per crate per fingerprint it has ever seen — this
# workspace's own copy had three builds of `read_fonts` in it, 42 MB each, two
# of them unreachable. Left alone that grows without bound until the cache is
# too big to be worth restoring.
#
# Keeping the two newest per crate and extension bounds it while leaving room
# for the genuine case of one crate at two semver-major versions (`bitflags` is
# in this lockfile twice). Deleting one that is still wanted costs a recompile
# of that crate and nothing else: cargo notices the artifact is gone and builds
# it again.
sweep_stale() {
    directory="$1"
    [ -d "$directory" ] || return 0

    # Newest first, so anything past the keep count is the older copy.
    local stale_list
    stale_list=$(ls -t "$directory" 2>/dev/null | awk -v keep=2 '
        {
            name = $0
            extension = ""
            if (match(name, /\.[A-Za-z0-9]+$/)) extension = substr(name, RSTART)
            stem = name
            sub(/-[0-9a-f]+(\.[A-Za-z0-9]+)?$/, "", stem)
            if (++seen[stem extension] > keep) print name
        }') || {
        warn "could not list ${directory}; leaving it alone"
        return 0
    }

    [ -n "$stale_list" ] || return 0

    local swept=0
    local stale
    while IFS= read -r stale; do
        if rm -rf "${directory:?}/${stale}"; then
            swept=$((swept + 1))
        else
            warn "could not remove stale artifact ${directory}/${stale}"
        fi
    done <<< "$stale_list"

    printf '  swept %s stale artifact(s) from %s\n' "$swept" "$directory"
}

for profile_dir in target/*/; do
    sweep_stale "${profile_dir}deps"
done

after=$(du -sk target 2>/dev/null | cut -f1 || echo 0)
printf 'trimmed target/: %s MiB -> %s MiB (%s cargo clean calls, %s failed)\n' \
    "$((before / 1024))" "$((after / 1024))" "$attempted" "$failed"
