//! Turning cells into instances.
//!
//! This is the only place that knows both what a [`Cell`] means and what the
//! GPU wants, and it contains no Metal at all — it produces plain `Vec`s of
//! `#[repr(C)]` structs. That is deliberate: the interesting failures in a
//! terminal renderer (a background quad drawn for a blank cell, a wide glyph
//! covering one column, an underline colour that silently falls back to the
//! foreground) are all decided here, and all of them are testable without a
//! GPU.
//!
//! ## Layout
//!
//! Every struct below has a twin in `shaders/mica.metal` and must stay
//! byte-identical to it. MSL aligns `float2` to 8 and `uchar4`/`ushort2` to 4,
//! so each struct is laid out wide-fields-first with explicit padding, and each
//! asserts its own size at compile time. A silent layout drift here does not
//! crash — it draws garbage — so the assertions are the whole defence.

use mica_atlas::atlas::{Atlas, GlyphEntry, GlyphKey};
use mica_atlas::fontset::{CellMetrics, Style};
use mica_atlas::raster::PixelFormat;
use mica_core::backend::{CursorShape, CursorState, RowRef};
use mica_core::cell::{Cell, CellFlags};
use mica_core::material::{Material, Rgb, Role};
use mica_core::motion::{CaretPresentation, TrailSample};
use mica_core::semantic::{Block, BlockStatus};
use mica_core::sidetable::SideTables;

/// Packed RGBA, ready for the `uchar4` the shaders unpack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(C)]
pub struct Rgba(pub [u8; 4]);

impl Rgba {
    pub const fn opaque(rgb: Rgb) -> Rgba {
        Rgba([rgb.r, rgb.g, rgb.b, 255])
    }

    pub fn with_alpha(rgb: Rgb, alpha: f32) -> Rgba {
        Rgba([rgb.r, rgb.g, rgb.b, (alpha.clamp(0.0, 1.0) * 255.0).round() as u8])
    }

