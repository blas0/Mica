//! Rasterising one glyph into a CPU bitmap.
//!
//! Three paths, in the order they are tried:
//!
//! 1. **Synthesised** — box drawing and block elements, drawn by
//!    [`crate::boxdraw`] against the exact cell rectangle. Never from the font.
//! 2. **Greyscale** — a scalar the text stack has a glyph for. Drawn with
//!    `CTFontDrawGlyphs` into an 8-bit alpha context.
//! 3. **Colour** — anything else: emoji, grapheme clusters, and characters no
//!    text face has. Drawn with `CTLine` into a BGRA context so CoreText does
//!    its own font fallback and colour glyph selection.
//!
//! Font smoothing is switched **off** and antialiasing on. Smoothing is
//! subpixel rendering: it bakes an assumption about the display's subpixel
//! layout into the texture, which is wrong the moment the window moves to a
//! different monitor, and it makes a greyscale atlas impossible because the
//! coverage is per-channel. Greyscale antialiasing is what every modern macOS
//! app gets anyway.

use std::ffi::c_void;

use objc2_core_foundation::{
    kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks, CFAttributedString,
    kCFBooleanTrue, CFBoolean, CFDictionary, CFRetained, CFString, CGAffineTransform, CGPoint,
};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGGlyph, CGImageAlphaInfo,
};
use objc2_core_text::{
    kCTFontAttributeName, kCTForegroundColorFromContextAttributeName, CTFont, CTLine,
};

use crate::boxdraw;
use crate::fontset::{CellMetrics, FontSet, Style};

/// How a glyph's pixels are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// One byte of coverage. The renderer tints it with the cell's colour.
    Alpha8,
    /// Premultiplied BGRA. The renderer draws it as-is — an emoji is not
    /// tintable, and pretending otherwise produces grey blobs.
    Bgra8,
}

impl PixelFormat {
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            PixelFormat::Alpha8 => 1,
            PixelFormat::Bgra8 => 4,
        }
    }
}

/// A rasterised glyph, trimmed to its ink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    pub width: u16,
    pub height: u16,
    pub format: PixelFormat,
    pub data: Vec<u8>,
    /// Offset of the bitmap's left edge from the cell's left edge. Negative
    /// when the glyph overhangs, which italics routinely do.
    pub left: i16,
    /// Offset of the bitmap's top edge from the cell's top edge.
    pub top: i16,
}

impl Bitmap {
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Reinterprets a monochrome BGRA bitmap as single-channel coverage.
    ///
    /// Only sound because the pixels are premultiplied white, so every channel
    /// already equals alpha; the alpha byte alone carries the whole glyph.
    pub fn into_mask(self) -> Bitmap {
        if self.format != PixelFormat::Bgra8 {
            return self;
        }
        let data = self.data.chunks_exact(4).map(|px| px[3]).collect();
        Bitmap { format: PixelFormat::Alpha8, data, ..self }
    }

    /// Whether the bitmap has actual colour in it.
    ///
    /// The question this really answers is "is this a picture or a letter?".
    /// A colour glyph has hues; a text glyph drawn through the same BGRA path
    /// — CoreText's fallback for a character the text font lacks — is white
    /// coverage and nothing else. Alpha8 bitmaps are never polychrome by
    /// construction.
    pub fn is_polychrome(&self) -> bool {
        if self.format != PixelFormat::Bgra8 {
            return false;
        }
        self.data.chunks_exact(4).any(|px| {
            let (b, g, r, a) = (px[0], px[1], px[2], px[3]);
            // Compare against alpha, not against each other: the pixels are
            // premultiplied, so a half-transparent white is (128,128,128,128)
            // and only differs from grey by knowing what alpha was.
            a != 0 && (b != a || g != a || r != a)
        })
    }
}

/// Slack around the cell so an overhanging glyph is measured rather than
/// clipped. Trimming afterwards means the slack costs no atlas space.
fn padding(metrics: CellMetrics) -> i32 {
    (metrics.width as i32 / 2).max(2)
}

/// Draws a synthesised box-drawing or block glyph.
pub fn rasterize_synthesised(ch: char, metrics: CellMetrics, columns: u8) -> Option<Bitmap> {
    let width = metrics.width as u16 * columns.max(1) as u16;
    let coverage = boxdraw::render(ch, width, metrics.height)?;
    Some(Bitmap {
        width: coverage.width,
        height: coverage.height,
        format: PixelFormat::Alpha8,
        data: coverage.data,
        left: 0,
        top: 0,
    })
}

