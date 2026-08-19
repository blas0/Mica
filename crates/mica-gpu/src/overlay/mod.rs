//! Overlays — the palette and the find bar.
//!
//! **These are drawn inside the Metal layer, not as AppKit views.** That is why
//! `ui_text` is a separate pipeline from `cell`: chrome text is positioned in
//! free pixels rather than snapped to the grid, but it comes from the same
//! resident atlas and costs the same instanced quad. An overlay opening is one
//! extra buffer and two extra draw calls — there is no view to instantiate, lay
//! out, and composite, which is what lets the palette appear within one frame.
//!
//! The trade is that AppKit gives us nothing: no text field, no focus ring, no
//! key-view loop. Overlays therefore own their own tiny editing model
//! ([`TextField`]) and their own key handling. For a single-line query field
//! that is perhaps eighty lines of code, and it buys the frame back.

pub mod find;
pub mod palette;

use mica_atlas::atlas::{Atlas, GlyphKey};
use mica_atlas::fontset::Style;
use mica_atlas::raster::PixelFormat;
use mica_core::material::{Material, Rgb, Role};

use crate::grid::{QuadInstance, Rgba, UiTextInstance, GLYPH_FLAG_COLOR};

/// A single-line editable string with a caret.
///
/// Deliberately minimal: no selection, no undo, no kill ring. A query field is
/// not a text editor, and every feature added here is one the terminal below it
/// already does better.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TextField {
    text: String,
    /// Caret position, in characters.
    caret: usize,
}

impl TextField {
    pub fn new() -> TextField {
        TextField::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.caret = 0;
    }

    pub fn set(&mut self, text: &str) {
        self.text = text.to_owned();
        self.caret = self.text.chars().count();
    }

    pub fn insert(&mut self, ch: char) {
        let byte = self.byte_offset(self.caret);
        self.text.insert(byte, ch);
        self.caret += 1;
    }

    pub fn insert_str(&mut self, text: &str) {
        for ch in text.chars() {
            self.insert(ch);
        }
    }

    /// Deletes the character before the caret. Returns whether anything moved,
    /// so the caller knows whether to schedule a frame.
    pub fn backspace(&mut self) -> bool {
        if self.caret == 0 {
            return false;
        }
        let byte = self.byte_offset(self.caret - 1);
        self.text.remove(byte);
        self.caret -= 1;
        true
    }

    pub fn delete_forward(&mut self) -> bool {
        let count = self.text.chars().count();
        if self.caret >= count {
            return false;
        }
        let byte = self.byte_offset(self.caret);
        self.text.remove(byte);
        true
    }

    pub fn move_left(&mut self) -> bool {
        if self.caret == 0 {
            return false;
        }
        self.caret -= 1;
        true
    }

    pub fn move_right(&mut self) -> bool {
        if self.caret >= self.text.chars().count() {
            return false;
        }
        self.caret += 1;
        true
    }

    pub fn move_home(&mut self) {
        self.caret = 0;
    }

    pub fn move_end(&mut self) {
        self.caret = self.text.chars().count();
    }

    /// Byte offset of a character index. Characters, not bytes, throughout —
    /// a query field that panics on a multi-byte character is a query field
    /// nobody can search a log with.
    fn byte_offset(&self, chars: usize) -> usize {
        self.text
            .char_indices()
            .nth(chars)
            .map(|(i, _)| i)
            .unwrap_or(self.text.len())
    }
}

/// Geometry shared by the overlays, in device pixels.
#[derive(Debug, Clone, Copy)]
pub struct OverlayMetrics {
    pub viewport: (f32, f32),
    pub cell_width: f32,
    pub cell_height: f32,
    pub baseline: f32,
}

impl OverlayMetrics {
    pub fn from_atlas(atlas: &Atlas, viewport: (f32, f32)) -> OverlayMetrics {
        let m = atlas.metrics();
        OverlayMetrics {
            viewport,
            cell_width: m.width as f32,
            cell_height: m.height as f32,
            baseline: m.baseline as f32,
        }
    }

    /// A comfortable row height for chrome — taller than a terminal row,
    /// because a list the eye scans wants more air than a grid of text.
    pub fn row_height(&self) -> f32 {
        (self.cell_height * 1.6).round()
    }

    pub fn padding(&self) -> f32 {
        (self.cell_width * 1.5).round()
    }
}

