//! The resident glyph atlas.
//!
//! The product claim this implements: *"the glyph atlas stays resident on the
//! GPU, so a 200 MB log costs what an empty prompt costs."* A terminal's glyph
//! repertoire is bounded by what the user's text actually contains — a few
//! hundred distinct glyphs in practice — so after the first screenful, streaming
//! a gigabyte through the terminal rasterises nothing at all.
//!
//! This crate does not own a texture. It owns *pages*: a packer, a format, and
//! a queue of [`Upload`] deltas that `mica-gpu` blits. That is what keeps
//! Metal out of a crate whose logic is worth testing headlessly.
//!
//! ## Eviction
//!
//! Whole pages are evicted, never individual rectangles. A shelf packer cannot
//! reclaim a freed rectangle without a compaction pass, so per-glyph eviction
//! would fragment the page while reporting free space that cannot be used. In
//! practice eviction never fires: it takes several hundred distinct glyphs to
//! fill even one page, and the pathological case — a document cycling through
//! thousands of CJK glyphs — is exactly the case where dropping the whole page
//! and re-rasterising the current screen is right.

use std::collections::HashMap;

use crate::fontset::{CellMetrics, FontSet, Style};
use crate::packer::{Rect, ShelfPacker};
use crate::raster::{self, Bitmap, PixelFormat};

/// What a cell wants drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GlyphId {
    Scalar(char),
    /// An interned grapheme cluster. The id comes from the terminal core's
    /// side table; the atlas asks for the text only on a cache miss, so the
    /// hot path never touches a string.
    Cluster(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub id: GlyphId,
    pub style: Style,
    /// 1 or 2 — a double-width glyph is a different bitmap, not a stretched one.
    pub columns: u8,
}

impl GlyphKey {
    pub fn scalar(ch: char, style: Style) -> GlyphKey {
        GlyphKey { id: GlyphId::Scalar(ch), style, columns: 1 }
    }

    pub fn wide(ch: char, style: Style) -> GlyphKey {
        GlyphKey { id: GlyphId::Scalar(ch), style, columns: 2 }
    }

    pub fn cluster(id: u32, style: Style, columns: u8) -> GlyphKey {
        GlyphKey { id: GlyphId::Cluster(id), style, columns }
    }
}

/// Where a glyph lives, and how to place it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphEntry {
    pub page: u16,
    pub rect: Rect,
    pub left: i16,
    pub top: i16,
    pub format: PixelFormat,
}

impl GlyphEntry {
    /// A glyph with no pixels — a space, a control character. The renderer
    /// drops it before it ever becomes a quad.
    pub fn is_blank(&self) -> bool {
        self.rect.width == 0 || self.rect.height == 0
    }
}

/// A region of a page that `mica-gpu` must copy into its texture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upload {
    pub page: u16,
    pub rect: Rect,
    pub format: PixelFormat,
    pub data: Vec<u8>,
}

struct Page {
    format: PixelFormat,
    packer: ShelfPacker,
    /// Frame index of the most recent hit anywhere in this page.
    last_used: u64,
    glyphs: u32,
}

/// A generation counter, bumped whenever pages are dropped.
///
/// `mica-gpu` compares it against its own copy: a change means the textures
/// must be recreated, and every cached entry the renderer is holding is stale.
pub type Generation = u32;

pub struct Atlas {
    fonts: FontSet,
    page_size: u16,
    pages: Vec<Page>,
    entries: HashMap<GlyphKey, GlyphEntry>,
    /// Parallel to `entries`, so a hit is one hash lookup and one store.
    last_used: HashMap<GlyphKey, u64>,
    pending: Vec<Upload>,
    frame: u64,
    generation: Generation,
    /// Glyphs that could not be placed even in a fresh page. Remembered so a
    /// pathological glyph is not re-rasterised on every single frame.
    rejected: HashMap<GlyphKey, ()>,
}

impl std::fmt::Debug for Atlas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Atlas")
            .field("pages", &self.pages.len())
            .field("glyphs", &self.entries.len())
            .field("generation", &self.generation)
            .field("metrics", &self.fonts.metrics())
            .finish()
    }
}