/// Draws one scalar character, choosing the greyscale or colour path.
pub fn rasterize_scalar(
    fonts: &FontSet,
    ch: char,
    style: Style,
    columns: u8,
) -> Option<Bitmap> {
    let metrics = fonts.metrics();
    if boxdraw::is_synthesised(ch) {
        return rasterize_synthesised(ch, metrics, columns);
    }
    if ch.is_whitespace() || ch.is_control() {
        return Some(Bitmap {
            width: 0,
            height: 0,
            format: PixelFormat::Alpha8,
            data: Vec::new(),
            left: 0,
            top: 0,
        });
    }
    match fonts.glyph_for(ch, style) {
        Some((font, glyph)) => draw_glyph_grey(font, glyph, metrics, columns),
        // Nothing in the text stack has it — CoreText's own fallback will.
        None => rasterize_cluster(fonts, &ch.to_string(), style, columns),
    }
}

/// Draws a grapheme cluster — a family emoji, a flag, a skin-tone sequence — as
/// **one** colour glyph occupying one cell.
pub fn rasterize_cluster(
    fonts: &FontSet,
    text: &str,
    style: Style,
    columns: u8,
) -> Option<Bitmap> {
    let metrics = fonts.metrics();
    // Prefer the emoji face when we have it; CoreText still substitutes for
    // anything it cannot draw, so this is a hint rather than a constraint.
    let font = fonts.emoji().unwrap_or_else(|| fonts.face(style));
    draw_line_colour(font, text, metrics, columns)
}

// --- the two CoreGraphics paths ---------------------------------------------

struct Canvas {
    context: CFRetained<CGContext>,
    data: Vec<u8>,
    width: usize,
    height: usize,
    format: PixelFormat,
}

impl Canvas {
    fn new(width: usize, height: usize, format: PixelFormat) -> Option<Canvas> {
        let bytes_per_row = width * format.bytes_per_pixel();
        let mut data = vec![0u8; bytes_per_row * height];

        let (space, info) = match format {
            PixelFormat::Alpha8 => (
                CGColorSpace::new_device_gray()?,
                CGImageAlphaInfo::None.0,
            ),
            PixelFormat::Bgra8 => (
                CGColorSpace::new_device_rgb()?,
                // Premultiplied-first plus little-endian 32-bit is BGRA, which
                // is what Metal's `bgra8Unorm` expects with no swizzle.
                CGImageAlphaInfo::PremultipliedFirst.0 | BYTE_ORDER_32_LITTLE,
            ),
        };

        // SAFETY: `data` outlives the context (both are owned by `Canvas` and
        // the context is dropped first), and the geometry matches the buffer.
        let context = unsafe {
            CGBitmapContextCreate(
                data.as_mut_ptr() as *mut c_void,
                width,
                height,
                8,
                bytes_per_row,
                Some(&space),
                info,
            )
        }?;

        CGContext::set_should_antialias(Some(&context), true);
        // See the module docs: subpixel smoothing bakes in a display's layout.
        CGContext::set_should_smooth_fonts(Some(&context), false);
        CGContext::set_allows_font_smoothing(Some(&context), false);
        CGContext::set_should_subpixel_position_fonts(Some(&context), true);
        CGContext::set_should_subpixel_quantize_fonts(Some(&context), false);
        // Identity: CoreText's own default text matrix, stated explicitly so a
        // future change to the context cannot quietly rotate the glyphs.
        CGContext::set_text_matrix(
            Some(&context),
            CGAffineTransform { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 0.0, ty: 0.0 },
        );

        Some(Canvas { context, data, width, height, format })
    }

    /// Trims to the ink bounding box.
    ///
    /// Every glyph is drawn into a padded box so overhang is measured rather
    /// than clipped; trimming here means that padding costs nothing in the
    /// atlas, which is the difference between a 512² page holding 300 glyphs
    /// and holding 1300.
    fn trim(self, origin_x: i32, origin_y: i32) -> Bitmap {
        let bpp = self.format.bytes_per_pixel();
        let opaque = |x: usize, y: usize| -> bool {
            let i = (y * self.width + x) * bpp;
            match self.format {
                PixelFormat::Alpha8 => self.data[i] != 0,
                // Premultiplied BGRA: the alpha byte is last in memory order.
                PixelFormat::Bgra8 => self.data[i + 3] != 0,
            }
        };

        let mut min_x = self.width;
        let mut min_y = self.height;
        let mut max_x = 0usize;
        let mut max_y = 0usize;
        for y in 0..self.height {
            for x in 0..self.width {
                if opaque(x, y) {
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x + 1);
                    max_y = max_y.max(y + 1);
                }
            }
        }