/// Lays out a string as `ui_text` instances and returns the advance width.
///
/// `origin` is the top-left of the text box. Advances are the cell width rather
/// than the glyph's own, so chrome stays monospaced and columns in the palette
/// line up — which is the whole reason a terminal's own font is used for its UI.
#[allow(clippy::too_many_arguments)]
pub fn layout_text(
    atlas: &mut Atlas,
    text: &str,
    origin: (f32, f32),
    color: Rgb,
    alpha: f32,
    metrics: OverlayMetrics,
    style: Style,
    out: &mut Vec<UiTextInstance>,
) -> f32 {
    let mut x = origin.0;
    for ch in text.chars() {
        if ch == ' ' {
            x += metrics.cell_width;
            continue;
        }
        let Some(entry) = atlas.glyph(GlyphKey::scalar(ch, style), || None) else {
            x += metrics.cell_width;
            continue;
        };
        if !entry.is_blank() {
            out.push(UiTextInstance {
                origin: [x + entry.left as f32, origin.1 + entry.top as f32],
                size: [entry.rect.width, entry.rect.height],
                uv_origin: [entry.rect.x, entry.rect.y],
                color: Rgba::with_alpha(color, alpha),
                page: entry.page,
                flags: match entry.format {
                    PixelFormat::Bgra8 => GLYPH_FLAG_COLOR,
                    PixelFormat::Alpha8 => 0,
                },
            });
        }
        x += metrics.cell_width;
    }
    x - origin.0
}

/// The width a string will occupy, without laying it out.
pub fn text_width(text: &str, metrics: OverlayMetrics) -> f32 {
    text.chars().count() as f32 * metrics.cell_width
}

/// A rounded panel: the background every overlay sits on.
pub fn panel(
    origin: (f32, f32),
    size: (f32, f32),
    material: &Material,
    alpha: f32,
) -> QuadInstance {
    let background = material.role(Role::Background);
    let dim = material.role(Role::Dim);
    QuadInstance {
        origin: [origin.0, origin.1],
        size: [size.0, size.1],
        // Slightly lifted off the background rather than a different colour:
        // an overlay should read as the same surface raised, which is what the
        // eight-role model makes easy.
        fill: Rgba::with_alpha(background.lerp(dim, 0.16), alpha * 0.98),
        border: Rgba::with_alpha(dim, alpha * 0.5),
        radius: 10.0,
        border_width: 1.0,
    }
}

/// A filled rectangle, for selection rows and search highlights.
pub fn fill(origin: (f32, f32), size: (f32, f32), color: Rgb, alpha: f32) -> QuadInstance {
    QuadInstance {
        origin: [origin.0, origin.1],
        size: [size.0, size.1],
        fill: Rgba::with_alpha(color, alpha),
        border: Rgba([0, 0, 0, 0]),
        radius: 3.0,
        border_width: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_field_is_empty_with_the_caret_at_the_start() {
        let field = TextField::new();
        assert!(field.is_empty());
        assert_eq!(field.caret(), 0);
    }

    #[test]
    fn typing_advances_the_caret() {
        let mut field = TextField::new();
        field.insert_str("error");
        assert_eq!(field.text(), "error");
        assert_eq!(field.caret(), 5);
    }

    #[test]
    fn backspace_at_the_start_reports_that_nothing_happened() {
        // The caller uses this to decide whether to schedule a frame; always
        // returning true would redraw on every rejected keystroke.
        let mut field = TextField::new();
        assert!(!field.backspace());
        field.insert('a');
        assert!(field.backspace());
        assert!(field.is_empty());
    }

    #[test]
    fn editing_happens_at_the_caret_not_at_the_end() {
        let mut field = TextField::new();
        field.insert_str("eror");
        field.move_home();
        field.move_right();
        field.insert('r');
        assert_eq!(field.text(), "error");
    }

    #[test]
    fn delete_forward_removes_the_character_under_the_caret() {
        let mut field = TextField::new();
        field.set("errror");
        field.move_home();
        field.move_right();
        assert!(field.delete_forward());
        assert_eq!(field.text(), "error");
    }

    #[test]
    fn the_caret_cannot_leave_the_string() {
        let mut field = TextField::new();
        field.set("ab");
        assert!(!field.move_right(), "the caret walked past the end");
        field.move_home();
        assert!(!field.move_left(), "the caret walked before the start");
    }

    #[test]
    fn multi_byte_characters_are_handled_by_character_not_by_byte() {
        // A query field that panics on an accent is a query field nobody can
        // search a log with.
        let mut field = TextField::new();
        field.insert_str("héllo→");
        assert_eq!(field.caret(), 6);
        assert!(field.backspace());
        assert_eq!(field.text(), "héllo");
        field.move_home();
        field.move_right();
        field.insert('X');
        assert_eq!(field.text(), "hXéllo");
    }

    #[test]
    fn deleting_a_multi_byte_character_removes_all_of_it() {
        let mut field = TextField::new();
        field.set("a→b");
        field.move_home();
        field.move_right();
        assert!(field.delete_forward());
        assert_eq!(field.text(), "ab");
    }

    #[test]
    fn text_width_counts_characters_not_bytes() {
        let metrics = OverlayMetrics {
            viewport: (800.0, 600.0),
            cell_width: 10.0,
            cell_height: 20.0,
            baseline: 15.0,
        };
        assert_eq!(text_width("abc", metrics), 30.0);
        assert_eq!(text_width("→→→", metrics), 30.0, "three arrows are three cells");
    }
}
