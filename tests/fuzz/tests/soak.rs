//! A short fuzzing pass on every target, run by `cargo test`.
//!
//! Small enough to sit in the ordinary test run — a few seconds per target —
//! because a fuzzer nobody runs finds nothing. The long soak is the same
//! harness with the count turned up: `cargo run -p fuzz -- --iterations 1e6`.
//!
//! The seed is fixed, so this is a regression test rather than a lottery: it
//! runs the same inputs on every machine and every commit, and a failure here
//! reproduces exactly. Catching *new* bugs is the soak's job, not this one's.

use fuzz::{Session, Target};

/// Iterations per target.
///
/// Chosen to keep the whole file inside a few seconds on CI's slowest runner.
/// Render is much heavier per input than the parsers, so it gets fewer.
fn iterations(target: Target) -> usize {
    match target {
        Target::Render => 150,
        Target::Image => 400,
        _ => 1500,
    }
}

fn soak(target: Target, seed: u64) {
    let mut session = Session::new(target);
    let report = session.run(seed, iterations(target));

    assert_eq!(report.iterations, iterations(target));
    assert!(
        report.crashes.is_empty(),
        "{} panicked on {} input(s); saved to {:?}",
        target.name(),
        report.crashes.len(),
        report.crashes
    );
    assert!(
        report.slow.is_empty(),
        "{} took longer than {:?} ({}x a {:?} baseline) on {:?}",
        target.name(),
        report.threshold,
        fuzz::SLOW_FACTOR,
        report.baseline,
        report.slow
    );
}

#[test]
fn html_survives_mutated_markup() {
    soak(Target::Html, 0x2000_0001);
}

#[test]
fn css_survives_mutated_stylesheets() {
    soak(Target::Css, 0x2000_0002);
}

#[test]
fn image_decoding_survives_mutated_files() {
    soak(Target::Image, 0x2000_0003);
}

#[test]
fn url_parsing_survives_mutated_urls() {
    soak(Target::Url, 0x2000_0004);
}

#[test]
fn the_whole_pipeline_survives_mutated_documents() {
    // The one that reaches cascade, layout, and paint. A parser can survive
    // garbage and still hand the next stage something it cannot cope with.
    soak(Target::Render, 0x2000_0005);
}
