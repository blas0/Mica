//! What each key combination does, and how the user changes it.
//!
//! Before this, the three shortcuts Mica owned were a `match` on characters
//! inside `keyDown:`. That is fine until the command palette starts printing an
//! accelerator next to every action — at which point the printed accelerator
//! and the `match` are two copies of the same fact, and they drift. Mica's
//! palette was already advertising `⌘↓` for "Scroll to Bottom" and `⌘↑` for
//! block navigation, none of which existed.
//!
//! So there is one table. The window dispatches through it, the palette prints
//! from it, and the shortcut panel edits it.
//!
//! ## Storage
//!
//! Bindings live in `settings.toml` under `[keys]`, as `action = "chord"`:
//!
//! ```toml
//! [keys]
//! "find.toggle" = "cmd+alt+f"
//! ```
//!
//! Written in the same only-what-differs style as the rest of the file, so a
//! user who has rebound one key has a one-line diff rather than a wall of
//! defaults.

use std::collections::BTreeMap;

use crate::keys::{Key, Modifiers};

/// A key plus the modifiers held with it.
///
/// The fields are private, and that is load-bearing. A chord's key is
/// **case-folded**: `charactersIgnoringModifiers` ignores every modifier
/// except Shift, so `⌘⇧P` arrives from AppKit as the character `P` while the
/// same binding written in a settings file says `p`. Case belongs to Shift,
/// which is already in the modifiers; carrying it in the character as well
/// gives one chord two spellings, and a table keyed on it misses half the
/// time. It did: the command palette stopped opening.
///
/// Folding in the constructor rather than at each lookup is the difference
/// between a rule and a habit — there is no way to build an unfolded chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Chord {
    modifiers: Modifiers,
    key: Key,
}

impl Chord {
    pub fn new(modifiers: Modifiers, key: Key) -> Chord {
        let key = match key {
            // `to_lowercase` yields a sequence; for every key on a keyboard it
            // is one character, and anything stranger is left alone rather
            // than truncated.
            Key::Char(ch) => {
                let mut lower = ch.to_lowercase();
                match (lower.next(), lower.next()) {
                    (Some(one), None) => Key::Char(one),
                    _ => Key::Char(ch),
                }
            }
            other => other,
        };
        Chord { modifiers, key }
    }

    pub fn key(&self) -> Key {
        self.key
    }

    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    /// Whether this is something a terminal should be allowed to steal.
    ///
    /// A chord with no modifiers, or with Shift alone, is a character the user
    /// is trying to type. Binding `a` to "Next Tab" would make the letter a
    /// unusable, and the panel has no business letting anyone do that by
    /// pressing the wrong key while capturing.
    pub fn is_bindable(&self) -> bool {
        if self.modifiers.command || self.modifiers.control || self.modifiers.alt {
            return true;
        }
        // Function keys and Escape are the exception: they are not text, so
        // they are bindable bare.
        matches!(self.key, Key::Function(_) | Key::Escape)
    }

    /// The form written to `settings.toml`: lowercase, `+`-separated, modifiers
    /// in a fixed order so two equal chords are the same string.
    pub fn to_config(self) -> String {
        let mut out = String::new();
        for (on, name) in [
            (self.modifiers.control, "ctrl"),
            (self.modifiers.alt, "alt"),
            (self.modifiers.shift, "shift"),
            (self.modifiers.command, "cmd"),
        ] {
            if on {
                out.push_str(name);
                out.push('+');
            }
        }
        out.push_str(&key_config_name(self.key));
        out
    }

