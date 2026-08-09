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
# It is also what keeps the cache inside GitHub's 10 GB per-repository budget.
# An untrimmed `target/` for this workspace is ~8 GB on its own, and four jobs
# each saving one would evict each other on every push, which is the same as
# having no cache while paying to upload one.
#
# Safe to run when the build failed, and safe to run twice: every step tolerates
# what it was going to remove being absent already.
set -euo pipefail

cd "$(dirname "$0")/.."

if [ ! -d target ]; then
    echo "no target/ to trim"
    exit 0
fi

before=$(du -sk target 2>/dev/null | cut -f1 || echo 0)

# Package names, not directory names — `tests/ref` builds `reftests`, and
# `cargo clean -p` wants what the manifest says. Asking cargo rather than
# hard-coding the list means a new crate is covered the day it is added.
members=$(cargo metadata --no-deps --format-version 1 --offline 2>/dev/null \
    | jq -r '.packages[].name' \
    || true)

if [ -z "$members" ]; then
    echo "WARNING: could not read workspace members; leaving target/ alone"
    exit 0
fi

# `--profile` covers `conformance`, which inherits release but is a separate
# directory. A package that was never built under a given profile makes
# `cargo clean` complain, and that is not a failure of anything.
for package in $members; do
    for profile in dev release conformance; do
        cargo clean --offline --profile "$profile" -p "$package" >/dev/null 2>&1 || true
    done
done

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
    ls -t "$directory" 2>/dev/null | awk -v keep=2 '
        {
            name = $0
            extension = ""
            if (match(name, /\.[A-Za-z0-9]+$/)) extension = substr(name, RSTART)
            stem = name
            sub(/-[0-9a-f]+(\.[A-Za-z0-9]+)?$/, "", stem)
            if (++seen[stem extension] > keep) print name
        }' | while IFS= read -r stale; do
        rm -rf "${directory:?}/${stale}"
    done
}

for profile_dir in target/*/; do
    sweep_stale "${profile_dir}deps"
done

after=$(du -sk target 2>/dev/null | cut -f1 || echo 0)
printf 'trimmed target/: %s MiB -> %s MiB\n' "$((before / 1024))" "$((after / 1024))"
