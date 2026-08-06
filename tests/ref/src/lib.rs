//! Reference tests: render each fixture and compare against a baseline image.
//!
//! There is one baseline set for all three platforms, not one per platform.
//! That is only sound because shaping and rasterisation are ours and the fonts
//! are bundled (ADR-0005) — so any per-platform difference is a bug, and this
//! is the test that catches it.
//!
//! To update baselines after an intentional rendering change:
//!
//! ```text
//! BLESS=1 cargo test -p reftests
//! ```
//!
//! Review the resulting image diff before committing. A blessed baseline is an
//! assertion that the new rendering is correct.
