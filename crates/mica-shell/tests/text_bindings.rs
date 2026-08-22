//! A `text:` binding, end to end: settings file to chord to shell.
//!
//! The unit tests cover the parsing and the round trip. This is the half that
//! cannot be faked — that a chord read out of a settings file puts real bytes
//! in front of a real shell, and that the shell runs them.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use mica_core::settings::Settings;
use mica_shell::bindings::Chord;
use mica_shell::keys::{Key, Modifiers};
use mica_shell::surface::Surface;

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

/// The focused pane's screen, rows trimmed and run together so wrapped text
/// rejoins.
fn flat(s: &mut Surface) -> String {
    s.pane_session(0).damage_all();
    let rows: Vec<String> = s
        .pane_session(0)
        .dirty_rows()
        .map(|row| row.cells.iter().map(|c| c.content.as_scalar().unwrap_or(' ')).collect())
        .collect();
    rows.iter().map(|r| r.trim_end()).collect::<Vec<_>>().concat()
}

#[test]
fn a_text_binding_reaches_the_shell() {
    let root = std::env::temp_dir().join(format!("mica-textbind-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);

    // Exactly what a user would write in `[keys]`, newline included: the
    // binding is only useful if it can press Return for them.
    let mut keys = BTreeMap::new();
    keys.insert("text:echo text-binding-probe\n".to_owned(), "cmd+shift+y".to_owned());
    let settings = Settings { keys, ..Settings::default() };

    let mut s = Surface::open(settings, (1200, 720), 2.0, root.clone(), None)
        .expect("a surface should open on this machine");
    s.set_settings_path(root.join("settings.toml"));
    pump_until(&mut s, Duration::from_secs(5), |s| {
        let screen = flat(s);
        screen.contains('%') || screen.contains('$')
    });

    // Through the table, the way a keystroke arrives: chord in, action out.
    let chord = Chord::new(
        Modifiers { command: true, shift: true, ..Modifiers::NONE },
        Key::Char('y'),
    );
    let action = s
        .bindings()
        .action(chord)
        .expect("the settings file bound ⌘⇧Y and the table did not learn it")
        .to_owned();
    assert_eq!(action, "text:echo text-binding-probe\n");
    assert!(s.dispatch(&action), "the binding was recognised and then not dispatched");

    assert!(
        pump_until(&mut s, Duration::from_secs(5), |s| flat(s)
            .contains("text-binding-probe")),
        "the shell never saw the text:\n{}",
        flat(&mut s)
    );

    // Not merely echoed back as the command line: the newline in the payload
    // ran it, so the word appears twice — once as typed, once as output.
    assert!(
        pump_until(&mut s, Duration::from_secs(5), |s| {
            flat(s).matches("text-binding-probe").count() >= 2
        }),
        "the payload was typed but never entered:\n{}",
        flat(&mut s)
    );

    drop(s);
    let _ = std::fs::remove_dir_all(&root);
}
