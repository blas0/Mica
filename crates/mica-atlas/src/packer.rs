//! Shelf packing for the glyph atlas.
//!
//! A terminal atlas is an unusually easy packing problem and it is worth saying
//! why, because it justifies not reaching for a general rectangle packer:
//! almost every glyph is the same height (one cell), they arrive in
//! unpredictable order but with very low variety, and the total count is
//! bounded by the character repertoire actually used in one session — a few
//! hundred, not a few thousand.
//!
//! Shelf packing wastes some vertical space per row and is O(1) per insert.
//! MaxRects would pack ~5% tighter and cost a lot more code. For a texture that
//! is uploaded once and read for the rest of the process lifetime, that is not
//! a trade worth making.
//!
//! This module is deliberately pure: no CoreText, no Metal, no allocation
//! beyond a `Vec` of shelves. It is the one part of the atlas that can be
//! tested exhaustively without a window server.

/// Where a glyph ended up in a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub const fn area(&self) -> u32 {
        self.width as u32 * self.height as u32
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.x + other.width
            && other.x < self.x + self.width
            && self.y < other.y + other.height
            && other.y < self.y + self.height
    }
}

#[derive(Debug, Clone, Copy)]
struct Shelf {
    y: u16,
    height: u16,
    /// Next free x on this shelf.
    cursor: u16,
}

/// One texture page's worth of free space.
#[derive(Debug)]
pub struct ShelfPacker {
    width: u16,
    height: u16,
    shelves: Vec<Shelf>,
    /// Top of the unshelved region.
    next_y: u16,
    used_area: u32,
    /// 1 px of padding between glyphs, so bilinear sampling at a glyph edge
    /// cannot pick up its neighbour. Without it, fast scrolling shows faint
    /// vertical seams that are extremely hard to diagnose after the fact.
    padding: u16,
}

impl ShelfPacker {
    pub fn new(width: u16, height: u16) -> ShelfPacker {
        ShelfPacker {
            width,
            height,
            shelves: Vec::new(),
            next_y: 0,
            used_area: 0,
            padding: 1,
        }
    }

