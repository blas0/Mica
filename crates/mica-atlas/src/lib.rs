//! `mica-atlas` — glyph rasterisation and the resident texture atlas.
//!
//! CoreText only. **No Metal here** — this crate produces CPU bitmaps and
//! rectangles; `mica-gpu` is what uploads them. Keeping the boundary means the
//! packing and the box-drawing geometry can be tested without a window server,
//! which is most of what can go subtly wrong in a terminal's text pipeline.

pub mod atlas;
pub mod boxdraw;
pub mod fontset;
pub mod packer;
pub mod raster;

pub use atlas::{Atlas, GlyphEntry, GlyphId, GlyphKey, Upload};
pub use fontset::{CellMetrics, FontSet, Style};
pub use raster::PixelFormat;
pub use packer::Rect;