        if min_x >= max_x || min_y >= max_y {
            return Bitmap {
                width: 0,
                height: 0,
                format: self.format,
                data: Vec::new(),
                left: 0,
                top: 0,
            };
        }

        let (w, h) = (max_x - min_x, max_y - min_y);
        let mut out = Vec::with_capacity(w * h * bpp);
        for y in min_y..max_y {
            let start = (y * self.width + min_x) * bpp;
            out.extend_from_slice(&self.data[start..start + w * bpp]);
        }

        Bitmap {
            width: w as u16,
            height: h as u16,
            format: self.format,
            data: out,
            left: (min_x as i32 - origin_x) as i16,
            top: (min_y as i32 - origin_y) as i16,
        }
    }
}

/// `kCGBitmapByteOrder32Little`. Not exported by the binding, and it is a
/// stable ABI constant.
const BYTE_ORDER_32_LITTLE: u32 = 2 << 12;

fn draw_glyph_grey(
    font: &CTFont,
    glyph: CGGlyph,
    metrics: CellMetrics,
    columns: u8,
) -> Option<Bitmap> {
    let pad = padding(metrics);
    let cell_w = metrics.width as i32 * columns.max(1) as i32;
    let width = (cell_w + pad * 2) as usize;
    let height = (metrics.height as i32 + pad * 2) as usize;

    let canvas = Canvas::new(width, height, PixelFormat::Alpha8)?;
    // White on black: in a device-gray context with no alpha channel, the
    // luminance *is* the coverage.
    CGContext::set_gray_fill_color(Some(&canvas.context), 1.0, 1.0);

    // CoreGraphics puts the origin at the bottom left; the buffer is stored top
    // row first. Converting once here keeps every other coordinate in this
    // crate top-down.
    let baseline_y = height as f64 - pad as f64 - metrics.baseline as f64;
    let mut glyphs = [glyph];
    let mut positions = [CGPoint::new(pad as f64, baseline_y)];

    // SAFETY: one glyph, one position, both live for the call.
    unsafe {
        font.draw_glyphs(
            std::ptr::NonNull::new(glyphs.as_mut_ptr())?,
            std::ptr::NonNull::new(positions.as_mut_ptr())?,
            1,
            &canvas.context,
        );
    }

    Some(canvas.trim(pad, pad))
}

