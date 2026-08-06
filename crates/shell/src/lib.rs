//! The browser shell.
//!
//! Exposed as a library so that reference tests (ADR-0005) can drive the exact
//! pipeline the binary uses, rather than a reimplementation of it.

pub mod history;
pub mod render;
pub mod window;
