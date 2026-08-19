//! The grid cell.
//!
//! A cell is **20 bytes and stays 20 bytes**. Grapheme clusters, colour emoji,
//! and per-cell underline colour do not live here — they live in lazily
//! allocated side tables that a plain build log never allocates. `Cell::SIZE`
//! is asserted at compile time; see `assert_cell_is_twenty_bytes` below.

/// A resolved or deferred colour.
///
/// Packed into a `u32` as `[tag:8][payload:24]` so that a cell can carry two of
/// them without paying for an enum discriminant.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Color(u32);

const TAG_DEFAULT: u32 = 0 << 24;
const TAG_PALETTE: u32 = 1 << 24;
const TAG_RGB: u32 = 2 << 24;
const TAG_MASK: u32 = 0xFF00_0000;
const PAYLOAD_MASK: u32 = 0x00FF_FFFF;

impl Color {
    /// "Whatever the theme says" — resolved at render time, not at parse time.
    /// This is what makes a live theme cross-fade possible without rewriting
    /// the grid.
    pub const DEFAULT: Color = Color(TAG_DEFAULT);

    pub const fn palette(index: u8) -> Color {
        Color(TAG_PALETTE | index as u32)
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color(TAG_RGB | ((r as u32) << 16) | ((g as u32) << 8) | b as u32)
    }

    pub const fn is_default(self) -> bool {
        self.0 & TAG_MASK == TAG_DEFAULT
    }

    pub const fn as_palette(self) -> Option<u8> {
        if self.0 & TAG_MASK == TAG_PALETTE {
            Some((self.0 & 0xFF) as u8)
        } else {
            None
        }
    }

    pub const fn as_rgb(self) -> Option<(u8, u8, u8)> {
        if self.0 & TAG_MASK == TAG_RGB {
            let p = self.0 & PAYLOAD_MASK;
            Some(((p >> 16) as u8, (p >> 8) as u8, p as u8))
        } else {
            None
        }
    }

    pub const fn to_bits(self) -> u32 {
        self.0
    }
}

impl core::fmt::Debug for Color {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match (self.is_default(), self.as_palette(), self.as_rgb()) {
            (true, _, _) => f.write_str("Color::DEFAULT"),
            (_, Some(i), _) => write!(f, "Color::palette({i})"),
            (_, _, Some((r, g, b))) => write!(f, "Color::rgb({r}, {g}, {b})"),
            _ => f.write_str("Color(?)"),
        }
    }
}

/// What occupies a cell.
///
/// A scalar `char` fits directly (Unicode tops out at `U+10FFFF`, so bit 31 is
/// free as a tag). Anything wider — a family emoji, a flag, a skin-tone
/// sequence — is an index into [`crate::sidetable::Graphemes`], and one cell
/// still means one cell.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct CellContent(u32);

const CONTENT_CLUSTER_TAG: u32 = 1 << 31;

impl CellContent {
    pub const EMPTY: CellContent = CellContent(' ' as u32);

    pub const fn scalar(c: char) -> CellContent {
        CellContent(c as u32)
    }

    /// A reference into the grapheme side table.
    pub const fn cluster(index: u32) -> CellContent {
        debug_assert!(index < CONTENT_CLUSTER_TAG);
        CellContent(CONTENT_CLUSTER_TAG | index)
    }

    pub const fn as_scalar(self) -> Option<char> {
        if self.0 & CONTENT_CLUSTER_TAG == 0 {
            char::from_u32(self.0)
        } else {
            None
        }
    }

    pub const fn as_cluster(self) -> Option<u32> {
        if self.0 & CONTENT_CLUSTER_TAG != 0 {
            Some(self.0 & !CONTENT_CLUSTER_TAG)
        } else {
            None
        }
    }

    pub const fn is_empty(self) -> bool {
        self.0 == ' ' as u32 || self.0 == 0
    }
}

impl core::fmt::Debug for CellContent {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match (self.as_scalar(), self.as_cluster()) {
            (Some(c), _) => write!(f, "{c:?}"),
            (_, Some(i)) => write!(f, "cluster#{i}"),
            _ => f.write_str("?"),
        }
    }
}