fn draw_line_colour(
    font: &CTFont,
    text: &str,
    metrics: CellMetrics,
    columns: u8,
) -> Option<Bitmap> {
    let pad = padding(metrics);
    let cell_w = metrics.width as i32 * columns.max(1) as i32;
    let width = (cell_w + pad * 2) as usize;
    let height = (metrics.height as i32 + pad * 2) as usize;

    let canvas = Canvas::new(width, height, PixelFormat::Bgra8)?;
    // White, for the same reason as the greyscale path: a monochrome glyph
    // coming out of here is coverage the renderer will tint with the cell's
    // colour, and the default fill is black. Drawing it black meant every
    // character the text font lacked — CJK, Greek, arrows — rendered as black
    // pixels whatever the theme said, which on a dark background is
    // indistinguishable from not rendering at all. A colour glyph ignores the
    // fill colour, so this costs emoji nothing.
    CGContext::set_rgb_fill_color(Some(&canvas.context), 1.0, 1.0, 1.0, 1.0);
    let line = attributed_line(font, text)?;

    let baseline_y = height as f64 - pad as f64 - metrics.baseline as f64;
    CGContext::set_text_position(Some(&canvas.context), pad as f64, baseline_y);
    // SAFETY: the line and the context are both live.
    unsafe { line.draw(&canvas.context) };

    let mut bitmap = canvas.trim(pad, pad);

    // An emoji drawn at the text size routinely exceeds its cells. Scaling it
    // down here rather than letting it bleed is what keeps "one cluster, one
    // cell" true.
    let downscaled = bitmap.width as i32 > cell_w;
    if downscaled {
        bitmap = downscale_to_width(&bitmap, cell_w as u16);
    }

    // **A picture gets centred; a letter keeps its baseline.**
    //
    // This path draws two quite different things. Grapheme clusters — a flag,
    // a family, a skin-tone sequence — are pictures, and CoreText lays them
    // out from the pen position using the emoji font's own metrics, which have
    // nothing to do with this grid. Measured at Menlo 14 on a 2x display: a
    // 28-pixel emoji landed at the left edge of a 34-pixel double-width cell,
    // six pixels of dead space to its right, occupying rows 1..29 where the
    // letter A occupies 4..23.
    //
    // But the same path is also the fallback for any scalar the text font does
    // not have — CJK, Greek, arrows. Those *are* letters, they do sit on the
    // baseline, and centring them would make a line of mixed Latin and CJK
    // stagger up and down. So the two cases are told apart by whether the
    // glyph actually has colour in it, which is the difference that matters and
    // costs one scan per glyph per session.
    if bitmap.is_polychrome() {
        bitmap.left = ((cell_w - bitmap.width as i32) / 2).max(0) as i16;
        bitmap.top = ((metrics.height as i32 - bitmap.height as i32) / 2).max(0) as i16;
        return Some(bitmap);
    }

    // Monochrome: this is a letter that came through the colour path because
    // the text font did not have it. Hand it back as a mask so the renderer
    // tints it with the cell's foreground like any other glyph — and so it
    // takes a quarter of the atlas space it would as BGRA.
    bitmap = bitmap.into_mask();
    if downscaled {
        // Monochrome and oversized: its offsets did not survive the scale, so
        // it has to be placed, but it is still text — centre it across the
        // cells and stand it on the baseline.
        bitmap.left = ((cell_w - bitmap.width as i32) / 2).max(0) as i16;
        bitmap.top = (metrics.baseline as i32 - bitmap.height as i32).max(0) as i16;
    }
    Some(bitmap)
}

fn attributed_line(font: &CTFont, text: &str) -> Option<CFRetained<CTLine>> {
    let cf_text = CFString::from_str(text);
    // Two attributes: the font, and permission to use the context's fill
    // colour.
    //
    // The second one is not optional decoration. `CTLine` does **not** consult
    // the context's fill colour by default — it draws in black unless the run
    // carries a colour or this flag. So every character the text font lacked
    // (CJK, Greek, arrows, anything reaching CoreText's fallback) was
    // rasterised as black pixels, cached that way, and drawn as a colour glyph
    // that ignores the cell's foreground. On a dark theme that is
    // indistinguishable from the character not rendering at all.
    let mut keys: [*const c_void; 2] = [
        unsafe { kCTFontAttributeName } as *const CFString as *const c_void,
        unsafe { kCTForegroundColorFromContextAttributeName } as *const CFString as *const c_void,
    ];
    let mut values: [*const c_void; 2] = [
        font as *const CTFont as *const c_void,
        unsafe { kCFBooleanTrue }? as *const CFBoolean as *const c_void,
    ];

    // SAFETY: the keys and values are borrowed for the length of the call;
    // `CFDictionaryCreate` retains what it stores.
    let attributes = unsafe {
        CFDictionary::new(
            None,
            keys.as_mut_ptr(),
            values.as_mut_ptr(),
            2,
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        )
    }?;
    let attributed = unsafe { CFAttributedString::new(None, Some(&cf_text), Some(&attributes)) }?;
    Some(unsafe { CTLine::with_attributed_string(&attributed) })
}

