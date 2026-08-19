//! Font resolution and cell metrics.
//!
//! Two decisions here are worth more than the code that implements them.
//!
//! **The font stack is ordered and explicit.** `JetBrains Mono → JetBrainsMono
//! NL → SF Mono → Menlo`, with `Apple Color Emoji` as the colour fallback.
//! Menlo ships with every macOS, so the last entry always resolves and the
//! terminal always starts.
//!
//! **Cell metrics are measured, not read from the font tables.** A monospaced
//! font's advertised advance and its actual advance disagree often enough —
//! ligature variants, `NL` variants, fonts that are only nearly monospaced —
//! that trusting the table produces a grid which drifts by a fraction of a
//! pixel per column and is visibly wrong by column eighty. Rasterising a probe
//! string and taking the maximum advance costs one call at startup and is
//! always right.

use objc2_core_foundation::{CFRetained, CFString, Type};
use objc2_core_graphics::CGGlyph;
use objc2_core_text::{CTFont, CTFontOrientation, CTFontSymbolicTraits};

use objc2_core_foundation::CGSize;

/// The characters used to measure the cell.
///
/// Chosen to include the widest ASCII forms — `@`, `W`, `M` — plus the
/// punctuation that some "monospaced" fonts quietly narrow.
const PROBE: &str = "#%&@$*/\\|<>[]{}?!=+~^WM0123456789ABCDEFabcdefgxyz";

/// Which face a run of text wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
}

impl Style {
    pub const REGULAR: Style = Style { bold: false, italic: false };

    pub const fn new(bold: bool, italic: bool) -> Style {
        Style { bold, italic }
    }

    fn traits(self) -> CTFontSymbolicTraits {
        let mut t = CTFontSymbolicTraits(0);
        if self.bold {
            t |= CTFontSymbolicTraits::TraitBold;
        }
        if self.italic {
            t |= CTFontSymbolicTraits::TraitItalic;
        }
        t
    }
}

/// The measured cell, in device pixels at the given scale factor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellMetrics {
    pub width: u16,
    pub height: u16,
    /// Distance from the top of the cell to the baseline.
    pub baseline: u16,
    pub underline_position: i16,
    pub underline_thickness: u16,
    pub strikethrough_position: i16,
}

/// The default stack, in order.
pub const DEFAULT_STACK: &[&str] =
    &["JetBrains Mono", "JetBrainsMono NL", "SF Mono", "Menlo"];

/// The colour fallback. Deliberately separate: it is never a text font, it is
/// only ever reached for glyphs nothing else has.
pub const EMOJI_FALLBACK: &str = "Apple Color Emoji";

/// One resolved family across the four faces, plus the metrics it implies.
pub struct FontSet {
    regular: CFRetained<CTFont>,
    bold: CFRetained<CTFont>,
    italic: CFRetained<CTFont>,
    bold_italic: CFRetained<CTFont>,
    emoji: Option<CFRetained<CTFont>>,
    /// Fonts after the resolved primary, consulted when it lacks a glyph.
    fallbacks: Vec<CFRetained<CTFont>>,
    family: String,
    size: f32,
    scale: f32,
    metrics: CellMetrics,
}

impl std::fmt::Debug for FontSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontSet")
            .field("family", &self.family)
            .field("size", &self.size)
            .field("scale", &self.scale)
            .field("metrics", &self.metrics)
            .field("fallbacks", &self.fallbacks.len())
            .finish()
    }
}

fn create_font(name: &str, size: f64) -> Option<CFRetained<CTFont>> {
    let cf_name = CFString::from_str(name);
    // SAFETY: `CTFontCreateWithName` takes a name and a size; a null matrix
    // means the identity, which is what we want.
    let font = unsafe { CTFont::with_name(&cf_name, size, std::ptr::null()) };
    // CoreText substitutes a default face rather than failing when a family is
    // missing, so ask the font what it actually is. Without this check the
    // stack silently collapses to Helvetica and every column is wrong.
    let resolved = unsafe { font.family_name() }.to_string();
    let requested_matches = resolved.eq_ignore_ascii_case(name)
        || resolved.replace(' ', "").eq_ignore_ascii_case(&name.replace(' ', ""));
    requested_matches.then_some(font)
}

fn with_traits(base: &CTFont, size: f64, style: Style) -> CFRetained<CTFont> {
    let traits = style.traits();
    if traits.0 == 0 {
        return base.retain();
    }
    let mask = CTFontSymbolicTraits::TraitBold | CTFontSymbolicTraits::TraitItalic;
    // SAFETY: null matrix means identity. A family with no bold face returns
    // None, in which case the regular face is the honest answer — synthesising
    // a fake bold by smearing looks worse than not being bold.
    unsafe { base.copy_with_symbolic_traits(size, std::ptr::null(), traits, mask) }
        .unwrap_or_else(|| base.retain())
}

