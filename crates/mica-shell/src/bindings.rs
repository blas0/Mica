//! What each key combination does, and how the user changes it.
//!
//! Before this, the shortcuts Mica owned were a `match` on characters inside
//! `keyDown:`, and every place that *wrote a shortcut down* was a second copy
//! of the same fact. They drifted: the app advertised `⌘↓` for two different
//! actions and `⌘↑` for one that had no binding at all.
//!
//! So there is one table. The window dispatches through it, the settings file
//! is generated from it, and `[keys]` is the only thing that edits it.
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
//!
//! One action id is not in the table at all. `text:…` types the rest of its
//! own name into the shell, so the id carries the payload:
//!
//! ```toml
//! [keys]
//! "text:clear && ls -la\n" = "cmd+shift+l"
//! ```
//!
//! See [`text_payload`].

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
/// time. It did: shifted bindings simply stopped firing.
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

    /// Reads the `settings.toml` spelling back.
    ///
    /// A chord is one key and its modifiers, so a spelling that names two keys
    /// — `cmd+,+r`, the natural way to write "⌘, then R" — is **refused**
    /// rather than resolved. It used to let the second key win, which turned
    /// that line into `⌘R`: a chord the user never asked for, bound to an
    /// action they did, with nothing said. A file that quietly means something
    /// else is worse than a file that is rejected.
    pub fn parse(text: &str) -> Option<Chord> {
        let mut modifiers = Modifiers::NONE;
        let mut key = None;
        for part in text.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "cmd" | "command" | "super" => modifiers.command = true,
                "ctrl" | "control" => modifiers.control = true,
                "alt" | "opt" | "option" => modifiers.alt = true,
                "shift" => modifiers.shift = true,
                name if key.is_none() => key = Some(parse_key_name(name)?),
                _ => return None,
            }
        }
        Some(Chord::new(modifiers, key?))
    }
}

/// The bytes a `text:` binding sends, if that is what this action is.
///
/// The action id *carries* the payload rather than pointing at it:
///
/// ```toml
/// [keys]
/// "text:clear && ls -la\n" = "cmd+shift+l"
/// ```
///
/// One map from chord to action, one conflict check, no second table to keep
/// in step. The escape rules are TOML's own — `\n`, `\t`, `\r`, `\\`, `\"`,
/// `\uXXXX` in a basic string, nothing at all in a literal one — because a
/// second escape dialect on top of the file format's is a thing to get wrong
/// twice.
///
/// An empty payload is not a binding, so `"text:"` is refused rather than
/// wiring a key to nothing.
pub fn text_payload(id: &str) -> Option<&str> {
    let text = id.strip_prefix("text:")?;
    (!text.is_empty()).then_some(text)
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
    /// The action id, as `[keys]` spells it.
    pub id: &'static str,
    pub label: &'static str,
    /// False for actions that exist as a binding but do
    /// nothing yet — tabs and panes, which Mica does not have.
    ///
    /// Recorded rather than omitted so the settings catalogue can say so out
    /// loud. A binding that silently does nothing is worse than no binding.
    pub implemented: bool,
}