/// Box-filter downscale, preserving the aspect ratio.
///
/// Only ever applied to oversized colour glyphs, where it runs once per emoji
/// per session — a better filter would be imperceptible and more code.
fn downscale_to_width(source: &Bitmap, target_width: u16) -> Bitmap {
    let bpp = source.format.bytes_per_pixel();
    let ratio = target_width as f32 / source.width as f32;
    let target_height = ((source.height as f32 * ratio).round() as u16).max(1);
    let mut data = vec![0u8; target_width as usize * target_height as usize * bpp];

    for y in 0..target_height as usize {
        for x in 0..target_width as usize {
            // The source box this destination pixel averages.
            let x0 = x * source.width as usize / target_width as usize;
            let x1 = (((x + 1) * source.width as usize) / target_width as usize).max(x0 + 1);
            let y0 = y * source.height as usize / target_height as usize;
            let y1 = (((y + 1) * source.height as usize) / target_height as usize).max(y0 + 1);

            for channel in 0..bpp {
                let mut total = 0u32;
                let mut count = 0u32;
                for sy in y0..y1.min(source.height as usize) {
                    for sx in x0..x1.min(source.width as usize) {
                        total += source.data[(sy * source.width as usize + sx) * bpp + channel]
                            as u32;
                        count += 1;
                    }
                }
                if count > 0 {
                    data[(y * target_width as usize + x) * bpp + channel] =
                        (total / count) as u8;
                }
            }
        }
    }

    Bitmap {
        width: target_width,
        height: target_height,
        format: source.format,
        data,
        // Deliberately zero rather than the source offsets scaled by `ratio`.
        // The glyph shrank; the cell did not, so an offset measured against
        // the old size means nothing against the new one. Scaling them was how
        // a downscaled emoji ended up drifting toward the top-left of its
        // cell. The caller places the result — see `draw_line_colour`.
        left: 0,
        top: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fonts() -> FontSet {
        FontSet::resolve("Menlo", 14.0, 2.0)
    }

    /// The clusters worth checking: a plain emoji, a variation-selector heart,
    /// a regional-indicator flag, and a ZWJ family.
    const CLUSTERS: [(&str, &str); 4] = [
        ("emoji", "\u{1F600}"),
        ("heart", "\u{2764}\u{FE0F}"),
        ("flag", "\u{1F1EF}\u{1F1F5}"),
        ("family", "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}"),
    ];

    #[test]
    fn an_emoji_is_centred_in_the_cells_it_occupies() {
        // The reported bug: emoji were drawn hard against the left edge of
        // their double-width cell, with the slack all on one side. CoreText
        // lays the run out from the pen position using the emoji font's
        // metrics, which know nothing about this grid.
        let f = fonts();
        let m = f.metrics();
        let cell_w = m.width as i32 * 2;

        for (name, text) in CLUSTERS {
            let b = rasterize_cluster(&f, text, Style::REGULAR, 2)
                .unwrap_or_else(|| panic!("{name} produced nothing"));
            let left = b.left as i32;
            let right = cell_w - (left + b.width as i32);
            assert!(
                (left - right).abs() <= 1,
                "{name} is off-centre: {left}px of slack on the left, {right}px on the right"
            );
        }
    }

    #[test]
    fn an_emoji_stays_inside_its_own_cells() {
        // Bleeding into the neighbour is how an emoji ends up clipping the
        // character next to it.
        let f = fonts();
        let m = f.metrics();
        let cell_w = m.width as i32 * 2;

        for (name, text) in CLUSTERS {
            let b = rasterize_cluster(&f, text, Style::REGULAR, 2).unwrap();
            let (l, t) = (b.left as i32, b.top as i32);
            assert!(l >= 0, "{name} overhangs the left edge by {}px", -l);
            assert!(t >= 0, "{name} overhangs the top edge by {}px", -t);
            assert!(
                l + b.width as i32 <= cell_w,
                "{name} is {}px wider than its cells",
                l + b.width as i32 - cell_w
            );
            assert!(
                t + b.height as i32 <= m.height as i32,
                "{name} is {}px taller than its cell",
                t + b.height as i32 - m.height as i32
            );
        }
    }

    #[test]
    fn a_fallback_character_comes_back_as_a_tintable_mask() {
        // Anything the text font lacks reaches CoreText's fallback through the
        // same BGRA path as emoji. It is still a letter: it has to be coverage
        // so the renderer tints it with the cell's colour.
        //
        // Before this, `CTLine` drew it in black — it ignores the context's
        // fill colour unless told otherwise — and the renderer drew that black
        // bitmap as a colour glyph. On a dark theme it was invisible.
        let f = fonts();
        for (name, text) in [("cjk", "\u{6F22}"), ("greek", "\u{03B1}"), ("arrow", "\u{2192}")] {
            let Some(b) = rasterize_cluster(&f, text, Style::REGULAR, 1) else { continue };
            if b.is_empty() {
                continue;
            }
            assert_eq!(
                b.format,
                PixelFormat::Alpha8,
                "{name} came back as a colour glyph, so it will ignore the cell's colour"
            );
            assert!(!b.is_polychrome());
            assert!(b.data.iter().any(|&v| v > 0), "{name} rasterised to nothing");
        }
    }

    #[test]
    fn a_fallback_character_sits_on_the_text_line_rather_than_floating() {
        // Centring a letter would make a line of mixed Latin and CJK stagger
        // up and down, so the fallback path leaves CoreText's own placement
        // alone. Asserted loosely on purpose: CJK ideographs legitimately
        // descend below the baseline — the measured one ends three pixels
        // under it — and pinning an exact offset would be pinning the metrics
        // of whichever font macOS happens to substitute today.
        let f = fonts();
        let m = f.metrics();
        for text in ["\u{6F22}", "\u{03B1}", "\u{2192}"] {
            let Some(g) = rasterize_cluster(&f, text, Style::REGULAR, 2) else { continue };
            if g.is_empty() {
                continue;
            }
            let bottom = g.top as i32 + g.height as i32;
            assert!(
                bottom <= m.height as i32,
                "{text:?} extends {}px past the bottom of its cell",
                bottom - m.height as i32
            );
            assert!(
                bottom >= m.baseline as i32 - m.baseline as i32 / 3,
                "{text:?} ends at {bottom}, far above the baseline at {} — it is floating",
                m.baseline
            );
        }
    }

    #[test]
    fn an_emoji_is_still_drawn_in_colour() {
        // The other half: the fix must not turn emoji into monochrome masks.
        let f = fonts();
        let b = rasterize_cluster(&f, "\u{1F600}", Style::REGULAR, 2).unwrap();
        assert_eq!(b.format, PixelFormat::Bgra8);
        assert!(b.is_polychrome(), "the emoji lost its colour");
    }

    #[test]
    fn a_monochrome_bitmap_survives_the_round_trip_to_a_mask() {
        let bgra = Bitmap {
            width: 2,
            height: 1,
            format: PixelFormat::Bgra8,
            // Premultiplied white at two coverages.
            data: vec![255, 255, 255, 255, 128, 128, 128, 128],
            left: 3,
            top: 4,
        };
        assert!(!bgra.is_polychrome());
        let mask = bgra.into_mask();
        assert_eq!(mask.format, PixelFormat::Alpha8);
        assert_eq!(mask.data, vec![255, 128]);
        assert_eq!((mask.left, mask.top, mask.width), (3, 4, 2));
    }

    #[test]
    fn a_coloured_bitmap_is_recognised_as_one() {
        let red = Bitmap {
            width: 1,
            height: 1,
            format: PixelFormat::Bgra8,
            data: vec![0, 0, 255, 255],
            left: 0,
            top: 0,
        };
        assert!(red.is_polychrome());
        // And a fully transparent pixel is not colour, whatever is in the
        // other channels.
        let blank = Bitmap { data: vec![0, 0, 0, 0], ..red };
        assert!(!blank.is_polychrome());
    }

    #[test]
    fn a_letter_rasterises_to_something_visible() {
        let f = fonts();
        let bitmap = rasterize_scalar(&f, 'A', Style::REGULAR, 1).unwrap();
        assert!(!bitmap.is_empty(), "an A produced no pixels");
        assert_eq!(bitmap.format, PixelFormat::Alpha8);
        assert!(bitmap.data.iter().any(|&v| v > 0));
    }

    #[test]
    fn a_space_produces_no_pixels_and_costs_no_atlas_space() {
        let f = fonts();
        let bitmap = rasterize_scalar(&f, ' ', Style::REGULAR, 1).unwrap();
        assert!(bitmap.is_empty());
        assert!(bitmap.data.is_empty());
    }

    #[test]
    fn glyphs_are_trimmed_to_their_ink() {
        // A period is a small mark; if trimming is not working it would come
        // back the size of the whole padded box.
        let f = fonts();
        let m = f.metrics();
        let dot = rasterize_scalar(&f, '.', Style::REGULAR, 1).unwrap();
        assert!(dot.height < m.height, "'.' was not trimmed: {}x{}", dot.width, dot.height);
        assert!(dot.top > 0, "'.' should sit below the top of the cell");
    }

    #[test]
    fn a_tall_letter_is_taller_than_a_short_one() {
        let f = fonts();
        let tall = rasterize_scalar(&f, 'H', Style::REGULAR, 1).unwrap();
        let short = rasterize_scalar(&f, 'x', Style::REGULAR, 1).unwrap();
        assert!(tall.height > short.height, "H {} vs x {}", tall.height, short.height);
    }

    #[test]
    fn a_descender_reaches_below_the_baseline() {
        let f = fonts();
        let m = f.metrics();
        let g = rasterize_scalar(&f, 'g', Style::REGULAR, 1).unwrap();
        let bottom = g.top as i32 + g.height as i32;
        assert!(bottom > m.baseline as i32, "'g' does not descend: bottom {bottom}, baseline {}", m.baseline);
    }

    #[test]
    fn bold_lays_down_more_ink_than_regular() {
        let f = fonts();
        let ink = |style| {
            rasterize_scalar(&f, 'M', style, 1)
                .unwrap()
                .data
                .iter()
                .map(|&v| v as u32)
                .sum::<u32>()
        };
        assert!(ink(Style::new(true, false)) > ink(Style::REGULAR));
    }

    #[test]
    fn box_drawing_bypasses_the_font_entirely() {
        let f = fonts();
        let m = f.metrics();
        let bitmap = rasterize_scalar(&f, '\u{2500}', Style::REGULAR, 1).unwrap();
        // Synthesised glyphs are cell-exact and untrimmed, which is precisely
        // what makes them tile without seams.
        assert_eq!(bitmap.width, m.width);
        assert_eq!(bitmap.height, m.height);
        assert_eq!((bitmap.left, bitmap.top), (0, 0));
    }

    #[test]
    fn an_emoji_takes_the_colour_path() {
        let f = fonts();
        let bitmap = rasterize_scalar(&f, '\u{1F680}', Style::REGULAR, 2).unwrap();
        assert_eq!(bitmap.format, PixelFormat::Bgra8, "an emoji must not be greyscale");
        assert!(!bitmap.is_empty());
        // A colour glyph has more than one distinct colour in it; a fallback
        // box would not.
        let distinct: std::collections::HashSet<[u8; 3]> = bitmap
            .data
            .chunks_exact(4)
            .filter(|p| p[3] > 0)
            .map(|p| [p[0], p[1], p[2]])
            .collect();
        assert!(distinct.len() > 1, "the emoji came out monochrome");
    }

    #[test]
    fn a_zwj_cluster_is_one_glyph_in_one_cell() {
        // The family emoji: five scalars, one picture, one cell.
        let f = fonts();
        let m = f.metrics();
        let family = rasterize_cluster(
            &f,
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}",
            Style::REGULAR,
            2,
        )
        .unwrap();
        assert!(!family.is_empty());
        assert!(
            family.width <= m.width * 2,
            "the cluster is {} px wide, wider than its two cells",
            family.width
        );
    }

    #[test]
    fn a_flag_sequence_is_one_glyph() {
        let f = fonts();
        let flag = rasterize_cluster(&f, "\u{1F1FA}\u{1F1F8}", Style::REGULAR, 2).unwrap();
        assert!(!flag.is_empty());
        assert_eq!(flag.format, PixelFormat::Bgra8);
    }

    #[test]
    fn an_oversized_colour_glyph_is_scaled_into_its_cells() {
        let f = FontSet::resolve("Menlo", 14.0, 1.0);
        let m = f.metrics();
        let emoji = rasterize_scalar(&f, '\u{1F600}', Style::REGULAR, 2).unwrap();
        assert!(
            emoji.width <= m.width * 2,
            "emoji {} px exceeds its {} px of cells",
            emoji.width,
            m.width * 2
        );
    }

    #[test]
    fn downscaling_preserves_the_aspect_ratio_and_the_format() {
        let source = Bitmap {
            width: 40,
            height: 20,
            format: PixelFormat::Bgra8,
            data: vec![255u8; 40 * 20 * 4],
            left: 4,
            top: 2,
        };
        let scaled = downscale_to_width(&source, 20);
        assert_eq!((scaled.width, scaled.height), (20, 10));
        assert_eq!(scaled.format, PixelFormat::Bgra8);
        assert_eq!(scaled.data.len(), 20 * 10 * 4);
        assert!(scaled.data.iter().all(|&v| v == 255), "a flat image must stay flat");
    }

    #[test]
    fn a_double_width_character_gets_two_cells_of_room() {
        let f = fonts();
        let m = f.metrics();
        let cjk = rasterize_scalar(&f, '\u{4E16}', Style::REGULAR, 2).unwrap();
        assert!(!cjk.is_empty());
        assert!(cjk.width <= m.width * 2 + 4, "CJK glyph overflows its two cells");
        assert!(cjk.width > m.width, "CJK glyph should be wider than one cell");
    }
}