    pub fn parse(text: &str) -> Option<Chord> {
        let mut modifiers = Modifiers::NONE;
        let mut key = None;
        for part in text.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "cmd" | "command" | "super" => modifiers.command = true,
                "ctrl" | "control" => modifiers.control = true,
                "alt" | "opt" | "option" => modifiers.alt = true,
                "shift" => modifiers.shift = true,
                name => key = Some(parse_key_name(name)?),
            }
        }
        Some(Chord::new(modifiers, key?))
    }

    /// The form shown in the palette and the shortcut panel, using the symbols
    /// every Mac menu uses. Order is Apple's: control, option, shift, command.
    pub fn to_display(self) -> String {
        let mut out = String::new();
        if self.modifiers.control {
            out.push('⌃');
        }
        if self.modifiers.alt {
            out.push('⌥');
        }
        if self.modifiers.shift {
            out.push('⇧');
        }
        if self.modifiers.command {
            out.push('⌘');
        }
        out.push_str(&key_display_name(self.key));
        out
    }
}

fn key_config_name(key: Key) -> String {
    match key {
        Key::Char(' ') => "space".into(),
        Key::Char(ch) => ch.to_lowercase().to_string(),
        Key::Enter => "enter".into(),
        Key::Tab => "tab".into(),
        Key::Backspace => "backspace".into(),
        Key::Delete => "delete".into(),
        Key::Escape => "escape".into(),
        Key::Up => "up".into(),
        Key::Down => "down".into(),
        Key::Left => "left".into(),
        Key::Right => "right".into(),
        Key::Home => "home".into(),
        Key::End => "end".into(),
        Key::PageUp => "pageup".into(),
        Key::PageDown => "pagedown".into(),
        Key::Insert => "insert".into(),
        Key::Function(n) => format!("f{n}"),
    }
}

fn key_display_name(key: Key) -> String {
    match key {
        Key::Char(ch) => ch.to_uppercase().to_string(),
        Key::Enter => "↩".into(),
        Key::Tab => "⇥".into(),
        Key::Backspace => "⌫".into(),
        Key::Delete => "⌦".into(),
        Key::Escape => "⎋".into(),
        Key::Up => "↑".into(),
        Key::Down => "↓".into(),
        Key::Left => "←".into(),
        Key::Right => "→".into(),
        Key::Home => "↖".into(),
        Key::End => "↘".into(),
        Key::PageUp => "⇞".into(),
        Key::PageDown => "⇟".into(),
        Key::Insert => "Ins".into(),
        Key::Function(n) => format!("F{n}"),
    }
}

fn parse_key_name(name: &str) -> Option<Key> {
    Some(match name {
        "enter" | "return" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "escape" | "esc" => Key::Escape,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" | "pgup" => Key::PageUp,
        "pagedown" | "pgdn" => Key::PageDown,
        "insert" | "ins" => Key::Insert,
        // Written out because a literal space cannot survive the `+`-split
        // grammar, and the settings catalogue lists it as a key.
        "space" => Key::Char(' '),
        other => {
            if let Some(digits) = other.strip_prefix('f') {
                if let Ok(n) = digits.parse::<u8>() {
                    if (1..=20).contains(&n) {
                        return Some(Key::Function(n));
                    }
                }
            }
            let mut chars = other.chars();
            let ch = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            Key::Char(ch)
        }
    })
}

/// One thing a key can be bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bindable {
    /// The action id, shared with the command palette.
    pub id: &'static str,
    pub label: &'static str,
    /// False for actions that exist as a binding and a palette entry but do
    /// nothing yet — tabs and panes, which Mica does not have.
    ///
    /// Recorded rather than omitted so the settings catalogue can say so out
    /// loud. A binding that silently does nothing is worse than no binding.
    pub implemented: bool,
}

