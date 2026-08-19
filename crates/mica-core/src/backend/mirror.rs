//! A translated copy of the visible grid, plus the damage set over it.
//!
//! Both backends keep their own internal representation — alacritty's `Grid`,
//! libghostty's page list — and neither is a `[Cell]` in Mica's layout. Rather
//! than teach the renderer two shapes, each backend translates *only the rows
//! its damage set names* into this mirror. The renderer then borrows plain
//! slices and never learns which backend it is talking to.
//!
//! The mirror is the reason `RowRef` can be a `&[Cell]` instead of a trait
//! object or a callback.

use crate::backend::RowRef;
use crate::cell::Cell;

/// A dense grid of [`Cell`] plus a per-row dirty bit.
#[derive(Debug)]
pub struct Mirror {
    cols: u16,
    rows: u16,
    cells: Vec<Cell>,
    wrapped: Vec<bool>,
    dirty: Vec<u64>,
    dirty_count: usize,
}

impl Mirror {
    pub fn new(cols: u16, rows: u16) -> Mirror {
        let mut m = Mirror {
            cols: 0,
            rows: 0,
            cells: Vec::new(),
            wrapped: Vec::new(),
            dirty: Vec::new(),
            dirty_count: 0,
        };
        m.resize(cols, rows);
        m
    }

    pub fn dimensions(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// Reshapes and marks everything dirty — a resize invalidates the whole
    /// surface by definition, so this is the one place a full damage is not a
    /// smell.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        self.cols = cols;
        self.rows = rows;
        self.cells.clear();
        self.cells.resize(cols as usize * rows as usize, Cell::EMPTY);
        self.wrapped.clear();
        self.wrapped.resize(rows as usize, false);
        self.dirty.clear();
        self.dirty.resize((rows as usize).div_ceil(64), 0);
        self.dirty_count = 0;
        self.damage_all();
    }

    /// Overwrites one row and marks it dirty **only if it actually changed**.
    ///
    /// Backends over-report damage: alacritty marks a row damaged when the
    /// cursor merely passes through it, and a blinking-cursor row would
    /// otherwise wake the renderer forever. Comparing here is what makes
    /// "an idle terminal submits zero command buffers" survive contact with a
    /// real VT implementation.
    pub fn put_row(&mut self, index: u16, cells: &[Cell], wrapped: bool) -> bool {
        if index >= self.rows {
            return false;
        }
        let start = index as usize * self.cols as usize;
        let end = start + self.cols as usize;
        let dst = &mut self.cells[start..end];

        let n = cells.len().min(dst.len());
        let unchanged = dst[..n] == cells[..n]
            && dst[n..].iter().all(|c| *c == Cell::EMPTY)
            && self.wrapped[index as usize] == wrapped;
        if unchanged {
            return false;
        }

        dst[..n].copy_from_slice(&cells[..n]);
        for c in &mut dst[n..] {
            *c = Cell::EMPTY;
        }
        self.wrapped[index as usize] = wrapped;
        self.mark_dirty(index);
        true
    }

    pub fn row(&self, index: u16) -> &[Cell] {
        let start = index as usize * self.cols as usize;
        &self.cells[start..start + self.cols as usize]
    }

    pub fn mark_dirty(&mut self, index: u16) {
        if index >= self.rows {
            return;
        }
        let (word, bit) = (index as usize / 64, index as usize % 64);
        if self.dirty[word] & (1 << bit) == 0 {
            self.dirty[word] |= 1 << bit;
            self.dirty_count += 1;
        }
    }

    pub fn damage_all(&mut self) {
        for w in &mut self.dirty {
            *w = !0;
        }
        // Clear the padding bits in the last word so `dirty_count` stays honest.
        let tail = self.rows as usize % 64;
        if tail != 0 {
            if let Some(last) = self.dirty.last_mut() {
                *last = (1u64 << tail) - 1;
            }
        }
        self.dirty_count = self.rows as usize;
    }

    pub fn clear_damage(&mut self) {
        for w in &mut self.dirty {
            *w = 0;
        }
        self.dirty_count = 0;
    }

    pub fn has_damage(&self) -> bool {
        self.dirty_count != 0
    }

    pub fn dirty_count(&self) -> usize {
        self.dirty_count
    }

    pub fn is_dirty(&self, index: u16) -> bool {
        if index >= self.rows {
            return false;
        }
        self.dirty[index as usize / 64] & (1 << (index as usize % 64)) != 0
    }

    pub fn dirty_rows(&self) -> impl Iterator<Item = RowRef<'_>> {
        (0..self.rows).filter(move |&i| self.is_dirty(i)).map(move |i| RowRef {
            index: i,
            cells: self.row(i),
            wrapped: self.wrapped[i as usize],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::{CellContent, CellFlags, Color};

    fn row_of(text: &str, cols: usize) -> Vec<Cell> {
        let mut v = vec![Cell::EMPTY; cols];
        for (i, ch) in text.chars().enumerate().take(cols) {
            v[i] = Cell::new(
                CellContent::scalar(ch),
                Color::DEFAULT,
                Color::DEFAULT,
                CellFlags::EMPTY,
            );
        }
        v
    }

    #[test]
    fn a_fresh_mirror_is_entirely_dirty() {
        let m = Mirror::new(10, 4);
        assert_eq!(m.dirty_count(), 4);
        assert_eq!(m.dirty_rows().count(), 4);
    }

    #[test]
    fn clearing_damage_yields_no_dirty_rows() {
        let mut m = Mirror::new(10, 4);
        m.clear_damage();
        assert!(!m.has_damage());
        assert_eq!(m.dirty_rows().count(), 0);
    }

    #[test]
    fn writing_an_identical_row_does_not_redirty_it() {
        let mut m = Mirror::new(10, 4);
        let r = row_of("hello", 10);
        m.put_row(1, &r, false);
        m.clear_damage();

        assert!(!m.put_row(1, &r, false), "identical content must not report a change");
        assert!(!m.has_damage(), "an idle repaint must not wake the renderer");
    }

    #[test]
    fn writing_a_changed_row_dirties_exactly_that_row() {
        let mut m = Mirror::new(10, 4);
        m.clear_damage();
        m.put_row(2, &row_of("hi", 10), false);
        assert_eq!(m.dirty_count(), 1);
        let dirty: Vec<u16> = m.dirty_rows().map(|r| r.index).collect();
        assert_eq!(dirty, vec![2]);
    }

    #[test]
    fn damage_count_is_exact_when_rows_do_not_fill_a_bitset_word() {
        let mut m = Mirror::new(10, 70);
        m.clear_damage();
        m.damage_all();
        assert_eq!(m.dirty_count(), 70);
        assert_eq!(m.dirty_rows().count(), 70);
    }

    #[test]
    fn a_shorter_row_blanks_the_tail() {
        let mut m = Mirror::new(10, 2);
        m.put_row(0, &row_of("abcdefghij", 10), false);
        m.put_row(0, &row_of("ab", 2), false);
        assert_eq!(m.row(0)[5], Cell::EMPTY);
    }
}
