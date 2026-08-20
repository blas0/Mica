//! The keyboard shortcut editor: `⌘,`.
//!
//! Arrow keys move, space starts capturing, the next combination becomes the
//! binding, backspace unbinds, escape backs out. Deliberately modal: while
//! capturing, *every* key belongs to the panel, including the ones that would
//! otherwise close it — a user rebinding an action to `⎋` has to be able to
//! press `⎋`.
//!
//! The state machine is here rather than in the view because it is the part
//! worth testing, and it needs no window to run.

use crate::bindings::{Bindings, Chord, BINDABLE};

/// What the panel did with a key, so the caller knows whether to redraw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handled {
    /// The panel used the key and nothing changed on screen.
    Ignored,
    /// The panel used the key and the panel changed.
    Redraw,
    /// The panel used the key and a binding changed — persist it.
    Changed,
    /// The panel closed.
    Closed,
}

#[derive(Debug, Clone, Default)]
pub struct ShortcutPanel {
    open: bool,
    selected: usize,
    capturing: bool,
    notice: Option<String>,
}

impl ShortcutPanel {
    pub fn new() -> ShortcutPanel {
        ShortcutPanel::default()
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn is_capturing(&self) -> bool {
        self.capturing
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub fn open(&mut self) {
        self.open = true;
        self.capturing = false;
        self.notice = None;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.capturing = false;
        self.notice = None;
    }

    pub fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open();
        }
    }

