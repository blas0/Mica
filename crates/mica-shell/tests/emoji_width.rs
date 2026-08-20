//! How many columns an emoji occupies.
//!
//! Rendering is not the problem here — a bare `🚀` or `✅` rasterises in full
//! colour, centred across its two cells, verified pixel by pixel. The problem
//! is *arithmetic*, and it is one column wide.
//!
//! An emoji-presentation sequence is a base character followed by `U+FE0F`:
//! `⚠️`, `✔️`, `ℹ️`. Unicode TR51 says these are wide, and every terminal a
//! CLI is developed against — Ghostty, WezTerm, kitty, iTerm2 — gives them two
//! columns. So does Node's `string-width`, which is what a great many CLIs use
//! to lay out a box or a table.
//!
//! `alacritty_terminal` gives them one. It calls `char::width()` per scalar, so
//! `U+FE0F` arrives as a zero-width continuation and is folded onto the
//! previous cell without widening it. There is no way to correct this from
//! outside the parser: nothing Mica can put in the byte stream makes the
//! terminal allocate a second cell for a character it has already measured.
//!
//! The consequence is visible and cumulative: every `⚠️` in a program's output
//! shifts the rest of its line one column left of where the program placed it,
//! and box-drawn frames come apart.
//!
//! It is fixed by the `ghostty` backend, which measures grapheme clusters
//! rather than scalars. That backend is written and gated behind a Zig
//! toolchain that is not installed. This test is the exit criterion: when it
//! passes without `--ignored`, the bug is gone.

use std::time::{Duration, Instant};

use mica_core::settings::Settings;
use mica_shell::surface::Surface;

fn settle(s: &mut Surface, within: Duration, mut predicate: impl FnMut(&mut Surface) -> bool) {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        s.pump();
        if predicate(s) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The width the terminal gave each cell of the row containing `marker`.
fn widths_of_row_with(s: &mut Surface, marker: char) -> Option<Vec<u8>> {
    s.session().damage_all();
    let rows: Vec<Vec<u8>> = s
        .session()
        .dirty_rows()
        .filter(|row| row.cells.iter().any(|c| c.content.as_scalar() == Some(marker)))
        .map(|row| row.cells.iter().map(|c| c.width).collect())
        .collect();
    rows.into_iter().next()
}

#[test]
#[ignore = "needs the ghostty backend: alacritty measures scalars, not clusters"]
fn an_emoji_presentation_sequence_occupies_two_columns() {
    let root = std::env::temp_dir().join(format!("mica-emoji-width-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let mut s = Surface::open(Settings::default(), (800, 480), 2.0, root.clone(), None)
        .expect("a surface should open");

    settle(&mut s, Duration::from_secs(5), |s| {
        let text: String = {
            s.session().damage_all();
            s.session()
                .dirty_rows()
                .flat_map(|r| r.cells.iter().map(|c| c.content.as_scalar().unwrap_or(' ')))
                .collect()
        };
        text.contains('%') || text.contains('$')
    });

    // `@` is the marker: it makes the row findable without depending on where
    // the shell decided to put its prompt.
    s.write_input("printf '@\\u{26a0}\\u{fe0f}#\\n'\n".as_bytes());
    settle(&mut s, Duration::from_secs(5), |s| {
        widths_of_row_with(s, '#').is_some()
    });

    let widths = widths_of_row_with(&mut s, '#').expect("the line never appeared");
    assert_eq!(
        widths.get(1).copied(),
        Some(2),
        "⚠\u{fe0f} was given {:?} columns; every CLI laying out that line assumed two",
        widths.get(1)
    );

    drop(s);
    let _ = std::fs::remove_dir_all(&root);
}
