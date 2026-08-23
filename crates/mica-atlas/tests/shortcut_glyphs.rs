//! The symbols a Mac menu uses for keyboard shortcuts.
//!
//! `⌘⌥⇧⌃⌫⌦⎋⇥↩` are what [`Chord::to_display`] emits, and what the settings
//! catalogue print. None of them are in a monospace programming face, so every
//! one reaches CoreText's fallback — which is the same path that produced black,
//! untintable glyphs for CJK before `kCTForegroundColorFromContextAttributeName`
//! was set.
//!
//! Two properties, and both matter:
//!
//! 1. They rasterise to something. A shortcut printed as a blank is a shortcut
//!    the user cannot read.
//! 2. They come back as **masks**, not colour bitmaps. A colour glyph ignores
//!    the cell's foreground, so a ⌘ that arrived as BGRA would be stuck at
//!    whatever colour the font drew it and would vanish on half the themes.
//!
//! This is the whole of "ship a keyboard-shortcut font family": macOS already
//! has one, and the job is making sure Mica's fallback reaches it and tints
//! what it gets back.

use mica_atlas::fontset::{FontSet, Style};
use mica_atlas::raster::{rasterize_scalar, PixelFormat};

/// Every symbol `Chord::to_display` can emit.
const SYMBOLS: &[(char, &str)] = &[
    ('\u{2318}', "command"),
    ('\u{2325}', "option"),
    ('\u{21E7}', "shift"),
    ('\u{2303}', "control"),
    ('\u{232B}', "backspace"),
    ('\u{2326}', "forward delete"),
    ('\u{238B}', "escape"),
    ('\u{21E5}', "tab"),
    ('\u{21A9}', "return"),
    ('\u{2191}', "up"),
    ('\u{2193}', "down"),
    ('\u{2190}', "left"),
    ('\u{2192}', "right"),
    ('\u{2196}', "home"),
    ('\u{2198}', "end"),
    ('\u{21DE}', "page up"),
    ('\u{21DF}', "page down"),
];

fn fonts() -> FontSet {
    FontSet::resolve("JetBrains Mono", 13.0, 2.0)
}

#[test]
fn every_shortcut_symbol_rasterises_to_something_visible() {
    let fonts = fonts();
    for (ch, name) in SYMBOLS {
        let bitmap = rasterize_scalar(&fonts, *ch, Style::default(), 1)
            .unwrap_or_else(|| panic!("the {name} symbol {ch:?} rasterised to nothing"));
        assert!(
            !bitmap.is_empty(),
            "the {name} symbol {ch:?} rasterised to an empty bitmap"
        );
        assert!(
            bitmap.data.iter().any(|&b| b > 0),
            "the {name} symbol {ch:?} rasterised to a bitmap with no coverage in it"
        );
    }
}

#[test]
fn every_shortcut_symbol_comes_back_tintable() {
    // A colour glyph ignores the cell's foreground. A ⌘ that arrived as BGRA
    // would be locked to whatever colour the fallback font drew it in and
    // would disappear against half the themes.
    let fonts = fonts();
    for (ch, name) in SYMBOLS {
        let bitmap = rasterize_scalar(&fonts, *ch, Style::default(), 1).unwrap();
        assert_eq!(
            bitmap.format,
            PixelFormat::Alpha8,
            "the {name} symbol {ch:?} came back as colour, so it cannot be tinted"
        );
    }
}

#[test]
fn a_shortcut_symbol_stays_inside_its_cell() {
    // These are read as a column of chords. One that
    // overhangs pushes into the label beside it.
    let fonts = fonts();
    let cell_width = fonts.metrics().width as i32;
    for (ch, name) in SYMBOLS {
        let bitmap = rasterize_scalar(&fonts, *ch, Style::default(), 1).unwrap();
        let right = bitmap.left as i32 + bitmap.width as i32;
        assert!(
            right <= cell_width + 1,
            "the {name} symbol {ch:?} is {right}px wide in a {cell_width}px cell"
        );
    }
}