/// Every action the shortcut panel offers, in the order it lists them.
///
/// A fixed list rather than everything the palette can dispatch: the palette
/// includes one entry per theme, and a shortcut panel with twenty-two "Theme ·
/// …" rows in it is a worse panel.
pub const BINDABLE: &[Bindable] = &[
    Bindable { id: "palette.toggle", label: "Command Palette", implemented: true },
    Bindable { id: "settings.open", label: "Settings", implemented: true },
    Bindable { id: "keys.open", label: "Keyboard Shortcuts", implemented: true },
    Bindable { id: "find.toggle", label: "Find in Scrollback", implemented: true },
    Bindable { id: "find.next", label: "Next Match", implemented: true },
    Bindable { id: "find.previous", label: "Previous Match", implemented: true },
    Bindable { id: "session.scroll_bottom", label: "Scroll to Bottom", implemented: true },
    Bindable { id: "session.scroll_top", label: "Scroll to Top", implemented: true },
    Bindable { id: "session.clear_selection", label: "Clear Selection", implemented: true },
    Bindable { id: "blocks.next", label: "Next Command Block", implemented: true },
    Bindable { id: "blocks.previous", label: "Previous Command Block", implemented: true },
    Bindable { id: "settings.fx.cursor", label: "Caret Motion · Next Style", implemented: true },
    Bindable { id: "settings.fx.decay", label: "Toggle Caret Decay", implemented: true },
    Bindable { id: "settings.fx.blink", label: "Toggle Caret Blink", implemented: true },
    Bindable { id: "settings.fx.reduce", label: "Toggle Reduce Motion", implemented: true },
    Bindable { id: "settings.fx.ambient", label: "Toggle Ambient Light", implemented: true },
    Bindable { id: "pane.split_right", label: "New Pane · Right", implemented: true },
    Bindable { id: "pane.split_left", label: "New Pane · Left", implemented: true },
    Bindable { id: "pane.split_down", label: "New Pane · Down", implemented: true },
    Bindable { id: "pane.split_up", label: "New Pane · Up", implemented: true },
    Bindable { id: "pane.focus_right", label: "Focus Pane · Right", implemented: true },
    Bindable { id: "pane.focus_left", label: "Focus Pane · Left", implemented: true },
    Bindable { id: "pane.focus_down", label: "Focus Pane · Down", implemented: true },
    Bindable { id: "pane.focus_up", label: "Focus Pane · Up", implemented: true },
    Bindable { id: "pane.close", label: "Close Pane", implemented: true },
    // Mica has one window per surface and no tab bar. These are here because
    // the bindings and the settings catalogue are the contract, and shipping
    // the contract before the feature is how the feature arrives without
    // breaking anyone's configuration.
    Bindable { id: "session.new_tab", label: "New Tab", implemented: false },
    Bindable { id: "session.next_tab", label: "Next Tab", implemented: false },
    Bindable { id: "session.previous_tab", label: "Previous Tab", implemented: false },
];

/// The chord → action table.
///
/// Keyed by chord because that is the direction the window looks it up in, on
/// every keystroke. The reverse lookup — what is this action bound to? — is for
/// drawing, and runs once per visible row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bindings {
    map: BTreeMap<Chord, String>,
}

impl Default for Bindings {
    fn default() -> Bindings {
        Bindings::defaults()
    }
}

