//! `mica-core` — terminal state, PTY, semantics, settings.
//!
//! **This crate must never import Metal or AppKit.** Dependencies point
//! inward: `mica-shell` → `mica-gpu` → `mica-atlas` → `mica-core`. The crate
//! boundary is the single most valuable thing recovered from the reference
//! binary, and it is enforced by `xtask check-layering` in CI, not by good
//! intentions.

/// A minimal stand-in for the `bitflags` crate.
///
/// Not invented for fun: the whole point of this codebase is a very small
/// dependency graph, and the ~40 lines below are the entire subset of
/// `bitflags` that the grid actually uses.
macro_rules! bitflags_lite {
    (
        $(#[$outer:meta])*
        pub struct $name:ident: $ty:ty {
            $(
                $(#[$inner:meta])*
                const $flag:ident = $value:expr;
            )*
        }
    ) => {
        $(#[$outer])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
        #[repr(transparent)]
        pub struct $name($ty);

        impl $name {
            pub const EMPTY: $name = $name(0);
            $(
                $(#[$inner])*
                pub const $flag: $name = $name($value);
            )*

            pub const fn bits(self) -> $ty { self.0 }
            pub const fn from_bits_truncate(bits: $ty) -> $name { $name(bits) }
            pub const fn contains(self, other: $name) -> bool { self.0 & other.0 == other.0 }
            pub const fn intersects(self, other: $name) -> bool { self.0 & other.0 != 0 }
            pub const fn union(self, other: $name) -> $name { $name(self.0 | other.0) }
            pub const fn intersection(self, other: $name) -> $name { $name(self.0 & other.0) }
            pub const fn difference(self, other: $name) -> $name { $name(self.0 & !other.0) }
            pub const fn is_empty(self) -> bool { self.0 == 0 }

            pub fn insert(&mut self, other: $name) { self.0 |= other.0; }
            pub fn remove(&mut self, other: $name) { self.0 &= !other.0; }
            pub fn set(&mut self, other: $name, on: bool) {
                if on { self.insert(other) } else { self.remove(other) }
            }
        }

        impl core::ops::BitOr for $name {
            type Output = $name;
            fn bitor(self, o: $name) -> $name { $name(self.0 | o.0) }
        }
        impl core::ops::BitAnd for $name {
            type Output = $name;
            fn bitand(self, o: $name) -> $name { $name(self.0 & o.0) }
        }
        impl core::ops::BitOrAssign for $name {
            fn bitor_assign(&mut self, o: $name) { self.0 |= o.0; }
        }
        impl core::fmt::Debug for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, concat!(stringify!($name), "({:#06x})"), self.0)
            }
        }
    };
}

pub mod backend;
pub mod cell;
pub mod material;
pub mod pty;
pub mod semantic;
pub mod session;
pub mod settings;
pub mod sidetable;

pub use cell::{Cell, CellContent, CellFlags, Color};
pub use backend::{CursorShape, CursorState, RowRef, Selection, TerminalCore};
pub use semantic::{Block, BlockStatus, SemanticEvent};

/// The `TERM` name Mica claims — but only once `tic -x` has succeeded. See
/// `mica-core::session::terminfo`.
pub const TERM_NAME: &str = "mica";
pub const TERM_PROGRAM: &str = "Mica";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