/// 1024² holds well over a thousand cells at a typical size, which is more
/// distinct glyphs than a terminal session normally sees.
pub const DEFAULT_PAGE_SIZE: u16 = 1024;

/// A ceiling on texture memory. Eight 1024² pages is 8 MB greyscale, 32 MB if
/// they were all colour — and they never are.
const MAX_PAGES: usize = 8;

impl Atlas {
    pub fn new(fonts: FontSet) -> Atlas {
        Atlas::with_page_size(fonts, DEFAULT_PAGE_SIZE)
    }

    pub fn with_page_size(fonts: FontSet, page_size: u16) -> Atlas {
        Atlas {
            fonts,
            page_size: page_size.max(64),
            pages: Vec::new(),
            entries: HashMap::new(),
            last_used: HashMap::new(),
            pending: Vec::new(),
            frame: 0,
            generation: 0,
            rejected: HashMap::new(),
        }
    }

    pub fn metrics(&self) -> CellMetrics {
        self.fonts.metrics()
    }

    pub fn fonts(&self) -> &FontSet {
        &self.fonts
    }

    pub fn generation(&self) -> Generation {
        self.generation
    }

    pub fn page_size(&self) -> u16 {
        self.page_size
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn page_format(&self, index: u16) -> Option<PixelFormat> {
        self.pages.get(index as usize).map(|p| p.format)
    }

    pub fn glyph_count(&self) -> usize {
        self.entries.len()
    }

    /// Marks the start of a frame. Only used to order LRU eviction, so it is
    /// fine for the caller to skip it — eviction just becomes less informed.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }

    /// Looks a glyph up, rasterising it on a miss.
    ///
    /// `cluster_text` is consulted **only on a miss**, so the caller can pass a
    /// closure that reaches into the terminal's side table without paying for
    /// it on the overwhelmingly common hit path.
    pub fn glyph(
        &mut self,
        key: GlyphKey,
        cluster_text: impl FnOnce() -> Option<String>,
    ) -> Option<GlyphEntry> {
        if let Some(entry) = self.entries.get(&key).copied() {
            self.last_used.insert(key, self.frame);
            if let Some(page) = self.pages.get_mut(entry.page as usize) {
                page.last_used = self.frame;
            }
            return Some(entry);
        }
        if self.rejected.contains_key(&key) {
            return None;
        }

        let bitmap = match key.id {
            GlyphId::Scalar(ch) => {
                raster::rasterize_scalar(&self.fonts, ch, key.style, key.columns)?
            }
            GlyphId::Cluster(_) => {
                let text = cluster_text()?;
                raster::rasterize_cluster(&self.fonts, &text, key.style, key.columns)?
            }
        };
        self.insert(key, bitmap)
    }

    fn insert(&mut self, key: GlyphKey, bitmap: Bitmap) -> Option<GlyphEntry> {
        // A blank glyph is cached as an entry with an empty rect: it costs no
        // texture space, and caching it stops a screen full of spaces from
        // re-entering the rasteriser every frame.
        if bitmap.is_empty() {
            let entry = GlyphEntry {
                page: 0,
                rect: Rect { x: 0, y: 0, width: 0, height: 0 },
                left: 0,
                top: 0,
                format: bitmap.format,
            };
            self.entries.insert(key, entry);
            self.last_used.insert(key, self.frame);
            return Some(entry);
        }

        let placed = self
            .place(bitmap.format, bitmap.width, bitmap.height)
            .or_else(|| {
                // Nothing fits anywhere. Drop the coldest page and try once
                // more; a second failure means the glyph is simply too large.
                self.evict_coldest_page(bitmap.format)?;
                self.place(bitmap.format, bitmap.width, bitmap.height)
            });

        let Some((page_index, rect)) = placed else {
            self.rejected.insert(key, ());
            return None;
        };

        self.pages[page_index as usize].glyphs += 1;
        self.pages[page_index as usize].last_used = self.frame;
        self.pending.push(Upload {
            page: page_index,
            rect,
            format: bitmap.format,
            data: bitmap.data,
        });

        let entry = GlyphEntry {
            page: page_index,
            rect,
            left: bitmap.left,
            top: bitmap.top,
            format: bitmap.format,
        };
        self.entries.insert(key, entry);
        self.last_used.insert(key, self.frame);
        Some(entry)
    }

    /// Finds room in an existing page of the right format, or opens a new one.
    ///
    /// Greyscale and colour never share a page: they have different pixel
    /// formats, and packing them together would force every text glyph to pay
    /// four bytes per pixel so that the handful of emoji could have colour.
    fn place(&mut self, format: PixelFormat, width: u16, height: u16) -> Option<(u16, Rect)> {
        for (i, page) in self.pages.iter_mut().enumerate() {
            if page.format != format {
                continue;
            }
            if let Some(rect) = page.packer.allocate(width, height) {
                return Some((i as u16, rect));
            }
        }
        if self.pages.len() >= MAX_PAGES {
            return None;
        }
        let mut packer = ShelfPacker::new(self.page_size, self.page_size);
        let rect = packer.allocate(width, height)?;
        self.pages.push(Page { format, packer, last_used: self.frame, glyphs: 0 });
        Some((self.pages.len() as u16 - 1, rect))
    }

    /// Empties the least-recently-used page of the given format.
    ///
    /// The page index is kept so that `mica-gpu`'s texture array does not have
    /// to be reshuffled; only its contents are invalidated.
    fn evict_coldest_page(&mut self, format: PixelFormat) -> Option<()> {
        let (index, _) = self
            .pages
            .iter()
            .enumerate()
            .filter(|(_, p)| p.format == format)
            .min_by_key(|(_, p)| p.last_used)?;

        self.pages[index].packer.reset();
        self.pages[index].glyphs = 0;
        self.pages[index].last_used = self.frame;

        // Nothing to clean up on the GPU side: the freed texture region is
        // simply overwritten by whatever gets packed there next.
        let evicted = index as u16;
        self.entries.retain(|_, entry| entry.page != evicted || entry.is_blank());
        self.last_used.retain(|key, _| self.entries.contains_key(key));
        self.rejected.clear();
        self.generation = self.generation.wrapping_add(1);
        Some(())
    }

    /// Everything `mica-gpu` still has to copy into its textures.
    pub fn take_uploads(&mut self) -> Vec<Upload> {
        std::mem::take(&mut self.pending)
    }

    pub fn has_uploads(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Rebuilds for a new font, size, or backing-store scale.
    ///
    /// Every cached glyph is wrong after this — different metrics, different
    /// rasterisation — so the whole atlas goes, and the generation bump tells
    /// the renderer its textures went with it.
    pub fn rebuild(&mut self, fonts: FontSet) {
        self.fonts = fonts;
        self.pages.clear();
        self.entries.clear();
        self.last_used.clear();
        self.pending.clear();
        self.rejected.clear();
        self.generation = self.generation.wrapping_add(1);
    }

    /// Total texture bytes currently allocated.
    pub fn texture_bytes(&self) -> usize {
        self.pages
            .iter()
            .map(|p| {
                self.page_size as usize * self.page_size as usize * p.format.bytes_per_pixel()
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atlas() -> Atlas {
        Atlas::new(FontSet::resolve("Menlo", 13.0, 2.0))
    }

    fn none() -> Option<String> {
        None
    }

    #[test]
    fn a_letter_is_rasterised_once_and_cached_thereafter() {
        let mut a = atlas();
        let key = GlyphKey::scalar('A', Style::REGULAR);

        let first = a.glyph(key, none).unwrap();
        assert_eq!(a.take_uploads().len(), 1, "the first lookup must upload");

        let second = a.glyph(key, none).unwrap();
        assert_eq!(first, second);
        assert!(a.take_uploads().is_empty(), "a cache hit must not re-upload");
    }

    #[test]
    fn a_streaming_log_stops_rasterising_after_the_first_screenful() {
        // The load-bearing claim: a 200 MB log costs what an empty prompt
        // costs, because the glyph repertoire is bounded.
        let mut a = atlas();
        let text = "  Compiling mica-core v0.1.0 (/Users/me/Mica)";
        for ch in text.chars() {
            a.glyph(GlyphKey::scalar(ch, Style::REGULAR), none);
        }
        let distinct = a.glyph_count();
        a.take_uploads();

        // Now "stream" the same line ten thousand more times.
        for _ in 0..10_000 {
            for ch in text.chars() {
                a.glyph(GlyphKey::scalar(ch, Style::REGULAR), none);
            }
        }
        assert_eq!(a.glyph_count(), distinct, "the repertoire grew while streaming");
        assert!(a.take_uploads().is_empty(), "streaming re-uploaded glyphs");
    }

    #[test]
    fn each_style_is_its_own_glyph() {
        let mut a = atlas();
        let regular = a.glyph(GlyphKey::scalar('A', Style::REGULAR), none).unwrap();
        let bold = a.glyph(GlyphKey::scalar('A', Style::new(true, false)), none).unwrap();
        assert_ne!(regular.rect, bold.rect, "bold and regular share a rectangle");
        assert_eq!(a.glyph_count(), 2);
    }

    #[test]
    fn a_space_costs_no_texture_space_but_is_still_cached() {
        let mut a = atlas();
        let entry = a.glyph(GlyphKey::scalar(' ', Style::REGULAR), none).unwrap();
        assert!(entry.is_blank());
        assert!(a.take_uploads().is_empty(), "a space must not produce an upload");
        assert_eq!(a.page_count(), 0, "a screen of spaces must not open a page");

        // Cached, so a screen full of spaces does not re-enter the rasteriser.
        a.glyph(GlyphKey::scalar(' ', Style::REGULAR), none).unwrap();
        assert_eq!(a.glyph_count(), 1);
    }

    #[test]
    fn colour_and_greyscale_never_share_a_page() {
        let mut a = atlas();
        let letter = a.glyph(GlyphKey::scalar('A', Style::REGULAR), none).unwrap();
        let emoji = a.glyph(GlyphKey::wide('\u{1F680}', Style::REGULAR), none).unwrap();

        assert_eq!(letter.format, PixelFormat::Alpha8);
        assert_eq!(emoji.format, PixelFormat::Bgra8);
        assert_ne!(letter.page, emoji.page, "an emoji landed on the greyscale page");
        assert_eq!(a.page_format(letter.page), Some(PixelFormat::Alpha8));
        assert_eq!(a.page_format(emoji.page), Some(PixelFormat::Bgra8));
    }

    #[test]
    fn a_cluster_asks_for_its_text_only_on_a_miss() {
        let mut a = atlas();
        let key = GlyphKey::cluster(7, Style::REGULAR, 2);
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";

        let mut asked = 0;
        let mut lookup = |a: &mut Atlas, asked: &mut i32| {
            a.glyph(key, || {
                *asked += 1;
                Some(family.to_owned())
            })
        };
        assert!(lookup(&mut a, &mut asked).is_some());
        assert_eq!(asked, 1);
        assert!(lookup(&mut a, &mut asked).is_some());
        assert_eq!(asked, 1, "a cache hit must not reach into the side table");
    }

    #[test]
    fn a_cluster_with_no_text_available_is_not_cached_as_a_failure_forever() {
        let mut a = atlas();
        let key = GlyphKey::cluster(99, Style::REGULAR, 1);
        assert!(a.glyph(key, none).is_none());
        // The id later becomes resolvable; the atlas must not have given up.
        assert!(a.glyph(key, || Some("\u{1F1FA}\u{1F1F8}".to_owned())).is_some());
    }

    #[test]
    fn uploads_are_drained_not_repeated() {
        let mut a = atlas();
        for ch in "hello".chars() {
            a.glyph(GlyphKey::scalar(ch, Style::REGULAR), none);
        }
        assert!(a.has_uploads());
        let first = a.take_uploads();
        assert_eq!(first.len(), 4, "h, e, l, o — the second l is a cache hit");
        assert!(!a.has_uploads());
        assert!(a.take_uploads().is_empty());
    }

    #[test]
    fn an_upload_carries_exactly_its_rectangle_of_pixels() {
        let mut a = atlas();
        a.glyph(GlyphKey::scalar('W', Style::REGULAR), none).unwrap();
        let upload = a.take_uploads().pop().unwrap();
        let expected = upload.rect.area() as usize * upload.format.bytes_per_pixel();
        assert_eq!(upload.data.len(), expected, "upload size disagrees with its rect");
    }

    #[test]
    fn allocated_rectangles_never_overlap_within_a_page() {
        let mut a = atlas();
        let mut placed: Vec<(u16, Rect)> = Vec::new();
        for ch in ('!'..='~').chain('\u{2500}'..='\u{2540}') {
            if let Some(e) = a.glyph(GlyphKey::scalar(ch, Style::REGULAR), none) {
                if e.is_blank() {
                    continue;
                }
                for (page, rect) in &placed {
                    if *page == e.page {
                        assert!(!e.rect.intersects(rect), "{:?} overlaps {rect:?}", e.rect);
                    }
                }
                placed.push((e.page, e.rect));
            }
        }
        assert!(placed.len() > 100);
    }

    #[test]
    fn rebuilding_for_a_new_size_invalidates_everything() {
        let mut a = atlas();
        a.glyph(GlyphKey::scalar('A', Style::REGULAR), none).unwrap();
        a.take_uploads();
        let generation = a.generation();

        a.rebuild(FontSet::resolve("Menlo", 24.0, 2.0));
        assert_ne!(a.generation(), generation, "the renderer was not told to drop its textures");
        assert_eq!(a.glyph_count(), 0);
        assert_eq!(a.page_count(), 0);

        // …and the next lookup rasterises at the new size.
        let entry = a.glyph(GlyphKey::scalar('A', Style::REGULAR), none).unwrap();
        assert!(!entry.is_blank());
        assert_eq!(a.take_uploads().len(), 1);
    }

    #[test]
    fn a_full_atlas_evicts_a_page_rather_than_failing() {
        // A tiny page forces the eviction path that would otherwise never run.
        let mut a = Atlas::with_page_size(FontSet::resolve("Menlo", 13.0, 1.0), 64);
        let mut placed = 0;
        for cp in 0x4E00u32..0x4E00 + 4000 {
            let ch = char::from_u32(cp).unwrap();
            a.begin_frame();
            if a.glyph(GlyphKey::wide(ch, Style::REGULAR), none).is_some() {
                placed += 1;
            }
            a.take_uploads();
        }
        assert!(placed > 100, "eviction stalled after {placed} glyphs");
        assert!(a.generation() > 0, "pages were never evicted");
        assert!(a.page_count() <= MAX_PAGES);
    }

    #[test]
    fn texture_memory_stays_bounded() {
        let mut a = Atlas::with_page_size(FontSet::resolve("Menlo", 13.0, 1.0), 256);
        for cp in 0x4E00u32..0x4E00 + 3000 {
            a.begin_frame();
            a.glyph(GlyphKey::wide(char::from_u32(cp).unwrap(), Style::REGULAR), none);
            a.take_uploads();
        }
        // Eight 256² pages at four bytes is the absolute ceiling.
        assert!(a.texture_bytes() <= MAX_PAGES * 256 * 256 * 4);
    }

    #[test]
    fn box_drawing_glyphs_are_cell_exact_in_the_atlas() {
        let mut a = atlas();
        let m = a.metrics();
        let entry = a.glyph(GlyphKey::scalar('\u{253C}', Style::REGULAR), none).unwrap();
        assert_eq!(entry.rect.width, m.width);
        assert_eq!(entry.rect.height, m.height);
        assert_eq!((entry.left, entry.top), (0, 0));
    }
}