impl Bindings {
    pub fn defaults() -> Bindings {
        let cmd = Modifiers { command: true, ..Modifiers::NONE };
        let cmd_shift = Modifiers { command: true, shift: true, ..Modifiers::NONE };
        let mut map = BTreeMap::new();
        let mut bind = |modifiers, key, id: &str| {
            map.insert(Chord::new(modifiers, key), id.to_owned());
        };
        let cmd_opt = Modifiers { command: true, alt: true, ..Modifiers::NONE };
        let cmd_opt_shift =
            Modifiers { command: true, alt: true, shift: true, ..Modifiers::NONE };
        let ctrl = Modifiers { control: true, ..Modifiers::NONE };
        let ctrl_shift = Modifiers { control: true, shift: true, ..Modifiers::NONE };

        // `F5` opens the palette and `⌘,` opens the settings file, which is
        // the split the two things actually have: one is a list of commands,
        // the other is a document. On a Mac with "use F1, F2 … as standard
        // function keys" off, that is `fn-F5`. It costs the shell its `F5`
        // (`CSI 15~`); unbind it in `[keys]` if a program needs it.
        bind(Modifiers::NONE, Key::Function(5), "palette.toggle");
        bind(cmd, Key::Char(','), "settings.open");
        bind(cmd_shift, Key::Char('k'), "keys.open");
        bind(cmd, Key::Char('f'), "find.toggle");
        bind(cmd, Key::Char('g'), "find.next");
        bind(cmd_shift, Key::Char('g'), "find.previous");
        bind(cmd, Key::Down, "session.scroll_bottom");
        bind(cmd, Key::Up, "session.scroll_top");
        // Not yet implemented, but bound: see `BINDABLE`.
        bind(cmd, Key::Char('t'), "session.new_tab");
        bind(ctrl, Key::Tab, "session.next_tab");
        bind(ctrl_shift, Key::Tab, "session.previous_tab");
        // `⌥⌘` arrows split, `⌥⌘⇧` arrows move between the panes. One
        // modifier apart, because they are the same gesture with and without
        // a new shell at the end of it.
        bind(cmd_opt, Key::Right, "pane.split_right");
        bind(cmd_opt, Key::Left, "pane.split_left");
        bind(cmd_opt, Key::Down, "pane.split_down");
        bind(cmd_opt, Key::Up, "pane.split_up");
        bind(cmd_opt_shift, Key::Right, "pane.focus_right");
        bind(cmd_opt_shift, Key::Left, "pane.focus_left");
        bind(cmd_opt_shift, Key::Down, "pane.focus_down");
        bind(cmd_opt_shift, Key::Up, "pane.focus_up");
        // `⌘W` is the window on a Mac. A pane closes the way a shell does —
        // `exit`, or `^D` — and by this, which is deliberately not `⌘W`.
        bind(cmd_shift, Key::Char('w'), "pane.close");
        // `⌘]` and `⌘[` rather than the arrows: `⌘↓` is scroll-to-bottom, and
        // the palette used to claim both for both.
        bind(cmd, Key::Char(']'), "blocks.next");
        bind(cmd, Key::Char('['), "blocks.previous");
        Bindings { map }
    }

    /// What this chord does, if anything.
    pub fn action(&self, chord: Chord) -> Option<&str> {
        self.map.get(&chord).map(String::as_str)
    }

    /// What this action is bound to, if anything.
    pub fn chord_for(&self, id: &str) -> Option<Chord> {
        self.map.iter().find(|(_, action)| *action == id).map(|(chord, _)| *chord)
    }

    /// Binds a chord, returning the action it was taken from.
    ///
    /// A chord does one thing. Rebinding to a chord that is already in use has
    /// to take it away from whatever had it — the alternative is two actions
    /// claiming one key and the window silently picking one, which is the kind
    /// of thing that is impossible to diagnose from the outside. The panel
    /// shows what was displaced.
    pub fn bind(&mut self, id: &str, chord: Chord) -> Option<String> {
        if !chord.is_bindable() {
            return None;
        }
        let displaced = self.map.insert(chord, id.to_owned());
        // An action holds one chord, so drop whatever it had before.
        let previous: Vec<Chord> = self
            .map
            .iter()
            .filter(|(c, action)| *action == id && **c != chord)
            .map(|(c, _)| *c)
            .collect();
        for c in previous {
            self.map.remove(&c);
        }
        displaced.filter(|d| d != id)
    }

    /// Unbinds an action entirely. It stays reachable from the palette.
    pub fn clear(&mut self, id: &str) {
        self.map.retain(|_, action| action != id);
    }

    /// The pairs that differ from the defaults, for `settings.toml`.
    pub fn overrides(&self) -> BTreeMap<String, String> {
        let defaults = Bindings::defaults();
        let mut out = BTreeMap::new();
        for bindable in BINDABLE {
            let mine = self.chord_for(bindable.id);
            let theirs = defaults.chord_for(bindable.id);
            if mine != theirs {
                // An unbound action is recorded as the empty string rather than
                // omitted, or reloading would restore the default and the
                // user's deliberate removal would not survive a restart.
                out.insert(
                    bindable.id.to_owned(),
                    mine.map(Chord::to_config).unwrap_or_default(),
                );
            }
        }
        out
    }

