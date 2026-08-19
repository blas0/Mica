//! `mica-gpu` — the Metal renderer.
//!
//! Metal only. **No AppKit**: `mica-shell` owns the window and hands this crate
//! a `CAMetalLayer`, which is what makes the renderer exercisable without a
//! window server and keeps the dependency arrow pointing inward.

pub mod context;
pub mod frame;
pub mod grid;
pub mod overlay;
pub mod renderer;
pub mod search;

pub use frame::{Decision, FrameScheduler, FrameStats, Reason};
pub use grid::InstanceBuffers;
