//! Every recorded finding, run through its target exactly as recorded.
//!
//! `corpus/<target>/` holds the inputs that found something, and until now
//! nothing ran them as they are. The fuzzer *mutates* them — they are seeds, so
//! a near miss keeps being explored — and `Session::calibrate` times only the
//! reference fixtures, deliberately, since a recorded crasher is garbage by
//! construction and would wreck the baseline. So a committed crasher was a good
//! starting point for the mutator and not, on its own, a test of anything: the
//! exact bytes that used to panic might never be run again.
//!
//! That is the difference between a corpus and a regression suite, and this
//! file is the second one. It is cheap — a few dozen small files — and it is
//! the check that actually fails if a fix is reverted.
//!
//! A finding here names the file, because the point of keeping the bytes is to
//! be able to open them.

use std::path::{Path, PathBuf};

use fuzz::{Target, run_once};

/// Runs every file in `corpus/<target>/` through the target.
fn replay(target: Target) {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("corpus")
        .join(target.name());

    let mut fonts = text::FontStore::new();
    let mut panicked = Vec::new();
    let mut ran = 0;

    for path in recorded(&directory) {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        ran += 1;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_once(target, &bytes, &mut fonts);
        }));
        if outcome.is_err() {
            panicked.push(path);
        }
    }

    assert!(
        panicked.is_empty(),
        "{} panicked on {} recorded input(s): {:#?}",
        target.name(),
        panicked.len(),
        panicked
    );
    // Not an error — most targets have found nothing yet, and a target with an
    // empty corpus directory is the normal state rather than a broken one. It
    // is printed so that a run claiming to have replayed a corpus says how big
    // the corpus was.
    println!("{}: replayed {ran} recorded input(s)", target.name());
}

/// The committed findings and seeds, skipping the two kinds of file that are
/// not test inputs.
///
/// Sorted, so a failure lists them the same way on every machine.
fn recorded(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path.file_name().and_then(|name| name.to_str());
            // `in-flight.bin` is whatever a run was holding when it died and is
            // not committed; a dotfile is repository furniture.
            path.is_file()
                && !name.is_some_and(|name| name == "in-flight.bin" || name.starts_with('.'))
        })
        .collect();
    paths.sort();
    paths
}

#[test]
fn every_recorded_input_still_parses() {
    // One test rather than six, because the whole set runs in well under a
    // second and six near-identical test bodies is six places to forget a
    // target when one is added.
    for target in Target::ALL {
        replay(target);
    }
}
