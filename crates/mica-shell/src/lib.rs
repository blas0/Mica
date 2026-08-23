//! `mica-shell` — the AppKit layer.
//!
//! The only crate permitted to import AppKit, and the only one that knows a
//! window exists. Everything below it — terminal state, rasterisation,
//! rendering — is reachable without one, which is why the other three crates
//! can be tested headlessly.

pub mod app;
pub mod bindings;
pub mod config;
pub mod keys;
pub mod pane;
pub mod scroll;
pub mod surface;
pub mod view;
pub mod terminfo;
pub mod integration;
