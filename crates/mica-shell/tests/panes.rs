//! Splitting a window, end to end.
//!
//! The layout itself is unit-tested in `pane.rs` with no shell in sight. This
//! is the other half: that a split actually starts a second shell, that both
//! shells are the size the layout says they are, that input goes to the
//! focused one only, and that the pixels come out where the tree says.

use std::time::{Duration, Instant};

use mica_core::settings::{PaneOrigin, Settings};
use mica_gpu::renderer::{pixel_at, read_back};
use mica_shell::surface::Surface;

fn temp_root(name: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("mica-panes-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn open(name: &str, settings: Settings) -> (Surface, std::path::PathBuf) {
    let root = temp_root(name);
    let mut s = Surface::open(settings, (1200, 720), 2.0, root.clone(), None)
        .expect("a surface should open on this machine");
    s.set_settings_path(root.join("settings.toml"));
    (s, root)
}

fn pump_until(s: &mut Surface, within: Duration, mut done: impl FnMut(&mut Surface) -> bool) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        s.pump();
        if done(s) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    done(s)
}

/// Everything on one pane's screen, as a string.
fn screen(s: &mut Surface, index: usize) -> String {
    let mut out = String::new();
    s.pane_session(index).damage_all();
    let rows: Vec<String> = s
        .pane_session(index)
        .dirty_rows()
        .map(|row| row.cells.iter().map(|c| c.content.as_scalar().unwrap_or(' ')).collect())
        .collect();
    for row in rows {
        out.push_str(&row);
        out.push('\n');
    }
    out
}

/// One pane's screen with the row padding removed and the rows run together.
///
/// A 37-column pane wraps `echo probe` across two rows, and a search that
/// keeps the row breaks would not find it. What is on screen is the text, not
/// the lines it happened to land on.
fn flat(s: &mut Surface, index: usize) -> String {
    screen(s, index).lines().map(str::trim_end).collect::<Vec<_>>().concat()
}

fn settle(s: &mut Surface) {
    pump_until(s, Duration::from_secs(5), |s| {
        (0..s.pane_count()).all(|i| screen(s, i).contains('%') || screen(s, i).contains('$'))
    });
}

