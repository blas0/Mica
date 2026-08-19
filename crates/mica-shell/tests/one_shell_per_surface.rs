//! A surface spawns **one** shell.
//!
//! This is a regression test for a real bug. The wakeup callback has to be
//! installed before the PTY reader thread starts, and an earlier version got
//! that ordering by *replacing* the session immediately after opening it —
//! which spawned the user's login shell twice, ran their rc files twice, and
//! left the first shell to be hung up a moment later. It was invisible in every
//! other test, because a discarded shell still leaves a working terminal
//! behind. It was obvious the first time anyone ran `ps`.
//!
//! Deliberately its own test binary: it counts child processes, so it must be
//! the only thing in the process spawning them. The unit tests in `surface.rs`
//! open surfaces on several threads at once and would race it.
//!
//! The tests here also serialise against *each other* through [`ONE_AT_A_TIME`]
//! rather than relying on `--test-threads=1`, because a test that only passes
//! under a flag someone has to remember is a test that will start failing the
//! first time it is run the normal way.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use mica_core::settings::Settings;
use mica_shell::surface::Surface;

/// Only one test may spawn or count children at a time.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// A poisoned lock means another test panicked; the counting is still valid, so
/// recover rather than cascading one failure into three.
fn serialised() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner())
}

/// Direct children of this process, excluding the `ps` used to look.
///
/// `ps` is itself a child, and it lists itself — which cost a confused minute
/// the first time this test ran and reported one shell too many.
fn children() -> Vec<i32> {
    let me = std::process::id();
    let output = std::process::Command::new("ps")
        .args(["-eo", "pid=,ppid=,stat=,comm="])
        .output()
        .expect("ps should exist on macOS");

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid: i32 = parts.next()?.parse().ok()?;
            let ppid: u32 = parts.next()?.parse().ok()?;
            let _stat = parts.next()?;
            let comm = parts.next()?;
            // Zombies are deliberately still counted: the reaping test wants
            // to watch the child disappear entirely, not merely stop running.
            (ppid == me && !comm.ends_with("ps")).then_some(pid)
        })
        .collect()
}

/// Waits until `predicate` holds, or gives up. Not a sleep: process teardown is
/// asynchronous, and polling is the only honest way to observe it.
fn wait_until(mut predicate: impl FnMut() -> bool, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    predicate()
}

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("mica-one-shell-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn opening_a_surface_spawns_exactly_one_shell() {
    let _guard = serialised();
    let before = children();

    let root = temp_root("open");
    let surface = Surface::open(Settings::default(), (800, 480), 2.0, root.clone(), None)
        .expect("a surface should open on this machine");

    let spawned: Vec<i32> =
        children().into_iter().filter(|pid| !before.contains(pid)).collect();

    assert_eq!(
        spawned.len(),
        1,
        "a surface spawned {} shells: {spawned:?}. One of them is orphaned, and \
         the user's rc files just ran {} times.",
        spawned.len(),
        spawned.len()
    );

    drop(surface);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn dropping_a_surface_reaps_its_shell() {
    let _guard = serialised();
    // The other half of the same concern: a shell that outlives its window is
    // a process nobody can see and nobody can kill from the UI.
    let before = children();

    let root = temp_root("drop");
    let surface = Surface::open(Settings::default(), (800, 480), 2.0, root.clone(), None)
        .expect("a surface should open");

    let spawned: Vec<i32> =
        children().into_iter().filter(|pid| !before.contains(pid)).collect();
    assert_eq!(spawned.len(), 1);
    let shell = spawned[0];

    drop(surface);

    assert!(
        wait_until(|| !children().contains(&shell), Duration::from_secs(10)),
        "shell {shell} survived its surface being dropped"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn two_surfaces_get_one_shell_each() {
    let _guard = serialised();
    let before = children();

    let (root_a, root_b) = (temp_root("a"), temp_root("b"));
    let a = Surface::open(Settings::default(), (800, 480), 2.0, root_a.clone(), None)
        .expect("first surface");
    let b = Surface::open(Settings::default(), (800, 480), 2.0, root_b.clone(), None)
        .expect("second surface");

    let spawned: Vec<i32> =
        children().into_iter().filter(|pid| !before.contains(pid)).collect();
    assert_eq!(spawned.len(), 2, "two surfaces produced {} shells: {spawned:?}", spawned.len());

    drop(a);
    drop(b);
    let _ = std::fs::remove_dir_all(&root_a);
    let _ = std::fs::remove_dir_all(&root_b);
}