    pub const fn alpha(self) -> u8 {
        self.0[3]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct BgInstance {
    pub cell: [u16; 2],
    pub width: u16,
    _pad: u16,
    pub color: Rgba,
}

impl BgInstance {
    pub fn new(column: u16, row: u16, width: u16, color: Rgba) -> BgInstance {
        BgInstance { cell: [column, row], width, _pad: 0, color }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct GlyphInstance {
    pub cell: [u16; 2],
    pub offset: [i16; 2],
    pub size: [u16; 2],
    pub uv_origin: [u16; 2],
    pub color: Rgba,
    pub page: u16,
    pub flags: u16,
}

/// Sample the colour page and do not tint. Mirrors `GLYPH_FLAG_COLOR`.
pub const GLYPH_FLAG_COLOR: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct RuleInstance {
    pub cell: [u16; 2],
    pub width: u16,
    pub style: u16,
    pub offset: i16,
    pub thickness: u16,
    pub color: Rgba,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct GutterInstance {
    pub row: u16,
    pub rows: u16,
    pub color: Rgba,
    pub width: f32,
    pub radius: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct ShapeInstance {
    pub origin: [f32; 2],
    pub size: [f32; 2],
    pub color: Rgba,
    pub radius: f32,
    pub softness: f32,
    _pad: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct DecayInstance {
    pub origin: [f32; 2],
    pub size: [f32; 2],
    pub direction: [f32; 2],
    pub color: Rgba,
    pub age: f32,
    pub radius: f32,
    _pad: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct QuadInstance {
    pub origin: [f32; 2],
    pub size: [f32; 2],
    pub fill: Rgba,
    pub border: Rgba,
    pub radius: f32,
    pub border_width: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct UiTextInstance {
    pub origin: [f32; 2],
    pub size: [u16; 2],
    pub uv_origin: [u16; 2],
    pub color: Rgba,
    pub page: u16,
    pub flags: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct Uniforms {
    pub viewport: [f32; 2],
    pub cell: [f32; 2],
    pub origin: [f32; 2],
    pub atlas_size: [f32; 2],
    pub time: f32,
    pub alpha: f32,
    _pad: [f32; 2],
}

impl Uniforms {
    pub fn new(
        viewport: (f32, f32),
        cell: CellMetrics,
        origin: (f32, f32),
        atlas_size: f32,
        time: f32,
        alpha: f32,
    ) -> Uniforms {
        Uniforms {
            viewport: [viewport.0, viewport.1],
            cell: [cell.width as f32, cell.height as f32],
            origin: [origin.0, origin.1],
            atlas_size: [atlas_size, atlas_size],
            time,
            alpha,
            _pad: [0.0; 2],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct SubstrateUniforms {
    pub background: [f32; 4],
    pub tint: [f32; 4],
    pub focus: [f32; 2],
    pub intensity: f32,
    pub vignette: f32,
}

// The whole defence against silent layout drift. If one of these fails, the
// matching struct in `mica.metal` moved and the fix is there, not here.
//
// **Sizes and field offsets, not alignment.** Rust and MSL genuinely disagree
// about alignment — `[u8; 4]` aligns to 1 here and `uchar4` aligns to 4 there —
// but the GPU reads these as a tightly packed array, so what has to match is
// the stride and where each field sits inside it. Asserting Rust's alignment
// would fail while the layout was in fact correct, which is worse than not
// asserting at all.
const _: () = {
    use core::mem::{offset_of, size_of};

    assert!(size_of::<BgInstance>() == 12);
    assert!(offset_of!(BgInstance, cell) == 0);
    assert!(offset_of!(BgInstance, width) == 4);
    assert!(offset_of!(BgInstance, color) == 8);

    assert!(size_of::<GlyphInstance>() == 24);
    assert!(offset_of!(GlyphInstance, cell) == 0);
    assert!(offset_of!(GlyphInstance, offset) == 4);
    assert!(offset_of!(GlyphInstance, size) == 8);
    assert!(offset_of!(GlyphInstance, uv_origin) == 12);
    assert!(offset_of!(GlyphInstance, color) == 16);
    assert!(offset_of!(GlyphInstance, page) == 20);
    assert!(offset_of!(GlyphInstance, flags) == 22);

    assert!(size_of::<RuleInstance>() == 16);
    assert!(offset_of!(RuleInstance, cell) == 0);
    assert!(offset_of!(RuleInstance, width) == 4);
    assert!(offset_of!(RuleInstance, style) == 6);
    assert!(offset_of!(RuleInstance, offset) == 8);
    assert!(offset_of!(RuleInstance, thickness) == 10);
    assert!(offset_of!(RuleInstance, color) == 12);

    assert!(size_of::<GutterInstance>() == 16);
    assert!(offset_of!(GutterInstance, row) == 0);
    assert!(offset_of!(GutterInstance, rows) == 2);
    assert!(offset_of!(GutterInstance, color) == 4);
    assert!(offset_of!(GutterInstance, width) == 8);
    assert!(offset_of!(GutterInstance, radius) == 12);

    assert!(size_of::<ShapeInstance>() == 32);
    assert!(offset_of!(ShapeInstance, origin) == 0);
    assert!(offset_of!(ShapeInstance, size) == 8);
    assert!(offset_of!(ShapeInstance, color) == 16);
    assert!(offset_of!(ShapeInstance, radius) == 20);
    assert!(offset_of!(ShapeInstance, softness) == 24);

    assert!(size_of::<DecayInstance>() == 40);
    assert!(offset_of!(DecayInstance, origin) == 0);
    assert!(offset_of!(DecayInstance, size) == 8);
    assert!(offset_of!(DecayInstance, direction) == 16);
    assert!(offset_of!(DecayInstance, color) == 24);
    assert!(offset_of!(DecayInstance, age) == 28);
    assert!(offset_of!(DecayInstance, radius) == 32);

    assert!(size_of::<QuadInstance>() == 32);
    assert!(offset_of!(QuadInstance, origin) == 0);
    assert!(offset_of!(QuadInstance, size) == 8);
    assert!(offset_of!(QuadInstance, fill) == 16);
    assert!(offset_of!(QuadInstance, border) == 20);
    assert!(offset_of!(QuadInstance, radius) == 24);
    assert!(offset_of!(QuadInstance, border_width) == 28);

    assert!(size_of::<UiTextInstance>() == 24);
    assert!(offset_of!(UiTextInstance, origin) == 0);
    assert!(offset_of!(UiTextInstance, size) == 8);
    assert!(offset_of!(UiTextInstance, uv_origin) == 12);
    assert!(offset_of!(UiTextInstance, color) == 16);
    assert!(offset_of!(UiTextInstance, page) == 20);
    assert!(offset_of!(UiTextInstance, flags) == 22);

    assert!(size_of::<Uniforms>() == 48);
    assert!(offset_of!(Uniforms, atlas_size) == 24);
    assert!(offset_of!(Uniforms, time) == 32);
    assert!(offset_of!(Uniforms, alpha) == 36);

    assert!(size_of::<SubstrateUniforms>() == 48);
    assert!(offset_of!(SubstrateUniforms, tint) == 16);
    assert!(offset_of!(SubstrateUniforms, focus) == 32);
    assert!(offset_of!(SubstrateUniforms, vignette) == 44);
};

/// One frame's worth of instances, reused across frames so the steady state
/// allocates nothing.
#[derive(Debug, Default)]
pub struct InstanceBuffers {
    pub backgrounds: Vec<BgInstance>,
    pub glyphs: Vec<GlyphInstance>,
    pub rules: Vec<RuleInstance>,
    pub gutters: Vec<GutterInstance>,
    pub shapes: Vec<ShapeInstance>,
    pub decays: Vec<DecayInstance>,
    pub quads: Vec<QuadInstance>,
    pub ui_text: Vec<UiTextInstance>,
}

impl InstanceBuffers {
    /// The grid's own instances: one per cell, rebuilt from terminal rows.
    ///
    /// Separated from [`InstanceBuffers::clear_transient`] because they have
    /// different lifetimes. The render pass clears the whole target every
    /// frame, so anything not re-emitted disappears — and the rows are only
    /// re-emitted when the terminal reports damage. A frame with no new output
    /// (a caret animation, for instance) must therefore keep the rows it
    /// already has, or it paints an empty screen. It did, and typing looked
    /// like the text was flashing away as you wrote it.
    pub fn clear_rows(&mut self) {
        self.backgrounds.clear();
        self.glyphs.clear();
        self.rules.clear();
    }

    /// Everything rebuilt from scratch every frame: the caret, its wake, the
    /// gutter, and the overlays. These move without the grid changing, which
    /// is exactly why they cannot be cached alongside it.
    pub fn clear_transient(&mut self) {
        self.gutters.clear();
        self.shapes.clear();
        self.decays.clear();
        self.quads.clear();
        self.ui_text.clear();
    }

    /// Empties without releasing capacity — the reason a steady-state frame
    /// does no allocation at all.
    pub fn clear(&mut self) {
        self.backgrounds.clear();
        self.glyphs.clear();
        self.rules.clear();
        self.gutters.clear();
        self.shapes.clear();
        self.decays.clear();
        self.quads.clear();
        self.ui_text.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.backgrounds.is_empty()
            && self.glyphs.is_empty()
            && self.rules.is_empty()
            && self.gutters.is_empty()
            && self.shapes.is_empty()
            && self.decays.is_empty()
            && self.quads.is_empty()
            && self.ui_text.is_empty()
    }

    pub fn total(&self) -> usize {
        self.backgrounds.len()
            + self.glyphs.len()
            + self.rules.len()
            + self.gutters.len()
            + self.shapes.len()
            + self.decays.len()
            + self.quads.len()
            + self.ui_text.len()
    }
}

/// Underline style, matching the shader's `style` field.
fn underline_style(flags: CellFlags) -> u16 {
    let masked = flags.intersection(CellFlags::UNDERLINE_MASK).bits() >> 8;
    match masked {
        1 => 1, // single
        2 => 2, // double
        3 => 3, // curly
        4 => 4, // dotted
        5 => 5, // dashed
        _ => 0,
    }
}

/// Resolves a cell's two colours through the theme.
///
/// Inverse and dim are applied here rather than in the shader because they are
/// *semantic* — dim means "70% of the way to the background", which only the
/// theme knows — and because doing it once per cell on the CPU is cheaper than
/// once per fragment on the GPU.
fn resolve_colors(cell: &Cell, material: &Material) -> (Rgb, Rgb) {
    let mut fg = material.resolve(cell.fg, Role::Foreground);
    let mut bg = material.resolve(cell.bg, Role::Background);

    if cell.flags.contains(CellFlags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.flags.contains(CellFlags::DIM) {
        fg = fg.lerp(bg, 0.45);
    }
    if cell.flags.contains(CellFlags::HIDDEN) {
        fg = bg;
    }
    (fg, bg)
}

fn style_of(cell: &Cell) -> Style {
    Style::new(cell.flags.contains(CellFlags::BOLD), cell.flags.contains(CellFlags::ITALIC))
}

/// Builds instances for one damaged row.
///
/// Only rows the terminal reported as dirty ever reach this function, which is
/// what keeps a 200 MB `cat` at the same cost as a single changed line.
pub struct RowBuilder<'a> {
    pub material: &'a Material,
    pub tables: &'a SideTables,
    pub metrics: CellMetrics,
    /// Alpha applied to the whole grid, used by the theme cross-fade.
    pub alpha: f32,
}

impl RowBuilder<'_> {
    pub fn build_row(&self, row: RowRef<'_>, atlas: &mut Atlas, out: &mut InstanceBuffers) {
        let default_bg = self.material.role(Role::Background);

        for (column, cell) in row.cells.iter().enumerate() {
            // The trailing half of a wide character carries no glyph of its
            // own; the leading half already covered both columns.
            if cell.flags.contains(CellFlags::WIDE_SPACER) {
                continue;
            }
            let column = column as u16;
            let (fg, bg) = resolve_colors(cell, self.material);
            let columns = cell.width.max(1).min(2);

            // A background quad is only worth submitting when it differs from
            // what the substrate already painted. On a plain build log that
            // drops essentially every background quad in the frame.
            if bg != default_bg {
                out.backgrounds.push(BgInstance {
                    cell: [column, row.index],
                    width: columns as u16,
                    _pad: 0,
                    color: Rgba::with_alpha(bg, self.alpha),
                });
            }

            self.push_rules(cell, column, row.index, fg, out);

            if cell.content.is_empty() || cell.flags.contains(CellFlags::HIDDEN) {
                continue;
            }
            let style = style_of(cell);
            let entry = match cell.content.as_scalar() {
                Some(ch) => atlas.glyph(
                    GlyphKey { id: mica_atlas::atlas::GlyphId::Scalar(ch), style, columns },
                    || None,
                ),
                None => {
                    let id = cell.content.as_cluster().unwrap_or(0);
                    atlas.glyph(GlyphKey::cluster(id, style, columns), || {
                        self.tables.graphemes.get(id).map(str::to_owned)
                    })
                }
            };
            let Some(entry) = entry.filter(|e| !e.is_blank()) else { continue };

            out.glyphs.push(glyph_instance(entry, column, row.index, fg, self.alpha));
        }
    }

    fn push_rules(
        &self,
        cell: &Cell,
        column: u16,
        row: u16,
        fg: Rgb,
        out: &mut InstanceBuffers,
    ) {
        let columns = cell.width.max(1).min(2) as u16;
        // Per-cell underline colour lives in a side table, and falls back to
        // the text colour when unset — which is the case for nearly every cell.
        let rule_color = self
            .tables
            .extras
            .get(cell.extra)
            .and_then(|e| e.underline_color)
            .map(|c| self.material.resolve(c, Role::Foreground))
            .unwrap_or(fg);
        let color = Rgba::with_alpha(rule_color, self.alpha);

        let style = underline_style(cell.flags);
        if style != 0 {
            out.rules.push(RuleInstance {
                cell: [column, row],
                width: columns,
                style,
                offset: self.metrics.underline_position,
                thickness: self.metrics.underline_thickness,
                color,
            });
        }
        if cell.flags.contains(CellFlags::STRIKETHROUGH) {
            out.rules.push(RuleInstance {
                cell: [column, row],
                width: columns,
                style: 1,
                offset: self.metrics.strikethrough_position,
                thickness: self.metrics.underline_thickness,
                color,
            });
        }
        if cell.flags.contains(CellFlags::OVERLINE) {
            out.rules.push(RuleInstance {
                cell: [column, row],
                width: columns,
                style: 1,
                offset: 0,
                thickness: self.metrics.underline_thickness,
                color,
            });
        }
    }
}

fn glyph_instance(
    entry: GlyphEntry,
    column: u16,
    row: u16,
    fg: Rgb,
    alpha: f32,
) -> GlyphInstance {
    GlyphInstance {
        cell: [column, row],
        offset: [entry.left, entry.top],
        size: [entry.rect.width, entry.rect.height],
        uv_origin: [entry.rect.x, entry.rect.y],
        color: Rgba::with_alpha(fg, alpha),
        page: entry.page,
        flags: match entry.format {
            PixelFormat::Bgra8 => GLYPH_FLAG_COLOR,
            PixelFormat::Alpha8 => 0,
        },
    }
}

/// Builds the caret.
///
/// Takes a [`CaretPresentation`] rather than a cell coordinate, because the
/// caret is not on the grid: it is somewhere between two cells, possibly
/// stretched, possibly half-faded. The physics that decides all of that lives
/// in `mica-core::motion` and knows nothing about pixels; this function is the
/// one place the two meet.
///
/// Returns `None` when the caret is hidden or fully blinked out, so an
/// invisible caret contributes no instance and therefore no draw call.
pub fn cursor_shape(
    cursor: CursorState,
    caret: CaretPresentation,
    metrics: CellMetrics,
    material: &Material,
    origin: (f32, f32),
    focused: bool,
) -> Option<ShapeInstance> {
    if !cursor.visible || caret.alpha <= 0.004 {
        return None;
    }
    let (cw, ch) = (metrics.width as f32, metrics.height as f32);
    // Fractional cell coordinates: this is the sub-cell interpolation, and it
    // is the whole reason the caret glides rather than stepping.
    let x = origin.0 + caret.position[0] * cw;
    let y = origin.1 + caret.position[1] * ch;

    let (size, offset) = match cursor.shape {
        CursorShape::Block => ([cw, ch], [0.0, 0.0]),
        CursorShape::Underline => {
            let thickness = (ch / 8.0).max(1.0);
            ([cw, thickness], [0.0, ch - thickness])
        }
        CursorShape::Bar => ([(cw / 6.0).max(1.0), ch], [0.0, 0.0]),
    };

    // Squash scales about the caret's centre, so a stretched caret grows in
    // both directions rather than sliding off its own position.
    let scaled = [size[0] * caret.scale[0], size[1] * caret.scale[1]];
    let recentre = [(size[0] - scaled[0]) * 0.5, (size[1] - scaled[1]) * 0.5];

    // An unfocused window shows a hollow caret in most terminals; here it is
    // the same shape at low alpha, which reads the same and costs one pipeline
    // instead of two.
    let focus_alpha = if focused { 1.0 } else { 0.35 };

    Some(ShapeInstance {
        origin: [x + offset[0] + recentre[0], y + offset[1] + recentre[1]],
        size: scaled,
        color: Rgba::with_alpha(material.role(Role::Accent), focus_alpha * caret.alpha),
        radius: 1.0,
        softness: caret.softness,
        _pad: 0.0,
    })
}

/// Builds the caret's wake.
///
/// One instance per live trail sample, appended to `out`. An unmoving caret
/// has no samples and this appends nothing, which is what keeps the decay
/// pipeline free when it is not in use.
pub fn caret_decay<'a>(
    cursor: CursorState,
    samples: impl Iterator<Item = &'a TrailSample>,
    metrics: CellMetrics,
    material: &Material,
    origin: (f32, f32),
    out: &mut Vec<DecayInstance>,
) {
    if !cursor.visible {
        return;
    }
    let (cw, ch) = (metrics.width as f32, metrics.height as f32);
    let (size, offset) = match cursor.shape {
        CursorShape::Block => ([cw, ch], [0.0, 0.0]),
        CursorShape::Underline => {
            let thickness = (ch / 8.0).max(1.0);
            ([cw, thickness], [0.0, ch - thickness])
        }
        CursorShape::Bar => ([(cw / 6.0).max(1.0), ch], [0.0, 0.0]),
    };

    for sample in samples {
        // The shader fades by age as well, but the alpha is pre-attenuated
        // here so a trail never competes with the caret itself for attention.
        let alpha = (1.0 - sample.age) * TRAIL_PEAK_ALPHA;
        out.push(DecayInstance {
            origin: [
                origin.0 + sample.position[0] * cw + offset[0],
                origin.1 + sample.position[1] * ch + offset[1],
            ],
            size,
            direction: sample.direction,
            color: Rgba::with_alpha(material.role(Role::Accent), alpha),
            age: sample.age,
            radius: 1.0,
            _pad: 0.0,
        });
    }
}

/// How opaque the freshest trail sample is allowed to be.
///
/// Well under the caret's own alpha on purpose: a wake that reads as bright as
/// the caret stops being a wake and becomes a row of duplicate carets.
const TRAIL_PEAK_ALPHA: f32 = 0.34;

/// Builds the gutter marks for the visible command blocks.
///
/// A failed block keeps its mark until it is cleared, which is why this reads
/// block state rather than cell state.
pub fn block_gutters(
    blocks: &[Block],
    first_visible_row: u64,
    visible_rows: u16,
    material: &Material,
    metrics: CellMetrics,
    out: &mut Vec<GutterInstance>,
) {
    let width = (metrics.width as f32 / 4.0).max(2.0);
    for block in blocks {
        let start = block.start_row;
        let end = block.end_row.unwrap_or(start + 1).max(start + 1);
        if end <= first_visible_row {
            continue;
        }
        let last_visible = first_visible_row + visible_rows as u64;
        if start >= last_visible {
            continue;
        }

        let top = start.saturating_sub(first_visible_row) as u16;
        let bottom = (end - first_visible_row).min(visible_rows as u64) as u16;
        let rows = bottom.saturating_sub(top).max(1);

        let (role, alpha) = match block.status {
            BlockStatus::Failed(_) => (Role::Error, 1.0),
            BlockStatus::Running => (Role::Accent, 0.9),
            BlockStatus::Succeeded => (Role::Dim, 0.5),
            BlockStatus::Prompting | BlockStatus::Unknown => (Role::Dim, 0.3),
        };

        out.push(GutterInstance {
            row: top,
            rows,
            color: Rgba::with_alpha(material.role(role), alpha),
            width,
            radius: width / 2.0,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mica_atlas::fontset::FontSet;
    use mica_core::cell::{CellContent, Color};
    use mica_core::material::{builtin, Theme};

    fn material() -> Material {
        Material::from_theme(&builtin("slate").unwrap()).unwrap()
    }

    fn atlas() -> Atlas {
        Atlas::new(FontSet::resolve("Menlo", 13.0, 2.0))
    }

    fn text_row(text: &str, cols: usize) -> Vec<Cell> {
        let mut cells = vec![Cell::EMPTY; cols];
        for (i, ch) in text.chars().enumerate().take(cols) {
            cells[i] = Cell::new(
                CellContent::scalar(ch),
                Color::DEFAULT,
                Color::DEFAULT,
                CellFlags::EMPTY,
            );
        }
        cells
    }

    fn build(cells: &[Cell], tables: &SideTables) -> (InstanceBuffers, Atlas) {
        let material = material();
        let mut atlas = atlas();
        let metrics = atlas.metrics();
        let builder = RowBuilder { material: &material, tables, metrics, alpha: 1.0 };
        let mut out = InstanceBuffers::default();
        builder.build_row(RowRef { index: 0, cells, wrapped: false }, &mut atlas, &mut out);
        (out, atlas)
    }

    #[test]
    fn a_plain_line_produces_one_glyph_per_visible_character_and_no_backgrounds() {
        // The common case, and the one that has to be cheap: default colours
        // mean the substrate already painted the background.
        let tables = SideTables::new();
        let (out, _) = build(&text_row("hello world", 40), &tables);
        assert_eq!(out.glyphs.len(), 10, "h e l l o w o r l d, spaces excluded");
        assert!(out.backgrounds.is_empty(), "default backgrounds must not be drawn");
        assert!(out.rules.is_empty());
    }

    #[test]
    fn a_coloured_background_does_produce_a_quad() {
        let tables = SideTables::new();
        let mut cells = text_row("x", 4);
        cells[0].bg = Color::palette(1);
        let (out, _) = build(&cells, &tables);
        assert_eq!(out.backgrounds.len(), 1);
        assert_eq!(out.backgrounds[0].cell, [0, 0]);
    }

    #[test]
    fn ansi_colours_are_resolved_through_the_theme() {
        let tables = SideTables::new();
        let m = material();
        let mut cells = text_row("x", 4);
        cells[0].fg = Color::palette(1); // red == the theme's error role
        let (out, _) = build(&cells, &tables);
        assert_eq!(out.glyphs[0].color, Rgba::opaque(m.role(Role::Error)));
    }

    #[test]
    fn inverse_swaps_the_two_colours() {
        let m = material();
        let mut cell = Cell::new(
            CellContent::scalar('x'),
            Color::DEFAULT,
            Color::DEFAULT,
            CellFlags::INVERSE,
        );
        cell.width = 1;
        let (fg, bg) = resolve_colors(&cell, &m);
        assert_eq!(fg, m.role(Role::Background));
        assert_eq!(bg, m.role(Role::Foreground));
    }

    #[test]
    fn inverse_produces_a_background_quad_where_a_plain_cell_would_not() {
        let tables = SideTables::new();
        let mut cells = text_row("x", 4);
        cells[0].flags.insert(CellFlags::INVERSE);
        let (out, _) = build(&cells, &tables);
        assert_eq!(out.backgrounds.len(), 1, "an inverted cell needs its background drawn");
    }

    #[test]
    fn dim_moves_the_text_toward_the_background_without_reaching_it() {
        let m = material();
        let cell = Cell::new(
            CellContent::scalar('x'),
            Color::DEFAULT,
            Color::DEFAULT,
            CellFlags::DIM,
        );
        let (fg, _) = resolve_colors(&cell, &m);
        assert_ne!(fg, m.role(Role::Foreground), "dim did nothing");
        assert_ne!(fg, m.role(Role::Background), "dim made the text invisible");
    }

    #[test]
    fn hidden_text_is_drawn_in_the_background_colour_and_not_at_all() {
        let tables = SideTables::new();
        let mut cells = text_row("secret", 10);
        for cell in &mut cells[..6] {
            cell.flags.insert(CellFlags::HIDDEN);
        }
        let (out, _) = build(&cells, &tables);
        assert!(out.glyphs.is_empty(), "hidden text must not reach the atlas");
    }

    #[test]
    fn every_underline_style_maps_to_its_own_shader_branch() {
        for (flag, expected) in [
            (CellFlags::UNDERLINE_SINGLE, 1),
            (CellFlags::UNDERLINE_DOUBLE, 2),
            (CellFlags::UNDERLINE_CURLY, 3),
            (CellFlags::UNDERLINE_DOTTED, 4),
            (CellFlags::UNDERLINE_DASHED, 5),
        ] {
            assert_eq!(underline_style(flag), expected, "{flag:?}");
        }
        assert_eq!(underline_style(CellFlags::BOLD), 0, "bold is not an underline");
    }

    #[test]
    fn strikethrough_and_underline_are_two_separate_rules() {
        let tables = SideTables::new();
        let mut cells = text_row("x", 4);
        cells[0].flags.insert(CellFlags::UNDERLINE_SINGLE);
        cells[0].flags.insert(CellFlags::STRIKETHROUGH);
        let (out, _) = build(&cells, &tables);
        assert_eq!(out.rules.len(), 2);
        assert_ne!(out.rules[0].offset, out.rules[1].offset, "both rules sit at the same height");
    }

    #[test]
    fn a_per_cell_underline_colour_overrides_the_text_colour() {
        use mica_core::sidetable::Extras;
        let mut tables = SideTables::new();
        let underline = Color::rgb(1, 2, 3);
        let extra = tables.extras.intern(Extras { underline_color: Some(underline), hyperlink: None });

        let mut cells = text_row("x", 4);
        cells[0].flags.insert(CellFlags::UNDERLINE_CURLY);
        cells[0].extra = extra;

        let (out, _) = build(&cells, &tables);
        assert_eq!(out.rules.len(), 1);
        assert_eq!(out.rules[0].color, Rgba::opaque(Rgb::new(1, 2, 3)));
        assert_ne!(out.rules[0].color, out.glyphs[0].color, "the rule took the text colour");
    }

    #[test]
    fn a_wide_character_produces_one_instance_two_cells_wide() {
        let tables = SideTables::new();
        let mut cells = vec![Cell::EMPTY; 6];
        cells[0] = Cell::new(
            CellContent::scalar('\u{4E16}'),
            Color::palette(2),
            Color::palette(4),
            CellFlags::EMPTY,
        );
        cells[0].width = 2;
        cells[1].flags.insert(CellFlags::WIDE_SPACER);
        cells[1].bg = Color::palette(4);

        let (out, _) = build(&cells, &tables);
        assert_eq!(out.glyphs.len(), 1, "the spacer must not produce its own glyph");
        assert_eq!(out.backgrounds.len(), 1, "the spacer must not produce its own background");
        assert_eq!(out.backgrounds[0].width, 2, "the background must span both columns");
    }

    #[test]
    fn an_emoji_is_flagged_as_colour_so_the_shader_does_not_tint_it() {
        let mut tables = SideTables::new();
        let id = tables.graphemes.intern("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}");

        let mut cells = vec![Cell::EMPTY; 6];
        cells[0] = Cell::new(
            CellContent::cluster(id),
            Color::DEFAULT,
            Color::DEFAULT,
            CellFlags::EMPTY,
        );
        cells[0].width = 2;
        cells[1].flags.insert(CellFlags::WIDE_SPACER);

        let (out, _) = build(&cells, &tables);
        assert_eq!(out.glyphs.len(), 1);
        assert_eq!(
            out.glyphs[0].flags & GLYPH_FLAG_COLOR,
            GLYPH_FLAG_COLOR,
            "a cluster must take the colour path"
        );
    }

    #[test]
    fn a_blank_row_produces_nothing_at_all() {
        let tables = SideTables::new();
        let (out, _) = build(&vec![Cell::EMPTY; 80], &tables);
        assert!(out.is_empty(), "an empty row produced {} instances", out.total());
    }

    #[test]
    fn clearing_keeps_capacity_so_a_steady_frame_does_not_allocate() {
        let mut buffers = InstanceBuffers::default();
        buffers.glyphs.extend(std::iter::repeat_n(GlyphInstance::default(), 500));
        let capacity = buffers.glyphs.capacity();
        buffers.clear();
        assert!(buffers.is_empty());
        assert_eq!(buffers.glyphs.capacity(), capacity, "clear() released the buffer");
    }

    /// A caret sitting still and fully visible at the given cell.
    fn at_rest(column: f32, line: f32) -> CaretPresentation {
        CaretPresentation {
            position: [column, line],
            scale: [1.0, 1.0],
            softness: 0.0,
            alpha: 1.0,
        }
    }

    #[test]
    fn a_hidden_cursor_produces_no_shape() {
        let m = material();
        let metrics = atlas().metrics();
        let mut cursor = CursorState::default();
        cursor.visible = false;
        assert!(cursor_shape(cursor, at_rest(0.0, 0.0), metrics, &m, (0.0, 0.0), true).is_none());
    }

    #[test]
    fn a_blinked_out_caret_produces_no_shape() {
        // Alpha rather than a boolean, because the blink now fades: the
        // instance has to disappear at the bottom of the fade and not before,
        // or the caret's last visible frame is a faint ghost that never clears.
        let m = material();
        let metrics = atlas().metrics();
        let cursor = CursorState::default();
        let faded = |alpha| CaretPresentation { alpha, ..at_rest(0.0, 0.0) };

        assert!(cursor_shape(cursor, faded(0.0), metrics, &m, (0.0, 0.0), true).is_none());
        assert!(cursor_shape(cursor, faded(0.5), metrics, &m, (0.0, 0.0), true).is_some());
        assert!(cursor_shape(cursor, faded(1.0), metrics, &m, (0.0, 0.0), true).is_some());
    }

    #[test]
    fn each_cursor_shape_has_its_own_geometry() {
        let m = material();
        let metrics = atlas().metrics();
        let at = |shape| {
            cursor_shape(
                CursorState { shape, ..CursorState::default() },
                at_rest(0.0, 0.0),
                metrics,
                &m,
                (0.0, 0.0),
                true,
            )
            .unwrap()
        };
        let block = at(CursorShape::Block);
        let underline = at(CursorShape::Underline);
        let bar = at(CursorShape::Bar);

        assert_eq!(block.size, [metrics.width as f32, metrics.height as f32]);
        assert!(underline.size[1] < block.size[1], "the underline caret is not short");
        assert!(underline.origin[1] > block.origin[1], "the underline caret is not at the bottom");
        assert!(bar.size[0] < block.size[0], "the bar caret is not narrow");
        assert_eq!(bar.size[1], block.size[1], "the bar caret should be full height");
    }

    #[test]
    fn the_caret_lands_where_the_physics_put_it_not_where_the_cursor_is() {
        // The caret's cell and the terminal's cursor cell are different things
        // while an animation is in flight. Reading `cursor.column` here was
        // the old behaviour and would make every motion style a no-op.
        let m = material();
        let metrics = atlas().metrics();
        let cursor = CursorState { line: 3, column: 7, ..CursorState::default() };
        let shape =
            cursor_shape(cursor, at_rest(2.0, 1.0), metrics, &m, (10.0, 20.0), true).unwrap();
        assert_eq!(shape.origin[0], 10.0 + 2.0 * metrics.width as f32);
        assert_eq!(shape.origin[1], 20.0 + 1.0 * metrics.height as f32);
    }

    #[test]
    fn a_caret_between_two_cells_is_drawn_between_them() {
        let m = material();
        let metrics = atlas().metrics();
        let cursor = CursorState::default();
        let left = cursor_shape(cursor, at_rest(4.0, 0.0), metrics, &m, (0.0, 0.0), true).unwrap();
        let half = cursor_shape(cursor, at_rest(4.5, 0.0), metrics, &m, (0.0, 0.0), true).unwrap();
        let right = cursor_shape(cursor, at_rest(5.0, 0.0), metrics, &m, (0.0, 0.0), true).unwrap();

        assert!(half.origin[0] > left.origin[0] && half.origin[0] < right.origin[0]);
        assert_eq!(half.origin[0] - left.origin[0], metrics.width as f32 * 0.5);
    }

    #[test]
    fn a_squashed_caret_keeps_its_centre() {
        // Scaling from the origin instead of the centre makes a stretching
        // caret appear to lunge forward, which reads as a jump rather than a
        // squash.
        let m = material();
        let metrics = atlas().metrics();
        let cursor = CursorState::default();
        let centre = |shape: &ShapeInstance| {
            [shape.origin[0] + shape.size[0] * 0.5, shape.origin[1] + shape.size[1] * 0.5]
        };

        let plain = cursor_shape(cursor, at_rest(4.0, 2.0), metrics, &m, (0.0, 0.0), true).unwrap();
        let squashed = cursor_shape(
            cursor,
            CaretPresentation { scale: [1.6, 0.625], ..at_rest(4.0, 2.0) },
            metrics,
            &m,
            (0.0, 0.0),
            true,
        )
        .unwrap();

        assert!(squashed.size[0] > plain.size[0] && squashed.size[1] < plain.size[1]);
        let (a, b) = (centre(&plain), centre(&squashed));
        assert!((a[0] - b[0]).abs() < 0.01 && (a[1] - b[1]).abs() < 0.01, "{a:?} vs {b:?}");
    }

    #[test]
    fn softness_reaches_the_instance_the_shader_reads() {
        let m = material();
        let metrics = atlas().metrics();
        let shape = cursor_shape(
            CursorState::default(),
            CaretPresentation { softness: 0.6, ..at_rest(0.0, 0.0) },
            metrics,
            &m,
            (0.0, 0.0),
            true,
        )
        .unwrap();
        assert_eq!(shape.softness, 0.6);
    }

    #[test]
    fn an_unfocused_cursor_is_dimmer_but_still_present() {
        let m = material();
        let metrics = atlas().metrics();
        let cursor = CursorState::default();
        let focused = cursor_shape(cursor, at_rest(0.0, 0.0), metrics, &m, (0.0, 0.0), true).unwrap();
        let unfocused =
            cursor_shape(cursor, at_rest(0.0, 0.0), metrics, &m, (0.0, 0.0), false).unwrap();
        assert!(unfocused.color.alpha() < focused.color.alpha());
        assert!(unfocused.color.alpha() > 0);
    }

    #[test]
    fn a_still_caret_contributes_no_decay_instances() {
        // The trail costs nothing when it is not in use — the property that
        // lets the decay pipeline exist without being paid for every frame.
        let m = material();
        let metrics = atlas().metrics();
        let mut out = Vec::new();
        caret_decay(CursorState::default(), [].iter(), metrics, &m, (0.0, 0.0), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn each_trail_sample_becomes_one_fading_instance() {
        let m = material();
        let metrics = atlas().metrics();
        let samples = [
            TrailSample { position: [2.0, 1.0], direction: [1.0, 0.0], age: 0.1 },
            TrailSample { position: [3.0, 1.0], direction: [1.0, 0.0], age: 0.8 },
        ];
        let mut out = Vec::new();
        caret_decay(CursorState::default(), samples.iter(), metrics, &m, (0.0, 0.0), &mut out);

        assert_eq!(out.len(), 2);
        assert_eq!(out[0].origin[0], 2.0 * metrics.width as f32);
        assert!(out[0].age < out[1].age);
        assert!(
            out[0].color.alpha() > out[1].color.alpha(),
            "the older sample was not the fainter one"
        );
    }

    #[test]
    fn the_trail_never_outshines_the_caret() {
        let m = material();
        let metrics = atlas().metrics();
        let caret =
            cursor_shape(CursorState::default(), at_rest(0.0, 0.0), metrics, &m, (0.0, 0.0), true)
                .unwrap();
        let samples = [TrailSample { position: [0.0, 0.0], direction: [1.0, 0.0], age: 0.0 }];
        let mut out = Vec::new();
        caret_decay(CursorState::default(), samples.iter(), metrics, &m, (0.0, 0.0), &mut out);
        assert!(
            out[0].color.alpha() < caret.color.alpha(),
            "the freshest trail sample is as bright as the caret itself"
        );
    }

    #[test]
    fn a_hidden_cursor_has_no_trail_either() {
        let m = material();
        let metrics = atlas().metrics();
        let samples = [TrailSample { position: [0.0, 0.0], direction: [1.0, 0.0], age: 0.0 }];
        let mut out = Vec::new();
        let hidden = CursorState { visible: false, ..CursorState::default() };
        caret_decay(hidden, samples.iter(), metrics, &m, (0.0, 0.0), &mut out);
        assert!(out.is_empty(), "a hidden caret left a visible wake");
    }

    #[test]
    fn a_failed_block_gets_a_gutter_mark_in_the_error_colour() {
        let m = material();
        let metrics = atlas().metrics();
        let block = Block {
            id: 1,
            start_row: 5,
            output_row: Some(6),
            end_row: Some(9),
            command: Some("false".into()),
            cwd: None,
            status: BlockStatus::Failed(1),
            started_ms: Some(0),
            duration_ms: Some(4),
            folded: false,
        };
        let mut out = Vec::new();
        block_gutters(&[block], 0, 24, &m, metrics, &mut out);

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].row, 5);
        assert_eq!(out[0].rows, 4);
        assert_eq!(out[0].color, Rgba::opaque(m.role(Role::Error)));
    }

    #[test]
    fn blocks_scrolled_out_of_view_produce_no_marks() {
        let m = material();
        let metrics = atlas().metrics();
        let block = |start, end| Block {
            id: 1,
            start_row: start,
            output_row: None,
            end_row: Some(end),
            command: None,
            cwd: None,
            status: BlockStatus::Succeeded,
            started_ms: None,
            duration_ms: None,
            folded: false,
        };
        let mut out = Vec::new();
        // Entirely above the viewport, and entirely below it.
        block_gutters(&[block(0, 5), block(100, 105)], 10, 24, &m, metrics, &mut out);
        assert!(out.is_empty(), "off-screen blocks produced {} marks", out.len());
    }

    #[test]
    fn a_block_straddling_the_top_of_the_viewport_is_clipped_not_dropped() {
        let m = material();
        let metrics = atlas().metrics();
        let block = Block {
            id: 1,
            start_row: 5,
            output_row: None,
            end_row: Some(20),
            command: None,
            cwd: None,
            status: BlockStatus::Running,
            started_ms: None,
            duration_ms: None,
            folded: false,
        };
        let mut out = Vec::new();
        block_gutters(&[block], 10, 24, &m, metrics, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].row, 0, "the mark should start at the top of the viewport");
        assert_eq!(out[0].rows, 10);
    }

    #[test]
    fn the_theme_alpha_reaches_every_instance() {
        // The cross-fade works by fading the whole grid, so anything that
        // ignores `alpha` would visibly pop during a theme change.
        let tables = SideTables::new();
        let material = material();
        let mut atlas = atlas();
        let metrics = atlas.metrics();
        let mut cells = text_row("x", 4);
        cells[0].bg = Color::palette(2);
        cells[0].flags.insert(CellFlags::UNDERLINE_SINGLE);

        let builder = RowBuilder { material: &material, tables: &tables, metrics, alpha: 0.5 };
        let mut out = InstanceBuffers::default();
        builder.build_row(
            RowRef { index: 0, cells: &cells, wrapped: false },
            &mut atlas,
            &mut out,
        );

        assert_eq!(out.backgrounds[0].color.alpha(), 128);
        assert_eq!(out.glyphs[0].color.alpha(), 128);
        assert_eq!(out.rules[0].color.alpha(), 128);
    }

    #[test]
    fn a_theme_with_a_bad_colour_is_rejected_before_it_reaches_the_renderer() {
        let broken = Theme { error: "not a colour".into(), ..builtin("slate").unwrap() };
        assert!(Material::from_theme(&broken).is_err());
    }
}
