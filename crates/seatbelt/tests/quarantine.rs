//! That this crate is the *only* place `unsafe` is allowed.
//!
//! ADR-0002 chose `forbid` over `deny` so that no `#[allow]` in source could
//! undo it: relaxing the rule has to be an edit to a manifest, which is a
//! reviewable diff. ADR-0017 makes exactly one such edit, and the argument for
//! it rests entirely on being one — "a single crate, a page long, containing
//! one call" is a very different proposition from "a precedent".
//!
//! Nothing about a manifest enforces that. Copying four lines into another
//! `Cargo.toml` is the easiest thing in the world, produces no warning, and
//! would read in review as consistency with an existing pattern. So the
//! boundary is a test: every workspace member inherits the workspace lints,
//! except this one, and adding a second exception fails here with a message
//! saying why.
//!
//! It lives beside the exception rather than somewhere central on purpose. The
//! argument and its enforcement should be hard to move apart.

use std::path::{Path, PathBuf};

/// The crate this test is defending the uniqueness of.
const QUARANTINED: &str = "seatbelt";

#[test]
fn every_other_crate_still_inherits_the_workspace_lints() {
    let mut exceptions = Vec::new();
    for manifest in manifests() {
        let text = std::fs::read_to_string(&manifest).expect("a manifest is readable");
        let name = crate_name(&text).unwrap_or_else(|| manifest.display().to_string());

        // `[lints] workspace = true` is what pulls in `unsafe_code = "forbid"`.
        // A crate without it is not covered, whether or not it says anything
        // about `unsafe` itself — an omission and an override are the same
        // hole.
        if !inherits_workspace_lints(&text) {
            exceptions.push(name);
        }
    }
    exceptions.sort();

    assert_eq!(
        exceptions,
        vec![QUARANTINED.to_owned()],
        "`unsafe` is forbidden workspace-wide (ADR-0002) with exactly one \
         exception (ADR-0017). A crate that does not inherit the workspace \
         lints is outside that rule, and adding a second one needs an ADR \
         rather than four lines copied from `crates/seatbelt/Cargo.toml`."
    );
}

#[test]
fn the_exception_is_still_as_small_as_its_argument_needs() {
    // ADR-0017 justifies the exception by its size — one call, a page of code.
    // A limit rather than a comment asking nicely, because the way this
    // decision goes wrong is not somebody deciding to abandon it, it is a
    // second useful thing being added here where `unsafe` happens to be
    // available.
    const ROOM: usize = 200;

    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut lines = 0;
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&source).expect("the crate has sources") {
        let path = entry.expect("a directory entry").path();
        if path.extension().is_some_and(|extension| extension == "rs") {
            lines += std::fs::read_to_string(&path)
                .expect("a source file is readable")
                .lines()
                .count();
            files.push(path);
        }
    }

    assert!(
        lines <= ROOM,
        "the quarantined crate has grown to {lines} lines across {files:?}. \
         Its whole justification is that it is small enough to read in one \
         sitting; if it needs to be bigger, that is a new argument and a new ADR."
    );
}

/// Every workspace member's manifest.
///
/// Read from the workspace root's `members` rather than walked, so a crate
/// added to the workspace and not to a hardcoded list here cannot slip past.
fn manifests() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the crate is two levels below the workspace root")
        .to_owned();

    let workspace = std::fs::read_to_string(root.join("Cargo.toml")).expect("a root manifest");
    let members = workspace
        .split_once("members = [")
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(list, _)| list.to_owned())
        .expect("the workspace lists its members");

    let mut manifests = Vec::new();
    for member in members.split(',') {
        let pattern = member.trim().trim_matches('"');
        if pattern.is_empty() {
            continue;
        }
        // `crates/*` and friends. Expanded here rather than pulling in a glob
        // crate for one line in one test.
        if let Some(directory) = pattern.strip_suffix("/*") {
            let entries = std::fs::read_dir(root.join(directory)).expect("a members directory");
            for entry in entries {
                let manifest = entry.expect("a directory entry").path().join("Cargo.toml");
                if manifest.is_file() {
                    manifests.push(manifest);
                }
            }
        } else {
            manifests.push(root.join(pattern).join("Cargo.toml"));
        }
    }
    assert!(
        manifests.len() > 5,
        "only found {} manifests, which means the members were not expanded \
         and this test is checking nothing",
        manifests.len()
    );
    manifests
}

/// Whether a manifest has a `[lints]` table saying `workspace = true`.
///
/// Written as a section scan rather than a substring search, which is the
/// version that reads fine and does not work: every manifest here says
/// `version.workspace = true`, so anything looking for `workspace = true`
/// anywhere in the file matches all of them — including the one crate this test
/// exists to single out.
fn inherits_workspace_lints(manifest: &str) -> bool {
    let mut in_lints = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            // `[lints.rust]` is a *different* table and does not inherit
            // anything, which is exactly how the exception is written.
            in_lints = line == "[lints]";
            continue;
        }
        if in_lints && line.replace(' ', "") == "workspace=true" {
            return true;
        }
    }
    false
}

fn crate_name(manifest: &str) -> Option<String> {
    manifest
        .lines()
        .find_map(|line| line.trim().strip_prefix("name = "))
        .map(|name| name.trim().trim_matches('"').to_owned())
}