    /// Applies stored overrides over the defaults.
    ///
    /// An unparseable chord is ignored rather than rejected: a settings file
    /// written by a newer Mica should still open in an older one, and a typo in
    /// one binding must not cost the user the other eleven.
    pub fn from_overrides(overrides: &BTreeMap<String, String>) -> Bindings {
        let mut bindings = Bindings::defaults();
        for (id, chord) in overrides {
            if !BINDABLE.iter().any(|b| b.id == id) {
                continue;
            }
            if chord.is_empty() {
                bindings.clear(id);
                continue;
            }
            if let Some(chord) = Chord::parse(chord) {
                bindings.bind(id, chord);
            }
        }
        bindings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd() -> Modifiers {
        Modifiers { command: true, ..Modifiers::NONE }
    }

    #[test]
    fn a_shifted_letter_finds_its_binding() {
        // Regression test. `charactersIgnoringModifiers` ignores every
        // modifier *except* Shift, so ⌘⇧G arrives as the character `G`, not
        // `g`. The table stored `g`, the lookup asked for `G`, and every
        // shifted binding simply stopped firing.
        //
        // Case belongs to Shift, which is already in the modifiers. Carrying it
        // in the character too means the same chord has two spellings.
        let bindings = Bindings::defaults();
        let shift_cmd = Modifiers { command: true, shift: true, ..Modifiers::NONE };
        assert_eq!(
            bindings.action(Chord::new(shift_cmd, Key::Char('G'))),
            Some("find.previous"),
            "⌘⇧G as AppKit actually delivers it did not find its binding"
        );
        assert_eq!(
            bindings.action(Chord::new(shift_cmd, Key::Char('g'))),
            Some("find.previous")
        );
    }

    #[test]
    fn case_is_not_part_of_a_chord() {
        let cmd = Modifiers { command: true, ..Modifiers::NONE };
        assert_eq!(Chord::new(cmd, Key::Char('F')), Chord::new(cmd, Key::Char('f')));
        assert_eq!(Chord::new(cmd, Key::Char('F')).to_config(), "cmd+f");
        // And a chord parsed from a file agrees with one built from a keypress.
        assert_eq!(Chord::parse("cmd+F"), Some(Chord::new(cmd, Key::Char('f'))));
    }

    #[test]
    fn a_shifted_binding_is_not_the_same_as_an_unshifted_one() {
        // The other half: folding case must not collapse ⌘G and ⌘⇧G, which are
        // next-match and previous-match.
        let bindings = Bindings::defaults();
        let cmd = Modifiers { command: true, ..Modifiers::NONE };
        let cmd_shift = Modifiers { command: true, shift: true, ..Modifiers::NONE };
        assert_eq!(bindings.action(Chord::new(cmd, Key::Char('g'))), Some("find.next"));
        assert_eq!(
            bindings.action(Chord::new(cmd_shift, Key::Char('G'))),
            Some("find.previous")
        );
    }

    #[test]
    fn every_default_binding_resolves_to_a_bindable_action() {
        // A default pointing at an id nothing dispatches is a key that does
        // nothing, and the panel would list it as configured.
        let bindings = Bindings::defaults();
        for bindable in BINDABLE {
            if let Some(chord) = bindings.chord_for(bindable.id) {
                assert_eq!(bindings.action(chord), Some(bindable.id));
            }
        }
    }

    #[test]
    fn no_default_chord_is_claimed_by_two_actions() {
        // The bug this whole table exists to prevent: the palette advertised
        // `⌘↓` for both "Scroll to Bottom" and "Next Command Block".
        let bindings = Bindings::defaults();
        let mut seen = std::collections::HashSet::new();
        for bindable in BINDABLE {
            if let Some(chord) = bindings.chord_for(bindable.id) {
                assert!(
                    seen.insert(chord),
                    "{} shares {} with another action",
                    bindable.id,
                    chord.to_display()
                );
            }
        }
    }

    #[test]
    fn space_survives_the_plus_split_grammar() {
        // The catalogue lists `space` as a bindable key. A literal space
        // cannot survive a `+`-separated grammar, so it has to be written out
        // — and it has to round-trip, or the file documents a key that fails
        // to parse the moment anyone uses it.
        let chord = Chord::new(cmd(), Key::Char(' '));
        assert_eq!(chord.to_config(), "cmd+space");
        assert_eq!(Chord::parse("cmd+space"), Some(chord));
    }

    #[test]
    fn every_modifier_spelling_the_catalogue_offers_actually_parses() {
        let expected = Chord::new(
            Modifiers { alt: true, command: true, ..Modifiers::NONE },
            Key::Char('k'),
        );
        for text in ["alt+cmd+k", "opt+cmd+k", "option+command+k", "cmd+alt+k"] {
            assert_eq!(Chord::parse(text), Some(expected), "`{text}` did not parse");
        }
    }

    #[test]
    fn a_chord_round_trips_through_its_config_form() {
        let chords = [
            Chord::new(cmd(), Key::Char('f')),
            Chord::new(Modifiers { command: true, shift: true, ..Modifiers::NONE }, Key::Char('p')),
            Chord::new(Modifiers { control: true, alt: true, ..Modifiers::NONE }, Key::Up),
            Chord::new(cmd(), Key::Function(5)),
            Chord::new(Modifiers::NONE, Key::Escape),
        ];
        for chord in chords {
            let text = chord.to_config();
            assert_eq!(Chord::parse(&text), Some(chord), "{text} did not round-trip");
        }
    }

    #[test]
    fn the_config_form_is_stable_whatever_order_the_modifiers_arrive_in() {
        // Otherwise the same binding produces a different string depending on
        // which finger landed first, and settings.toml churns.
        let a = Chord::parse("cmd+shift+p").unwrap();
        let b = Chord::parse("shift+cmd+p").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.to_config(), b.to_config());
    }