#[test]
fn a_split_starts_a_second_shell_that_answers_for_itself() {
    // The whole feature in one test: two panes, two live shells, and input
    // reaching only the focused one.
    let (mut s, root) = open("two-shells", Settings::default());
    settle(&mut s);
    assert_eq!(s.pane_count(), 1);

    assert!(s.dispatch("pane.split_right"), "the split was refused");
    assert_eq!(s.pane_count(), 2);
    settle(&mut s);

    s.write_input(b"echo pane-probe-alpha\n");
    assert!(
        pump_until(&mut s, Duration::from_secs(5), |s| {
            (0..s.pane_count()).any(|i| flat(s, i).contains("pane-probe-alpha"))
        }),
        "neither pane echoed the command"
    );

    let hits = (0..s.pane_count())
        .filter(|i| flat(&mut s, *i).contains("pane-probe-alpha"))
        .count();
    assert_eq!(hits, 1, "input reached {hits} panes; it belongs to the focused one only");

    drop(s);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn each_shell_is_told_the_size_of_its_own_pane() {
    // A shell that thinks it is full-width will wrap its prompt across the
    // divider. This is the check that `SIGWINCH` actually went out.
    let (mut s, root) = open("sizes", Settings::default());
    settle(&mut s);
    let (full_cols, full_rows) = s.pane_session(0).dimensions();

    assert!(s.dispatch("pane.split_right"));
    settle(&mut s);

    let a = s.pane_session(0).dimensions();
    let b = s.pane_session(1).dimensions();
    assert!(a.0 < full_cols, "the original pane was never told it got narrower");
    assert_eq!(a.1, full_rows, "a side-by-side split changed the number of rows");
    assert_eq!(b.1, full_rows);
    // One cell for the divider.
    assert_eq!(a.0 + b.0 + 1, full_cols);

    assert!(s.dispatch("pane.split_down"));
    settle(&mut s);
    let stacked: Vec<u16> = (0..s.pane_count()).map(|i| s.pane_session(i).dimensions().1).collect();
    assert!(stacked.iter().any(|r| *r < full_rows), "a stacked split left every pane full height");

    drop(s);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_new_pane_starts_where_the_focused_one_is() {
    // `new-pane-from-focused-pane = inherit`, which is the default: split in a
    // directory and the new shell is already there.
    let (mut s, root) = open("inherit", Settings::default());
    settle(&mut s);

    // Wait on the shell *reporting* the new directory, not on text appearing.
    // The screen shows the command as you type it, so "did the output show
    // up?" is answered `true` by the echo before the `cd` has run.
    s.write_input(b"cd /usr/lib\n");
    assert!(
        pump_until(&mut s, Duration::from_secs(5), |s| s.pane_session(0).cwd()
            == Some("/usr/lib")),
        "the shell never reported the new working directory"
    );

    assert!(s.dispatch("pane.split_right"));
    settle(&mut s);
    s.write_input(b"pwd\n");
    assert!(
        pump_until(&mut s, Duration::from_secs(5), |s| s.pane_session(1).cwd()
            == Some("/usr/lib")),
        "the new pane did not inherit the working directory; it showed:\n{}",
        screen(&mut s, 1)
    );

    drop(s);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn starting_dir_wins_when_the_setting_says_so() {
    let mut settings = Settings::default();
    settings.shell.new_pane_origin = PaneOrigin::StartingDir;
    settings.shell.starting_dir = Some("/usr/lib".into());
    let (mut s, root) = open("starting-dir", settings);
    settle(&mut s);

    s.write_input(b"cd /usr/share\n");
    assert!(pump_until(&mut s, Duration::from_secs(5), |s| s.pane_session(0).cwd()
        == Some("/usr/share")));

    assert!(s.dispatch("pane.split_right"));
    settle(&mut s);
    assert!(
        pump_until(&mut s, Duration::from_secs(5), |s| s.pane_session(1).cwd().is_some()),
        "the new pane never reported a working directory"
    );
    assert_eq!(
        s.pane_session(1).cwd(),
        Some("/usr/lib"),
        "the new pane followed the focused pane instead of starting-dir"
    );

    drop(s);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn closing_a_pane_gives_the_space_back() {
    let (mut s, root) = open("close", Settings::default());
    settle(&mut s);
    let full = s.pane_session(0).dimensions();

    assert!(s.dispatch("pane.split_right"));
    settle(&mut s);
    assert!(s.dispatch("pane.close"));
    assert_eq!(s.pane_count(), 1);
    settle(&mut s);
    assert_eq!(s.pane_session(0).dimensions(), full, "the survivor did not get the space back");

    // The last pane does not close: that is the window closing.
    assert!(!s.dispatch("pane.close"));
    assert_eq!(s.pane_count(), 1);

    drop(s);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn both_panes_are_actually_drawn_and_the_divider_sits_between_them() {
    // Offscreen tests were green through every rendering bug this project has
    // had, because they never look at a pixel. This one does: text on the left
    // half, text on the right half, and a line between.
    let (mut s, root) = open("pixels", Settings::default());
    settle(&mut s);
    assert!(s.dispatch("pane.split_right"));
    settle(&mut s);

    // Fill both panes with something solid and unmistakable. `focus_new_panes`
    // is on by default, so the pane that just opened is the one taking input.
    s.write_input(b"printf 'MMMMMMMMMMMM\\n'\n");
    assert!(pump_until(&mut s, Duration::from_secs(5), |s| flat(s, 1).contains("MMMMMMMMMMMM")));
    assert!(s.dispatch("pane.focus_left"), "focus would not move back to the first pane");
    s.write_input(b"printf 'MMMMMMMMMMMM\\n'\n");
    assert!(pump_until(&mut s, Duration::from_secs(5), |s| flat(s, 0).contains("MMMMMMMMMMMM")));

    // Let the arrival animation finish, so nothing is drawn part-way.
    for _ in 0..30 {
        s.advance(Duration::from_millis(16));
        s.pump();
    }

    let target = s.renderer().context().offscreen_target(1200, 720).unwrap();
    s.render_to_texture(&target).unwrap();
    let data = read_back(&target);

    let background = pixel_at(&data, 1200, 4, 700);
    let ink = |x: usize, y: usize| {
        let p = pixel_at(&data, 1200, x, y);
        let d = |a: u8, b: u8| (a as i32 - b as i32).abs();
        d(p.0, background.0) + d(p.1, background.1) + d(p.2, background.2) > 24
    };

    let column_has_ink = |x: usize| (0..720).step_by(2).any(|y| ink(x, y));
    let left = (8..560).step_by(4).filter(|x| column_has_ink(*x)).count();
    let right = (640..1180).step_by(4).filter(|x| column_has_ink(*x)).count();
    assert!(left > 0, "the left pane drew nothing");
    assert!(right > 0, "the right pane drew nothing");

    // And the rule between them. A divider is the one thing on screen that
    // runs the full height of the window, which is what tells it apart from a
    // column that happens to be full of text.
    let coverage = |x: usize| (8..712).step_by(4).filter(|y| ink(x, *y)).count();
    let full = (8..712).step_by(4).count();
    let rule = (560..660).find(|x| coverage(*x) * 10 > full * 9);
    assert!(
        rule.is_some(),
        "no full-height rule between the panes; best column covered {}/{full}",
        (560..660).map(coverage).max().unwrap_or(0)
    );

    drop(s);
    let _ = std::fs::remove_dir_all(&root);
}
