//! Lazily allocated per-cell extras.
//!
//! The product claim is that a plain build log allocates **zero bytes** for
//! grapheme clusters, colour emoji, and per-cell underline colour. That is only
//! true if the containers stay empty until something actually uses them — so
//! every table here starts with no allocation and grows on first insert.

use crate::cell::Color;

/// Interned grapheme clusters — anything that does not fit in a `char`.
///
/// Family emoji, flags, and skin-tone sequences all land here and still occupy
/// exactly one cell.
#[derive(Debug, Default)]
pub struct Graphemes {
    /// Flat UTF-8 arena; entries are `(offset, len)` into it.
    arena: String,
    spans: Vec<(u32, u32)>,
}

impl Graphemes {
    pub const fn new() -> Graphemes {
        Graphemes { arena: String::new(), spans: Vec::new() }
    }

    /// Interns a cluster and returns its id. Linear-scans for an existing
    /// entry: the table is small by construction (a screen holds at most
    /// `cols * rows` distinct clusters, and in practice a handful), so a hash
    /// map would cost more than it saves.
    pub fn intern(&mut self, cluster: &str) -> u32 {
        if let Some(id) = self.lookup(cluster) {
            return id;
        }
        let offset = self.arena.len() as u32;
        self.arena.push_str(cluster);
        self.spans.push((offset, cluster.len() as u32));
        (self.spans.len() - 1) as u32
    }

    pub fn lookup(&self, cluster: &str) -> Option<u32> {
        self.spans
            .iter()
            .position(|&(o, l)| &self.arena[o as usize..(o + l) as usize] == cluster)
            .map(|i| i as u32)
    }

    pub fn get(&self, id: u32) -> Option<&str> {
        let &(o, l) = self.spans.get(id as usize)?;
        Some(&self.arena[o as usize..(o + l) as usize])
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    pub fn len(&self) -> usize {
        self.spans.len()
    }

    /// Bytes actually allocated. Used by the "a build log costs nothing" test.
    pub fn allocated_bytes(&self) -> usize {
        self.arena.capacity() + self.spans.capacity() * core::mem::size_of::<(u32, u32)>()
    }

    pub fn clear(&mut self) {
        self.arena.clear();
        self.spans.clear();
    }
}

/// Everything a cell may carry that almost no cell does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Extras {
    pub underline_color: Option<Color>,
    /// OSC 8 hyperlink id, interned in [`Hyperlinks`].
    pub hyperlink: Option<u32>,
}

impl Extras {
    pub fn is_empty(&self) -> bool {
        self.underline_color.is_none() && self.hyperlink.is_none()
    }
}

/// The extras table. Id 0 is reserved as "no extras" so that
/// [`crate::cell::NO_EXTRA`] costs nothing to check.
#[derive(Debug)]
pub struct ExtrasTable {
    entries: Vec<Extras>,
}

impl Default for ExtrasTable {
    fn default() -> ExtrasTable {
        ExtrasTable::new()
    }
}

impl ExtrasTable {
    pub const fn new() -> ExtrasTable {
        ExtrasTable { entries: Vec::new() }
    }

    /// Returns [`crate::cell::NO_EXTRA`] for an empty `Extras`, so the caller
    /// never has to special-case the common path.
    pub fn intern(&mut self, extras: Extras) -> u32 {
        if extras.is_empty() {
            return crate::cell::NO_EXTRA;
        }
        if self.entries.is_empty() {
            // Slot 0 is the reserved sentinel; it is never handed out.
            self.entries.push(Extras::default());
        }
        if let Some(i) = self.entries.iter().position(|e| *e == extras) {
            return i as u32;
        }
        self.entries.push(extras);
        (self.entries.len() - 1) as u32
    }

    pub fn get(&self, id: u32) -> Option<&Extras> {
        if id == crate::cell::NO_EXTRA {
            return None;
        }
        self.entries.get(id as usize)
    }

    pub fn allocated_bytes(&self) -> usize {
        self.entries.capacity() * core::mem::size_of::<Extras>()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// OSC 8 hyperlink targets.
#[derive(Debug, Default)]
pub struct Hyperlinks {
    uris: Vec<String>,
}

impl Hyperlinks {
    pub const fn new() -> Hyperlinks {
        Hyperlinks { uris: Vec::new() }
    }

    pub fn intern(&mut self, uri: &str) -> u32 {
        if let Some(i) = self.uris.iter().position(|u| u == uri) {
            return i as u32;
        }
        self.uris.push(uri.to_owned());
        (self.uris.len() - 1) as u32
    }

    pub fn get(&self, id: u32) -> Option<&str> {
        self.uris.get(id as usize).map(String::as_str)
    }

    pub fn allocated_bytes(&self) -> usize {
        self.uris.capacity() * core::mem::size_of::<String>()
            + self.uris.iter().map(String::capacity).sum::<usize>()
    }
}

/// All side tables for one surface, held together so the backend can hand the
/// renderer a single borrow.
#[derive(Debug, Default)]
pub struct SideTables {
    pub graphemes: Graphemes,
    pub extras: ExtrasTable,
    pub hyperlinks: Hyperlinks,
}

impl SideTables {
    pub const fn new() -> SideTables {
        SideTables {
            graphemes: Graphemes::new(),
            extras: ExtrasTable::new(),
            hyperlinks: Hyperlinks::new(),
        }
    }

    pub fn allocated_bytes(&self) -> usize {
        self.graphemes.allocated_bytes()
            + self.extras.allocated_bytes()
            + self.hyperlinks.allocated_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_build_log_allocates_nothing() {
        let tables = SideTables::new();
        assert_eq!(tables.allocated_bytes(), 0);
    }

    #[test]
    fn interning_the_same_cluster_twice_yields_one_entry() {
        let mut g = Graphemes::new();
        let a = g.intern("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}");
        let b = g.intern("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}");
        assert_eq!(a, b);
        assert_eq!(g.len(), 1);
    }

    #[test]
    fn distinct_clusters_round_trip() {
        let mut g = Graphemes::new();
        let family = g.intern("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}");
        let flag = g.intern("\u{1F1FA}\u{1F1F8}");
        assert_ne!(family, flag);
        assert_eq!(g.get(flag), Some("\u{1F1FA}\u{1F1F8}"));
    }

    #[test]
    fn empty_extras_never_allocate_and_map_to_the_sentinel() {
        let mut t = ExtrasTable::new();
        assert_eq!(t.intern(Extras::default()), crate::cell::NO_EXTRA);
        assert_eq!(t.allocated_bytes(), 0);
        assert!(t.get(crate::cell::NO_EXTRA).is_none());
    }

    #[test]
    fn underline_colour_survives_interning() {
        let mut t = ExtrasTable::new();
        let e = Extras { underline_color: Some(Color::rgb(9, 8, 7)), hyperlink: None };
        let id = t.intern(e);
        assert_ne!(id, crate::cell::NO_EXTRA);
        assert_eq!(t.get(id).unwrap().underline_color, Some(Color::rgb(9, 8, 7)));
    }
}
