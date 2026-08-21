//! `implemented: true` is a claim, and this is what checks it.
//!
//! An action can be answered in one of two places: `Surface::dispatch`, which
//! is the terminal, or `MicaView::window_action`, which is the window. The
//! failure this prevents is a palette entry that looks live, is bound to a
//! chord, appears in the settings catalogue without the "not implemented yet"
//! note — and does nothing when you pick it.
//!
//! It reads the source rather than calling `dispatch`, deliberately. Half
//! these actions have side effects: one of them opens a text editor.

use mica_shell::bindings::BINDABLE;
use mica_shell::view::WINDOW_ACTIONS;

fn source(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is not readable: {e}", path.display()))
}

#[test]
fn every_implemented_action_is_answered_somewhere() {
    let surface = source("surface.rs");
    let view = source("view.rs");

    for bindable in BINDABLE.iter().filter(|b| b.implemented) {
        let quoted = format!("\"{}\"", bindable.id);
        let answered = surface.contains(&quoted) || view.contains(&quoted);
        assert!(
            answered,
            "`{}` says it is implemented, but neither the surface nor the window \
             mentions it",
            bindable.id
        );
    }
}

#[test]
fn the_window_actions_are_all_bindable_actions() {
    // The other direction: a window action nobody can reach is dead code
    // pretending to be a feature.
    for id in WINDOW_ACTIONS {
        assert!(
            BINDABLE.iter().any(|b| b.id == id),
            "`{id}` is handled by the window but is not a bindable action"
        );
    }
}

#[test]
fn nothing_claims_to_be_unimplemented_while_being_dispatched() {
    // The inverse mistake: an action marked `implemented: false` that the
    // surface quietly handles anyway. The settings file would tell the user it
    // does nothing while it worked, which is the same drift in the other
    // direction.
    let surface = source("surface.rs");
    for bindable in BINDABLE.iter().filter(|b| !b.implemented) {
        let arm = format!("\"{}\" =>", bindable.id);
        assert!(
            !surface.contains(&arm),
            "`{}` is marked unimplemented but the surface has a match arm for it",
            bindable.id
        );
    }
}
