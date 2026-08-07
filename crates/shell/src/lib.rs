//! The browser shell.
//!
//! Exposed as a library so that reference tests (ADR-0005) can drive the exact
//! pipeline the binary uses, rather than a reimplementation of it.

pub mod bookmarks;
pub mod chrome;
pub mod field;
pub mod history;
pub mod isolated;
pub mod render;
pub mod tabs;
pub mod viewport;
pub mod window;