    /// The action currently highlighted.
    pub fn selected_id(&self) -> Option<&'static str> {
        BINDABLE.get(self.selected).map(|b| b.id)
    }

    /// Moves the highlight. Saturating rather than wrapping: a list you can
    /// scroll off the end of and reappear at the top of is disorienting when
    /// it is ten rows of a twelve-row list.
    pub fn move_selection(&mut self, delta: i32) -> Handled {
        if self.capturing {
            return Handled::Ignored;
        }
        let last = BINDABLE.len().saturating_sub(1);
        let next = (self.selected as i32 + delta).clamp(0, last as i32) as usize;
        if next == self.selected {
            return Handled::Ignored;
        }
        self.selected = next;
        self.notice = None;
        Handled::Redraw
    }

    /// Space: start waiting for a combination.
    pub fn begin_capture(&mut self) -> Handled {
        if self.capturing {
            return Handled::Ignored;
        }
        self.capturing = true;
        self.notice = None;
        Handled::Redraw
    }

    pub fn cancel_capture(&mut self) -> Handled {
        if !self.capturing {
            return Handled::Ignored;
        }
        self.capturing = false;
        self.notice = None;
        Handled::Redraw
    }

    /// A chord arrived while capturing.
    ///
    /// Returns `Changed` when the table moved. A chord that is not bindable —
    /// a bare letter — is refused *with a reason*: the user pressed something,
    /// and a panel that swallows the keystroke and looks identical afterwards
    /// is one nobody trusts.
    pub fn capture(&mut self, chord: Chord, bindings: &mut Bindings) -> Handled {
        if !self.capturing {
            return Handled::Ignored;
        }
        let Some(id) = self.selected_id() else { return Handled::Ignored };

        if !chord.is_bindable() {
            self.notice = Some(format!(
                "{} needs ⌘, ⌃ or ⌥ — otherwise it takes the key away from typing",
                chord.to_display()
            ));
            return Handled::Redraw;
        }

        let displaced = bindings.bind(id, chord);
        self.capturing = false;
        self.notice = displaced.map(|other| {
            let label = BINDABLE
                .iter()
                .find(|b| b.id == other)
                .map(|b| b.label)
                .unwrap_or("another action");
            // Said out loud, because the alternative is a shortcut that
            // silently stopped working somewhere else in the app.
            format!("{} taken from {label}", chord.to_display())
        });
        Handled::Changed
    }

    /// Backspace: unbind the selected action.
    pub fn unbind(&mut self, bindings: &mut Bindings) -> Handled {
        if self.capturing {
            return Handled::Ignored;
        }
        let Some(id) = self.selected_id() else { return Handled::Ignored };
        if bindings.chord_for(id).is_none() {
            return Handled::Ignored;
        }
        bindings.clear(id);
        self.notice = Some("unbound — still available from the palette".to_owned());
        Handled::Changed
    }

    /// Escape: out of capture first, out of the panel second.
    pub fn escape(&mut self) -> Handled {
        if self.capturing {
            return self.cancel_capture();
        }
        self.close();
        Handled::Closed
    }

    /// The rows to draw, paired with their current bindings.
    pub fn rows(&self, bindings: &Bindings) -> Vec<(&'static str, String)> {
        BINDABLE
            .iter()
            .map(|b| {
                let chord = bindings.chord_for(b.id).map(Chord::to_display).unwrap_or_default();
                (b.label, chord)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{Key, Modifiers};

    fn cmd() -> Modifiers {
        Modifiers { command: true, ..Modifiers::NONE }
    }

    fn open() -> (ShortcutPanel, Bindings) {
        let mut panel = ShortcutPanel::new();
        panel.open();
        (panel, Bindings::defaults())
    }

    #[test]
    fn the_panel_starts_closed_and_toggles() {
        let mut panel = ShortcutPanel::new();
        assert!(!panel.is_open());
        panel.toggle();
        assert!(panel.is_open());
        panel.toggle();
        assert!(!panel.is_open());
    }

    #[test]
    fn arrow_keys_move_the_highlight_and_stop_at_the_ends() {
        let (mut panel, _) = open();
        assert_eq!(panel.selected(), 0);
        assert_eq!(panel.move_selection(1), Handled::Redraw);
        assert_eq!(panel.selected(), 1);

        // Off the top: nothing happens, rather than wrapping to the bottom.
        panel.move_selection(-5);
        assert_eq!(panel.selected(), 0);
        assert_eq!(panel.move_selection(-1), Handled::Ignored);

        panel.move_selection(1000);
        assert_eq!(panel.selected(), BINDABLE.len() - 1);
        assert_eq!(panel.move_selection(1), Handled::Ignored);
    }

    #[test]
    fn space_starts_capturing_and_the_next_combination_binds() {
        let (mut panel, mut bindings) = open();
        panel.move_selection(1); // Find in Scrollback
        assert_eq!(panel.selected_id(), Some("find.toggle"));

        assert_eq!(panel.begin_capture(), Handled::Redraw);
        assert!(panel.is_capturing());

        let chord = Chord::new(cmd(), Key::Char('j'));
        assert_eq!(panel.capture(chord, &mut bindings), Handled::Changed);
        assert!(!panel.is_capturing(), "the panel stayed in capture mode");
        assert_eq!(bindings.chord_for("find.toggle"), Some(chord));
    }

    #[test]
    fn the_arrow_keys_do_not_move_the_highlight_while_capturing() {
        // Otherwise binding an action to ⌘↓ moves the selection instead, and
        // the arrow keys become unbindable.
        let (mut panel, _) = open();
        panel.begin_capture();
        assert_eq!(panel.move_selection(1), Handled::Ignored);
        assert_eq!(panel.selected(), 0);
    }

    #[test]
    fn a_bare_letter_is_refused_with_a_reason_rather_than_swallowed() {
        let (mut panel, mut bindings) = open();
        let before = bindings.clone();
        panel.begin_capture();

        let handled = panel.capture(Chord::new(Modifiers::NONE, Key::Char('a')), &mut bindings);
        assert_eq!(handled, Handled::Redraw);
        assert_eq!(bindings, before, "a bare letter was bound");
        assert!(panel.is_capturing(), "capture ended on a refused chord");
        assert!(panel.notice().is_some(), "the panel refused silently");
    }

    #[test]
    fn taking_a_chord_from_another_action_says_which_one() {
        // The user has to be told, or a shortcut elsewhere in the app silently
        // stops working.
        let (mut panel, mut bindings) = open();
        panel.move_selection(2); // Next Match
        panel.begin_capture();

        let find = Chord::new(cmd(), Key::Char('f'));
        assert_eq!(panel.capture(find, &mut bindings), Handled::Changed);

        let notice = panel.notice().expect("no notice for a displaced binding");
        assert!(notice.contains("Find in Scrollback"), "notice was {notice:?}");
        assert_eq!(bindings.chord_for("find.toggle"), None);
    }

    #[test]
    fn backspace_unbinds_and_the_action_stays_in_the_list() {
        let (mut panel, mut bindings) = open();
        assert_eq!(panel.selected_id(), Some("palette.toggle"));
        assert_eq!(panel.unbind(&mut bindings), Handled::Changed);
        assert_eq!(bindings.chord_for("palette.toggle"), None);

        assert_eq!(
            panel.rows(&bindings).len(),
            BINDABLE.len(),
            "unbinding removed the row as well as the binding"
        );
        assert_eq!(panel.rows(&bindings)[0].1, "", "the row still shows a chord");

        // Nothing left to unbind.
        assert_eq!(panel.unbind(&mut bindings), Handled::Ignored);
    }

    #[test]
    fn escape_leaves_capture_before_it_leaves_the_panel() {
        let (mut panel, _) = open();
        panel.begin_capture();
        assert_eq!(panel.escape(), Handled::Redraw);
        assert!(panel.is_open(), "escape closed the panel instead of cancelling the capture");
        assert!(!panel.is_capturing());

        assert_eq!(panel.escape(), Handled::Closed);
        assert!(!panel.is_open());
    }

    #[test]
    fn escape_can_itself_be_bound() {
        // The reason capture is modal. A panel that treats ⎋ as "cancel" even
        // while capturing cannot bind ⎋ to anything.
        let (mut panel, mut bindings) = open();
        panel.begin_capture();
        let esc = Chord::new(Modifiers::NONE, Key::Escape);
        assert_eq!(panel.capture(esc, &mut bindings), Handled::Changed);
        assert_eq!(bindings.chord_for("palette.toggle"), Some(esc));
    }

    #[test]
    fn every_row_has_a_label_and_the_bound_ones_show_a_chord() {
        let (panel, bindings) = open();
        let rows = panel.rows(&bindings);
        assert_eq!(rows.len(), BINDABLE.len());
        assert!(rows.iter().all(|(label, _)| !label.is_empty()));
        assert!(
            rows.iter().any(|(_, chord)| !chord.is_empty()),
            "no row showed a binding at all"
        );
        assert_eq!(rows[0].1, "⇧⌘P");
    }

    #[test]
    fn closing_the_panel_drops_a_half_finished_capture() {
        // Reopening into "press a combination…" for a row the user has
        // forgotten about is a keystroke stolen from the terminal.
        let (mut panel, _) = open();
        panel.begin_capture();
        panel.close();
        panel.open();
        assert!(!panel.is_capturing());
        assert!(panel.notice().is_none());
    }
}