impl FontSet {
    /// Resolves `preferred`, then the default stack, then Menlo.
    ///
    /// `scale` is the backing-store scale factor (2.0 on Retina). Metrics are
    /// returned in device pixels because that is the only unit the atlas and
    /// the renderer both agree on.
    pub fn resolve(preferred: &str, size: f32, scale: f32) -> FontSet {
        let scale = if scale > 0.0 { scale } else { 1.0 };
        let pixel_size = (size * scale) as f64;

        let mut candidates: Vec<&str> = Vec::with_capacity(DEFAULT_STACK.len() + 1);
        if !preferred.is_empty() {
            candidates.push(preferred);
        }
        candidates.extend(DEFAULT_STACK.iter().copied());

        let mut resolved: Vec<(String, CFRetained<CTFont>)> = Vec::new();
        for name in candidates {
            if resolved.iter().any(|(n, _)| n == name) {
                continue;
            }
            if let Some(font) = create_font(name, pixel_size) {
                resolved.push((name.to_owned(), font));
            }
        }

        // Menlo is present on every macOS, so this is a belt-and-braces path
        // rather than an expected one — but a terminal that refuses to open
        // because a font is missing is a terminal nobody can fix the font with.
        let (family, regular) = match resolved.first() {
            Some((name, font)) => (name.clone(), font.retain()),
            None => {
                let cf = CFString::from_str("Menlo");
                ("Menlo".to_owned(), unsafe {
                    CTFont::with_name(&cf, pixel_size, std::ptr::null())
                })
            }
        };

        let fallbacks: Vec<CFRetained<CTFont>> =
            resolved.iter().skip(1).map(|(_, f)| f.retain()).collect();

        let bold = with_traits(&regular, pixel_size, Style::new(true, false));
        let italic = with_traits(&regular, pixel_size, Style::new(false, true));
        let bold_italic = with_traits(&regular, pixel_size, Style::new(true, true));
        let emoji = create_font(EMOJI_FALLBACK, pixel_size);

        let metrics = measure(&regular, &bold);

        FontSet {
            regular,
            bold,
            italic,
            bold_italic,
            emoji,
            fallbacks,
            family,
            size,
            scale,
            metrics,
        }
    }

    pub fn family(&self) -> &str {
        &self.family
    }

