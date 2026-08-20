//! Scrolling inside a full-screen program.
//!
//! A CLI that takes over the screen — `less`, `vim`, `htop`, `claude` — runs on
//! the alternate screen, which by definition has no scrollback. A wheel gesture
//! there moved a viewport that could not move, so scrolling inside those
//! programs did nothing whatsoever. This is the end-to-end proof that it now
//! reaches the child, because the unit tests in `scroll.rs` only prove the
//! decision, not that anyone acts on it.

use std::time::{Duration, Instant};

use mica_core::settings::Settings;
use mica_shell::surface::Surface;

fn temp_root(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("mica-altscroll-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Pumps until `predicate` holds, or gives up. The child is a real shell on a
/// real PTY; nothing about its timing is ours to assume.
fn pump_until(s: &mut Surface, within: Duration, mut predicate: impl FnMut(&mut Surface) -> bool) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        s.pump();
        if predicate(s) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    predicate(s)
}

/// Everything currently on the visible screen, as one string.
fn screen(s: &mut Surface) -> String {
    s.session().damage_all();
    let mut out = String::new();
    for row in s.session().dirty_rows() {
        for cell in row.cells.iter() {
            out.push(cell.content.as_scalar().unwrap_or(' '));
        }
        out.push('\n');
    }
    out
}

#[test]
fn a_scroll_gesture_on_the_alternate_screen_reaches_the_child() {
    let root = temp_root("reaches");
    let mut s = Surface::open(Settings::default(), (800, 480), 2.0, root.clone(), None)
        .expect("a surface should open on this machine");

    // Settle the shell before asking it to do anything.
    assert!(
        pump_until(&mut s, Duration::from_secs(5), |s| screen(s).contains('%')
            || screen(s).contains('$')),
        "the shell never printed a prompt"
    );

    // Enter the alternate screen and echo whatever arrives, visibly.
    s.write_input(b"printf '\\033[?1049h'; cat -v\n");
    assert!(
        pump_until(&mut s, Duration::from_secs(5), |s| s.session().modes().alt_screen),
        "the terminal never entered the alternate screen"
    );

    s.scroll(2);

    assert!(
        pump_until(&mut s, Duration::from_secs(5), |s| screen(s).contains("^[[A^[[A")),
        "two scroll-up lines produced no arrow keys on the child's stdin; the \
         screen was:\n{}",
        screen(&mut s)
    );

    // Leave the child in a state that exits cleanly.
    s.write_input(&[0x04]);
    drop(s);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_normal_screen_still_moves_the_viewport() {
    // The other half: the fix must not have taught every scroll to type.
    let root = temp_root("normal");
    let mut s = Surface::open(Settings::default(), (800, 480), 2.0, root.clone(), None)
        .expect("a surface should open");

    assert!(
        pump_until(&mut s, Duration::from_secs(5), |s| screen(s).contains('%')
            || screen(s).contains('$')),
        "the shell never printed a prompt"
    );
    s.write_input(b"seq 1 200\n");
    assert!(
        pump_until(&mut s, Duration::from_secs(5), |s| screen(s).contains("200")),
        "seq never finished"
    );
    assert!(!s.session().modes().alt_screen);

    let before = screen(&mut s);
    s.scroll(10);
    s.pump();
    assert_ne!(before, screen(&mut s), "scrolling back through history changed nothing");

    drop(s);
    let _ = std::fs::remove_dir_all(&root);
}