/// Every action a chord can be bound to, in the order the catalogue lists them.
///
/// Themes are deliberately not in it. There are twenty-two, they are chosen by
/// name in `[appearance]`, and twenty-two more bindable actions would bury the
/// dozen that are about what the terminal *does*.
pub const BINDABLE: &[Bindable] = &[
    Bindable { id: "settings.open", label: "Settings", implemented: true },
    Bindable { id: "settings.reload", label: "Reload Settings", implemented: true },
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
    // Tabs are AppKit's own: each one is a real window with its own shell.
    // The window layer answers these, not the surface — see
    // `MicaView::window_action`.
    Bindable { id: "session.new_tab", label: "New Tab", implemented: true },
    Bindable { id: "session.next_tab", label: "Next Tab", implemented: true },
    Bindable { id: "session.previous_tab", label: "Previous Tab", implemented: true },
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

        // `⌘,` is Settings on a Mac, and Mica's settings are a file — so it
        // opens the file. There is no second surface to keep in step with it.
        bind(cmd, Key::Char(','), "settings.open");
        // One modifier apart from the key that opens the file, because they
        // are the two halves of one loop: edit it, then apply it. `⌘,` then
        // `R` would need a prefix mode, and nothing else in Mica wants one.
        bind(cmd_shift, Key::Char(','), "settings.reload");
        bind(cmd, Key::Char('f'), "find.toggle");
        bind(cmd, Key::Char('g'), "find.next");
        bind(cmd_shift, Key::Char('g'), "find.previous");
        bind(cmd, Key::Down, "session.scroll_bottom");
        bind(cmd, Key::Up, "session.scroll_top");
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
        // the app used to claim both for both.
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

    /// Unbinds an action entirely. Nothing else reaches it.
    pub fn clear(&mut self, id: &str) {
        self.map.retain(|_, action| action != id);
    }

    /// The pairs that differ from the defaults, for `settings.toml`.
    pub fn overrides(&self) -> BTreeMap<String, String> {
        let defaults = Bindings::defaults();
        let mut out = BTreeMap::new();
        // Text bindings are not in `BINDABLE` — there is no fixed catalogue of
        // them — so they are collected from the map itself. Without this a save
        // from inside the app would quietly delete every one of them.
        for (chord, action) in &self.map {
            if text_payload(action).is_some() {
                out.insert(action.clone(), chord.to_config());
            }
        }
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
            if !BINDABLE.iter().any(|b| b.id == id) && text_payload(id).is_none() {
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
        // The bug this whole table exists to prevent: the app advertised
        // `⌘↓` for both "Scroll to Bottom" and "Next Command Block".
        let bindings = Bindings::defaults();
        let mut seen = std::collections::HashSet::new();
        for bindable in BINDABLE {
            if let Some(chord) = bindings.chord_for(bindable.id) {
                assert!(
                    seen.insert(chord),
                    "{} shares {} with another action",
                    bindable.id,
                    chord.to_config()
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
    fn the_config_form_is_one_spelling_per_chord() {
        // Modifier order in the file is the user's; the form Mica writes back
        // is fixed, so two equal chords are the same string and the table can
        // be keyed on it.
        let chord =
            Chord::new(Modifiers { command: true, shift: true, ..Modifiers::NONE }, Key::Char('p'));
        assert_eq!(chord.to_config(), "shift+cmd+p");
        assert_eq!(Chord::parse("cmd+shift+p"), Some(chord));
        assert_eq!(Chord::parse("shift+cmd+p"), Some(chord));
        assert_eq!(Chord::new(cmd(), Key::Down).to_config(), "cmd+down");
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
        overrides.insert("find.next".to_owned(), "not a chord".to_owned());
        overrides.insert("nonexistent.action".to_owned(), "cmd+k".to_owned());

        let bindings = Bindings::from_overrides(&overrides);
        assert_eq!(bindings.chord_for("find.toggle"), Some(Chord::new(cmd(), Key::Char('j'))));
        assert_eq!(
            bindings.chord_for("find.next"),
            Bindings::defaults().chord_for("find.next"),
            "a typo in one binding cost the user another"
        );
        assert_eq!(bindings.action(Chord::new(cmd(), Key::Char('k'))), None);
    }

    #[test]
    fn a_chord_with_two_keys_is_refused_rather_than_guessed() {
        // `cmd+,+r` is the natural way to write "⌘, then R", and Mica has no
        // prefix chords. It used to let the second key win and hand back ⌘R —
        // a working binding for a chord nobody asked for.
        assert_eq!(Chord::parse("cmd+,+r"), None);
        assert_eq!(Chord::parse("cmd+a+b"), None);
        // The ordinary spellings are untouched, in every modifier order.
        assert_eq!(
            Chord::parse("cmd+shift+p"),
            Some(Chord::new(
                Modifiers { command: true, shift: true, ..Modifiers::NONE },
                Key::Char('p')
            ))
        );
        assert_eq!(
            Chord::parse("shift+cmd+p"),
            Chord::parse("cmd+shift+p"),
            "modifier order is not supposed to matter"
        );
        assert!(Chord::parse("cmd+,").is_some());
    }

    #[test]
    fn the_reload_action_is_bindable_and_bound() {
        assert!(BINDABLE.iter().any(|b| b.id == "settings.reload" && b.implemented));
        let bindings = Bindings::defaults();
        assert_eq!(
            bindings.chord_for("settings.reload"),
            Chord::parse("cmd+shift+,"),
            "reload should sit one modifier from the ⌘, that opens the file"
        );
        // And it must not have taken the opener's key while doing so.
        assert_eq!(bindings.chord_for("settings.open"), Chord::parse("cmd+,"));
    }

    #[test]
    fn a_text_binding_carries_its_payload() {
        assert_eq!(text_payload("text:clear\n"), Some("clear\n"));
        assert_eq!(text_payload("text:\u{1b}[A"), Some("\u{1b}[A"));
        assert_eq!(text_payload("find.toggle"), None);
        assert_eq!(text_payload("textual.thing"), None);
    }

    #[test]
    fn an_empty_text_binding_is_refused() {
        assert_eq!(text_payload("text:"), None);

        // And it does not reach the table, so the key it names stays the
        // shell's rather than being bound to sending nothing.
        let mut overrides = BTreeMap::new();
        overrides.insert("text:".to_owned(), "cmd+shift+y".to_owned());
        let bindings = Bindings::from_overrides(&overrides);
        assert_eq!(bindings.action(Chord::parse("cmd+shift+y").unwrap()), None);
    }

    #[test]
    fn a_text_binding_survives_a_round_trip_through_the_overrides() {
        // `overrides()` walks BINDABLE, which a text binding is deliberately
        // not in. Without the extra pass it collects them by, saving from the
        // inside the app would silently delete every one.
        let mut overrides = BTreeMap::new();
        overrides.insert("text:clear && ls -la\n".to_owned(), "cmd+shift+l".to_owned());
        let bindings = Bindings::from_overrides(&overrides);
        let chord = Chord::parse("cmd+shift+l").unwrap();
        assert_eq!(bindings.action(chord), Some("text:clear && ls -la\n"));

        let written = bindings.overrides();
        // Compared as a chord, not as a string: `to_config` writes modifiers in
        // its own fixed order (ctrl, alt, shift, cmd), which is the point of
        // having one.
        assert_eq!(
            written.get("text:clear && ls -la\n").map(String::as_str).and_then(Chord::parse),
            Some(chord),
            "a text binding did not survive being written back out"
        );
        assert_eq!(Bindings::from_overrides(&written).action(chord), bindings.action(chord));
    }

    #[test]
    fn a_text_binding_is_never_a_catalogue_entry() {
        // BINDABLE is the fixed set of things Mica implements. A text binding
        // is an unbounded string the user wrote, so it must never appear
        // there — the settings writer collects those separately, and the
        // commented catalogue explains the syntax without printing a payload.
        assert!(BINDABLE.iter().all(|b| text_payload(b.id).is_none()));
    }
}