bitflags_lite! {
    /// Per-cell rendering attributes. Underline *style* is here; underline
    /// *colour* is a side table, because almost nothing sets it.
    pub struct CellFlags: u16 {
        const BOLD          = 1 << 0;
        const DIM           = 1 << 1;
        const ITALIC        = 1 << 2;
        const INVERSE       = 1 << 3;
        const HIDDEN        = 1 << 4;
        const STRIKETHROUGH = 1 << 5;
        const OVERLINE      = 1 << 6;
        // Underline style occupies bits 8..11 (DECSCUSR-adjacent SGR 4:x).
        const UNDERLINE_SINGLE = 1 << 8;
        const UNDERLINE_DOUBLE = 2 << 8;
        const UNDERLINE_CURLY  = 3 << 8;
        const UNDERLINE_DOTTED = 4 << 8;
        const UNDERLINE_DASHED = 5 << 8;
        const UNDERLINE_MASK   = 0xF << 8;
        /// The trailing half of a double-width cell. Carries no glyph of its
        /// own; the renderer must skip it.
        const WIDE_SPACER   = 1 << 12;
    }
}

/// Index into the per-row extras side table. Zero means "no extras", which is
/// the case for essentially every cell in a build log.
pub type ExtraId = u32;
pub const NO_EXTRA: ExtraId = 0;

/// One grid cell. **Exactly 20 bytes.**
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct Cell {
    pub content: CellContent, // 4
    pub fg: Color,            // 4
    pub bg: Color,            // 4
    pub extra: ExtraId,       // 4  — underline colour, hyperlink; 0 = none
    pub flags: CellFlags,     // 2
    pub width: u8,            // 1  — 1 or 2 columns
    _pad: u8,                 // 1
}

impl Cell {
    pub const SIZE: usize = 20;

    pub const EMPTY: Cell = Cell {
        content: CellContent::EMPTY,
        fg: Color::DEFAULT,
        bg: Color::DEFAULT,
        extra: NO_EXTRA,
        flags: CellFlags::EMPTY,
        width: 1,
        _pad: 0,
    };

    pub fn new(content: CellContent, fg: Color, bg: Color, flags: CellFlags) -> Cell {
        Cell { content, fg, bg, extra: NO_EXTRA, flags, width: 1, _pad: 0 }
    }

    /// The full constructor. `_pad` is private on purpose — it exists only to
    /// round the struct to 20 bytes, and leaving it uninitialised through a
    /// struct-update expression would make two otherwise-equal cells compare
    /// unequal.
    pub fn build(
        content: CellContent,
        fg: Color,
        bg: Color,
        extra: ExtraId,
        flags: CellFlags,
        width: u8,
    ) -> Cell {
        Cell { content, fg, bg, extra, flags, width, _pad: 0 }
    }

    /// True when the cell would render as nothing at all — the fast path the
    /// renderer uses to drop a quad before it ever reaches the GPU.
    pub fn is_blank(&self) -> bool {
        self.content.is_empty()
            && self.bg.is_default()
            && self.extra == NO_EXTRA
            && self.flags.intersection(CellFlags::UNDERLINE_MASK
                .union(CellFlags::STRIKETHROUGH)
                .union(CellFlags::OVERLINE)
                .union(CellFlags::INVERSE))
                .is_empty()
    }
}

impl Default for Cell {
    fn default() -> Cell {
        Cell::EMPTY
    }
}

/// The claim from the product surface, enforced by the compiler rather than by
/// a comment. If this ever fails, the fix is a side table, not a bigger cell.
const _: () = {
    assert!(core::mem::size_of::<Cell>() == Cell::SIZE);
    assert!(core::mem::align_of::<Cell>() == 4);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_is_twenty_bytes() {
        assert_eq!(core::mem::size_of::<Cell>(), 20);
    }

    #[test]
    fn color_roundtrips_through_its_packing() {
        assert!(Color::DEFAULT.is_default());
        assert_eq!(Color::palette(9).as_palette(), Some(9));
        assert_eq!(Color::rgb(1, 2, 3).as_rgb(), Some((1, 2, 3)));
        assert_eq!(Color::rgb(1, 2, 3).as_palette(), None);
        assert!(!Color::palette(0).is_default());
    }

    #[test]
    fn max_scalar_char_does_not_collide_with_the_cluster_tag() {
        let c = CellContent::scalar('\u{10FFFF}');
        assert_eq!(c.as_scalar(), Some('\u{10FFFF}'));
        assert_eq!(c.as_cluster(), None);
    }

    #[test]
    fn cluster_index_survives_tagging() {
        let c = CellContent::cluster(0x7FFF_FFFE);
        assert_eq!(c.as_cluster(), Some(0x7FFF_FFFE));
        assert_eq!(c.as_scalar(), None);
    }

    #[test]
    fn empty_cell_is_blank_but_a_coloured_background_is_not() {
        assert!(Cell::EMPTY.is_blank());
        let mut c = Cell::EMPTY;
        c.bg = Color::palette(1);
        assert!(!c.is_blank());
    }
}