    #[test]
    fn parsing_accepts_the_names_people_actually_write() {
        for text in ["cmd+f", "command+f", "super+f"] {
            assert_eq!(Chord::parse(text), Some(Chord::new(cmd(), Key::Char('f'))), "{text}");
        }
        for text in ["alt+up", "opt+up", "option+up"] {
            let expected =
                Chord::new(Modifiers { alt: true, ..Modifiers::NONE }, Key::Up);
            assert_eq!(Chord::parse(text), Some(expected), "{text}");
        }
    }

    #[test]
    fn nonsense_does_not_parse_into_a_binding() {
        for text in ["", "cmd+", "cmd+notakey", "f99", "cmd+shift"] {
            assert_eq!(Chord::parse(text), None, "{text:?} parsed into something");
        }
    }

    #[test]
    fn the_display_form_uses_the_symbols_a_mac_menu_uses() {
        let chord =
            Chord::new(Modifiers { command: true, shift: true, ..Modifiers::NONE }, Key::Char('p'));
        assert_eq!(chord.to_display(), "⇧⌘P");
        assert_eq!(Chord::new(cmd(), Key::Down).to_display(), "⌘↓");
    }

    #[test]
    fn a_bare_letter_is_not_bindable() {
        // Otherwise capturing a shortcut while the user fumbles takes the
        // letter A away from them permanently.
        assert!(!Chord::new(Modifiers::NONE, Key::Char('a')).is_bindable());
        assert!(!Chord::new(Modifiers { shift: true, ..Modifiers::NONE }, Key::Char('a'))
            .is_bindable());
        assert!(Chord::new(cmd(), Key::Char('a')).is_bindable());
        assert!(Chord::new(Modifiers { control: true, ..Modifiers::NONE }, Key::Char('a'))
            .is_bindable());
        // Not text, so bindable bare.
        assert!(Chord::new(Modifiers::NONE, Key::Function(3)).is_bindable());
        assert!(Chord::new(Modifiers::NONE, Key::Escape).is_bindable());
    }