    pub fn size(&self) -> f32 {
        self.size
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    pub fn face(&self, style: Style) -> &CTFont {
        match (style.bold, style.italic) {
            (false, false) => &self.regular,
            (true, false) => &self.bold,
            (false, true) => &self.italic,
            (true, true) => &self.bold_italic,
        }
    }

    pub fn emoji(&self) -> Option<&CTFont> {
        self.emoji.as_deref()
    }

    /// Finds the first face in the stack that actually has this character.
    ///
    /// Returns `None` when nothing in the text stack has it — the caller then
    /// takes the colour path, which is how emoji end up rendered as emoji
    /// rather than as a fallback box.
    pub fn glyph_for(&self, ch: char, style: Style) -> Option<(&CTFont, CGGlyph)> {
        let primary = self.face(style);
        if let Some(glyph) = glyph_id(primary, ch) {
            return Some((primary, glyph));
        }
        for font in &self.fallbacks {
            if let Some(glyph) = glyph_id(font, ch) {
                return Some((font, glyph));
            }
        }
        None
    }
}

/// The glyph id for a character, or `None` when the font does not have it.
///
/// CoreText reports a missing glyph as id 0 (`.notdef`) *and* returns false,
/// but only for the whole batch — with one character in the batch the two
/// agree, which is why this asks one at a time.
fn glyph_id(font: &CTFont, ch: char) -> Option<CGGlyph> {
    let mut utf16 = [0u16; 2];
    let encoded = ch.encode_utf16(&mut utf16);
    let mut glyphs = [0u16; 2];
    // SAFETY: both buffers hold `encoded.len()` elements and outlive the call.
    let ok = unsafe {
        font.glyphs_for_characters(
            std::ptr::NonNull::new(encoded.as_mut_ptr()).unwrap(),
            std::ptr::NonNull::new(glyphs.as_mut_ptr()).unwrap(),
            encoded.len() as isize,
        )
    };
    (ok && glyphs[0] != 0).then_some(glyphs[0])
}

/// Measures the cell by rasterising the probe string, not by trusting tables.
fn measure(regular: &CTFont, bold: &CTFont) -> CellMetrics {
    let mut advance: f64 = 0.0;
    for font in [regular, bold] {
        for ch in PROBE.chars() {
            let Some(glyph) = glyph_id(font, ch) else { continue };
            let mut glyphs = [glyph];
            let mut size = CGSize::new(0.0, 0.0);
            // SAFETY: one glyph in, one size out.
            unsafe {
                font.advances_for_glyphs(
                    CTFontOrientation::Default,
                    std::ptr::NonNull::new(glyphs.as_mut_ptr()).unwrap(),
                    &mut size,
                    1,
                );
            }
            advance = advance.max(size.width);
        }
    }

    // SAFETY: plain accessors on a live font.
    let (ascent, descent, leading) =
        unsafe { (regular.ascent(), regular.descent(), regular.leading()) };

    // Round the cell outward, then keep the baseline inside it. Rounding the
    // parts independently is what produces the classic one-pixel clipped
    // descender.
    let width = advance.ceil().max(1.0) as u16;
    let height = (ascent + descent + leading).ceil().max(1.0) as u16;
    let baseline = (ascent + leading).round().clamp(1.0, height as f64) as u16;

    let underline_thickness = (height as f64 / 14.0).round().max(1.0) as u16;
    let underline_position = (baseline as i32 + underline_thickness as i32).min(height as i32 - 1);

    CellMetrics {
        width,
        height,
        baseline,
        underline_position: underline_position as i16,
        underline_thickness,
        strikethrough_position: (baseline as f64 * 0.7).round() as i16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menlo_always_resolves_so_the_terminal_always_starts() {
        // Menlo ships with macOS. If this fails, the resolution logic is
        // rejecting a font that is genuinely present.
        assert!(create_font("Menlo", 13.0).is_some());
    }

    #[test]
    fn a_missing_family_does_not_silently_become_helvetica() {
        // CoreText substitutes rather than failing, which would let a typo in
        // the config quietly break the grid.
        assert!(create_font("Definitely Not An Installed Font", 13.0).is_none());
    }

    #[test]
    fn an_unknown_preference_falls_through_to_the_stack() {
        let set = FontSet::resolve("No Such Font At All", 13.0, 1.0);
        assert!(
            DEFAULT_STACK.contains(&set.family()),
            "fell back to {:?}, which is not in the stack",
            set.family()
        );
    }

    #[test]
    fn metrics_are_positive_and_self_consistent() {
        let m = FontSet::resolve("Menlo", 13.0, 1.0).metrics();
        assert!(m.width > 0 && m.height > 0);
        assert!(m.baseline > 0 && m.baseline <= m.height, "baseline {m:?} escapes the cell");
        assert!(m.underline_thickness >= 1);
        assert!((m.underline_position as u16) < m.height, "underline sits outside the cell");
    }

    #[test]
    fn a_monospaced_font_gives_a_cell_narrower_than_it_is_tall() {
        // Not a law of nature, but true of every font in the stack — and if it
        // ever fails, the measurement is picking up a proportional face.
        let m = FontSet::resolve("Menlo", 13.0, 1.0).metrics();
        assert!(m.width < m.height, "cell {m:?} is wider than tall");
    }

    #[test]
    fn the_retina_scale_factor_doubles_the_cell() {
        let one = FontSet::resolve("Menlo", 13.0, 1.0).metrics();
        let two = FontSet::resolve("Menlo", 13.0, 2.0).metrics();
        assert!(two.width >= one.width * 2 - 1 && two.width <= one.width * 2 + 1);
        assert!(two.height >= one.height * 2 - 2);
    }

    #[test]
    fn ascii_resolves_in_the_text_stack() {
        let set = FontSet::resolve("Menlo", 13.0, 1.0);
        for ch in "AZaz09 #@".chars() {
            assert!(set.glyph_for(ch, Style::REGULAR).is_some(), "{ch:?} has no glyph");
        }
    }

    #[test]
    fn emoji_do_not_resolve_in_the_text_stack() {
        // This is the mechanism that routes emoji to the colour path instead
        // of drawing a fallback box.
        let set = FontSet::resolve("Menlo", 13.0, 1.0);
        assert!(set.glyph_for('\u{1F680}', Style::REGULAR).is_none());
        assert!(set.emoji().is_some(), "Apple Color Emoji should be present on macOS");
    }

    #[test]
    fn every_face_is_available_even_when_a_family_lacks_one() {
        let set = FontSet::resolve("Menlo", 13.0, 1.0);
        for style in [
            Style::REGULAR,
            Style::new(true, false),
            Style::new(false, true),
            Style::new(true, true),
        ] {
            assert!(set.glyph_for('A', style).is_some(), "{style:?} cannot draw an A");
        }
    }
}