    pub fn dimensions(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    /// Fraction of the page consumed, padding included.
    pub fn occupancy(&self) -> f32 {
        self.used_area as f32 / (self.width as f32 * self.height as f32)
    }

    /// Finds room for a `width × height` glyph, or `None` when the page is
    /// full. A full page is not an error — the atlas opens another one.
    pub fn allocate(&mut self, width: u16, height: u16) -> Option<Rect> {
        // A zero-sized glyph (a space, a control character) has no pixels and
        // must not consume atlas space; the caller records an empty rect.
        if width == 0 || height == 0 {
            return Some(Rect { x: 0, y: 0, width: 0, height: 0 });
        }
        let padded_w = width.checked_add(self.padding)?;
        let padded_h = height.checked_add(self.padding)?;
        if padded_w > self.width || padded_h > self.height {
            return None;
        }

        // Best fit among existing shelves: the shelf whose height wastes the
        // least. First-fit is simpler but degrades badly once one tall glyph
        // — a box-drawing character, or an emoji — creates a tall shelf that
        // then swallows every subsequent short glyph.
        let mut best: Option<(usize, u16)> = None;
        for (i, shelf) in self.shelves.iter().enumerate() {
            if shelf.height < padded_h {
                continue;
            }
            if shelf.cursor.saturating_add(padded_w) > self.width {
                continue;
            }
            let waste = shelf.height - padded_h;
            if best.is_none_or(|(_, best_waste)| waste < best_waste) {
                best = Some((i, waste));
            }
        }

        // Reuse a shelf only when the height it wastes is reasonable. A 40 px
        // shelf opened by one tall glyph would otherwise swallow every 9 px
        // glyph for the rest of the session, wasting 31 px of column each
        // time; opening a fresh shelf is cheaper as long as one still fits.
        let wasteful = best.is_some_and(|(_, waste)| waste > padded_h);
        let room_for_a_new_shelf =
            self.next_y.checked_add(padded_h).is_some_and(|end| end <= self.height);

        if let Some((i, _)) = best.filter(|_| !(wasteful && room_for_a_new_shelf)) {
            let shelf = &mut self.shelves[i];
            let rect = Rect { x: shelf.cursor, y: shelf.y, width, height };
            shelf.cursor += padded_w;
            self.used_area += padded_w as u32 * shelf.height as u32;
            return Some(rect);
        }

        // Open a new shelf.
        if self.next_y.checked_add(padded_h)? > self.height {
            return None;
        }
        let shelf = Shelf { y: self.next_y, height: padded_h, cursor: padded_w };
        let rect = Rect { x: 0, y: shelf.y, width, height };
        self.next_y += padded_h;
        self.used_area += padded_w as u32 * padded_h as u32;
        self.shelves.push(shelf);
        Some(rect)
    }

    /// Drops every allocation. Used when the page is evicted wholesale —
    /// a font or size change invalidates every glyph in it at once, and
    /// repacking is cheaper and simpler than reclaiming rectangles piecemeal.
    pub fn reset(&mut self) {
        self.shelves.clear();
        self.next_y = 0;
        self.used_area = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_page_is_empty() {
        let p = ShelfPacker::new(256, 256);
        assert_eq!(p.occupancy(), 0.0);
        assert_eq!(p.dimensions(), (256, 256));
    }

    #[test]
    fn allocations_do_not_overlap() {
        let mut p = ShelfPacker::new(128, 128);
        let mut placed: Vec<Rect> = Vec::new();
        // A spread of sizes, including a couple of tall ones, in an order that
        // forces both the shelf-reuse and the new-shelf path.
        for (w, h) in [(8, 16), (8, 16), (10, 20), (8, 16), (30, 8), (8, 16), (12, 24)] {
            let r = p.allocate(w, h).expect("128x128 has room for these");
            for existing in &placed {
                assert!(!r.intersects(existing), "{r:?} overlaps {existing:?}");
            }
            placed.push(r);
        }
        assert_eq!(placed.len(), 7);
    }

    #[test]
    fn every_allocation_stays_inside_the_page() {
        let mut p = ShelfPacker::new(64, 64);
        while let Some(r) = p.allocate(9, 17) {
            assert!(r.x + r.width <= 64, "{r:?} runs off the right edge");
            assert!(r.y + r.height <= 64, "{r:?} runs off the bottom edge");
        }
    }

    #[test]
    fn a_glyph_larger_than_the_page_is_refused_rather_than_clipped() {
        let mut p = ShelfPacker::new(32, 32);
        assert_eq!(p.allocate(64, 8), None);
        assert_eq!(p.allocate(8, 64), None);
        // …and refusing it must not corrupt the page for the next glyph.
        assert!(p.allocate(8, 8).is_some());
    }

    #[test]
    fn a_zero_sized_glyph_consumes_no_space() {
        let mut p = ShelfPacker::new(64, 64);
        let r = p.allocate(0, 0).unwrap();
        assert_eq!(r.area(), 0);
        assert_eq!(p.occupancy(), 0.0, "a space must not cost atlas area");
    }

    #[test]
    fn a_full_page_reports_full_instead_of_wrapping() {
        let mut p = ShelfPacker::new(16, 16);
        let mut count = 0;
        while p.allocate(7, 7).is_some() {
            count += 1;
            assert!(count < 100, "the packer never reported the page full");
        }
        assert!(count > 0);
    }

    #[test]
    fn uniform_glyphs_pack_densely() {
        // The realistic case: one cell size, hundreds of glyphs. Padding costs
        // roughly 1px in each direction, so ~70% is the floor worth holding to.
        let mut p = ShelfPacker::new(512, 512);
        let mut n = 0;
        while p.allocate(9, 18).is_some() {
            n += 1;
        }
        assert!(n >= 1300, "only packed {n} cells into 512x512");
        assert!(p.occupancy() > 0.7, "occupancy was {}", p.occupancy());
    }

    #[test]
    fn best_fit_keeps_a_tall_shelf_from_swallowing_short_glyphs() {
        let mut p = ShelfPacker::new(256, 64);
        // One tall glyph opens a 40px shelf, then a short one opens its own.
        p.allocate(10, 39).unwrap();
        let short = p.allocate(10, 8).unwrap();
        assert_ne!(short.y, 0, "the short glyph should not sit on the tall shelf");
    }

    #[test]
    fn reset_makes_the_page_reusable() {
        let mut p = ShelfPacker::new(64, 64);
        while p.allocate(10, 20).is_some() {}
        p.reset();
        assert_eq!(p.occupancy(), 0.0);
        assert_eq!(p.allocate(10, 20), Some(Rect { x: 0, y: 0, width: 10, height: 20 }));
    }
}
