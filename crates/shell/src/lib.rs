//! The browser shell.
//!
//! Exposed as a library so that reference tests (ADR-0005) can drive the exact
//! pipeline the binary uses, rather than a reimplementation of it.

pub mod a11y;
pub mod access;
pub mod bookmarks;
pub mod chrome;
pub mod devtools;
pub mod downloads;
pub mod dropdown;
pub mod field;
pub mod history;
pub mod icon;
pub mod isolated;
pub mod menu;
pub mod preview;
pub mod render;
pub mod scrollbar;
pub mod site_panel;
pub mod sites;
pub mod status;
pub mod tabs;
pub mod viewport;
pub mod visits;
pub mod window;