    #[test]
    fn binding_an_unbindable_chord_changes_nothing() {
        let mut bindings = Bindings::defaults();
        let before = bindings.clone();
        assert_eq!(bindings.bind("find.toggle", Chord::new(Modifiers::NONE, Key::Char('q'))), None);
        assert_eq!(bindings, before, "a bare letter was accepted");
    }

    #[test]
    fn rebinding_takes_the_chord_away_from_whoever_had_it() {
        // Two actions on one key means the window picks one and the user
        // cannot tell which.
        let mut bindings = Bindings::defaults();
        let find = Chord::new(cmd(), Key::Char('f'));
        let displaced = bindings.bind("blocks.next", find);

        assert_eq!(displaced.as_deref(), Some("find.toggle"));
        assert_eq!(bindings.action(find), Some("blocks.next"));
        assert_eq!(bindings.chord_for("find.toggle"), None, "find kept a chord it lost");
    }

    #[test]
    fn an_action_holds_one_chord_at_a_time() {
        let mut bindings = Bindings::defaults();
        bindings.bind("find.toggle", Chord::new(cmd(), Key::Char('j')));
        assert_eq!(bindings.action(Chord::new(cmd(), Key::Char('f'))), None, "the old chord stuck");
        assert_eq!(bindings.chord_for("find.toggle"), Some(Chord::new(cmd(), Key::Char('j'))));
    }

    #[test]
    fn rebinding_an_action_to_the_chord_it_already_has_displaces_nothing() {
        let mut bindings = Bindings::defaults();
        let find = Chord::new(cmd(), Key::Char('f'));
        assert_eq!(bindings.bind("find.toggle", find), None);
        assert_eq!(bindings.action(find), Some("find.toggle"));
    }

    #[test]
    fn defaults_write_no_overrides_at_all() {
        assert!(
            Bindings::defaults().overrides().is_empty(),
            "an untouched keymap would be written to settings.toml"
        );
    }

    #[test]
    fn only_what_changed_is_written_and_it_comes_back() {
        let mut bindings = Bindings::defaults();
        bindings.bind("find.toggle", Chord::new(cmd(), Key::Char('j')));

        let overrides = bindings.overrides();
        assert_eq!(overrides.len(), 1, "{overrides:?}");
        assert_eq!(overrides.get("find.toggle").map(String::as_str), Some("cmd+j"));
        assert_eq!(Bindings::from_overrides(&overrides), bindings);
    }

    #[test]
    fn an_unbound_action_survives_a_restart() {
        // Recorded as an empty string, not omitted: omitting it would restore
        // the default and quietly undo what the user did.
        let mut bindings = Bindings::defaults();
        bindings.clear("find.toggle");
        let overrides = bindings.overrides();
        assert_eq!(overrides.get("find.toggle").map(String::as_str), Some(""));
        assert_eq!(Bindings::from_overrides(&overrides).chord_for("find.toggle"), None);
    }

    #[test]
    fn a_broken_entry_costs_only_itself() {
        let mut overrides = BTreeMap::new();
        overrides.insert("find.toggle".to_owned(), "cmd+j".to_owned());
        overrides.insert("palette.toggle".to_owned(), "not a chord".to_owned());
        overrides.insert("nonexistent.action".to_owned(), "cmd+k".to_owned());

        let bindings = Bindings::from_overrides(&overrides);
        assert_eq!(bindings.chord_for("find.toggle"), Some(Chord::new(cmd(), Key::Char('j'))));
        assert_eq!(
            bindings.chord_for("palette.toggle"),
            Bindings::defaults().chord_for("palette.toggle"),
            "a typo in one binding cost the user another"
        );
        assert_eq!(bindings.action(Chord::new(cmd(), Key::Char('k'))), None);
    }
}
