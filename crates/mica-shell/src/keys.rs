//! Turning key events into bytes.
//!
//! The encoding policy is fixed **once**, here, deliberately, because a
//! terminal that decides it piecemeal ends up with a Home key that works in
//! `vim` and not in `htop`. Two decisions in particular:
//!
//! - **Backspace sends `\x7f` (DEL), not `^H`.** This matches the `kbs=\177`
//!   claimed in Mica's terminfo entry. Claiming one and sending the other is
//!   the single most common way a terminal ends up with a backspace key that
//!   deletes forwards in some programs.
//! - **Alt sends an ESC prefix, not a high-bit byte.** Meta-mode (`smm`/`rmm`)
//!   is cancelled in the terminfo entry precisely so nothing expects the
//!   alternative. On a Mac keyboard this costs Option-as-Compose, which is why
//!   it is a setting.
//!
//! This module knows nothing about AppKit: it takes a decoded [`Key`] and
//! returns bytes. That is what makes the whole encoding table testable without
//! pressing anything.

/// Modifier state, as reported by the window layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub command: bool,
}

impl Modifiers {
    pub const NONE: Modifiers =
        Modifiers { shift: false, control: false, alt: false, command: false };

    pub const fn control() -> Modifiers {
        Modifiers { control: true, ..Modifiers::NONE }
    }

    pub const fn alt() -> Modifiers {
        Modifiers { alt: true, ..Modifiers::NONE }
    }

    pub const fn shift() -> Modifiers {
        Modifiers { shift: true, ..Modifiers::NONE }
    }

    /// The xterm modifier parameter: 1 + a bitmask. Used by every `CSI 1;<n>X`
    /// form.
    fn xterm_parameter(self) -> u8 {
        1 + (self.shift as u8)
            + ((self.alt as u8) << 1)
            + ((self.control as u8) << 2)
    }

    fn any(self) -> bool {
        self.shift || self.control || self.alt || self.command
    }
}

/// A key, after the window layer has decoded it.
#[derive(Debug, Clone, PartialEq, Eq, Copy, PartialOrd, Ord, Hash)]
pub enum Key {
    /// A character the keyboard layout produced. Already has dead keys and
    /// input methods applied — Mica never reimplements the keyboard layout.
    Char(char),
    Enter,
    Tab,
    Backspace,
    Delete,
    Escape,
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    /// F1..F20.
    Function(u8),
}

/// How the cursor keys are encoded, set by DECCKM (`CSI ?1h` / `CSI ?1l`).
///
/// Applications like `vim` and `less` switch this, and getting it wrong makes
/// arrow keys insert letters — the classic `^[[A` in a shell prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorKeyMode {
    #[default]
    Normal,
    Application,
}

/// Encoder configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyConfig {
    pub cursor_keys: CursorKeyMode,
    /// When false, Option composes characters the way macOS expects (`Option-e`
    /// then `e` gives `é`) instead of sending ESC. The default is *true*
    /// because a terminal that cannot send Alt-b is a terminal you cannot use
    /// readline in.
    pub alt_sends_escape: bool,
}

impl Default for KeyConfig {
    fn default() -> KeyConfig {
        KeyConfig { cursor_keys: CursorKeyMode::Normal, alt_sends_escape: true }
    }
}

/// The only keys Command is allowed to send to the shell.
///
/// macOS has two text-deletion conventions that every native control obeys:
/// `⌥⌫` deletes the word to the left, and `⌘⌫` deletes to the start of the
/// line. The first needs nothing special here — Option already sends an ESC
/// prefix, so `⌥⌫` is `ESC DEL`, which is exactly what readline binds to
/// `backward-kill-word`. The second has no such luck: Command is reserved for
/// the application, so `⌘⌫` produced nothing at all and the key silently did
/// less than it does in every other Mac text field.
///
/// `^U` is the byte to send, and it is worth being precise about what it does,
/// because the two common shells disagree:
///
/// - **bash** binds `^U` to `unix-line-discard` — deletes from the cursor back
///   to the start of the line, which is exactly the macOS behaviour.
/// - **zsh** binds it to `kill-whole-line` — deletes the entire line including
///   anything to the right of the cursor.
///
/// The difference only shows when the cursor is mid-line. There is no escape
/// sequence that means "delete to line start" in general, so the choice is
/// between `^U` and sending nothing; a key that usually does the right thing
/// beats a key that never does anything. A user who wants the strict bash
/// behaviour in zsh can `bindkey '^U' backward-kill-line`.
fn command_exception(key: &Key) -> Option<Vec<u8>> {
    match key {
        Key::Backspace => Some(vec![0x15]),
        _ => None,
    }
}

/// Encodes a key press. Returns `None` when the key produces nothing — which
/// is the correct response to a bare modifier or an unhandled combination, and
/// is not the same as producing an empty string.
pub fn encode(key: &Key, modifiers: Modifiers, config: KeyConfig) -> Option<Vec<u8>> {
    // Command is the application's, never the terminal's. Cmd-C must not send
    // a byte to the shell; if it reaches this function at all, something in the
    // menu handling is wrong.
    //
    // One deliberate exception, below.
    if modifiers.command {
        return command_exception(key);
    }

    let mut out = Vec::with_capacity(8);
    let alt_prefix = modifiers.alt && config.alt_sends_escape;

    match key {
        Key::Char(ch) => {
            let bytes = encode_char(*ch, modifiers)?;
            if alt_prefix {
                out.push(0x1b);
            }
            out.extend_from_slice(&bytes);
        }
        Key::Enter => {
            if alt_prefix {
                out.push(0x1b);
            }
            out.push(b'\r');
        }
        Key::Tab => {
            if modifiers.shift {
                // Back-tab. Not `\t` with a flag: nothing downstream would see
                // the flag.
                out.extend_from_slice(b"\x1b[Z");
            } else {
                if alt_prefix {
                    out.push(0x1b);
                }
                out.push(b'\t');
            }
        }
        Key::Backspace => {
            if alt_prefix {
                out.push(0x1b);
            }
            // DEL, matching `kbs=\177`. Control-Backspace is the one exception,
            // where ^H is what every readline expects for delete-word.
            out.push(if modifiers.control { 0x08 } else { 0x7f });
        }
        Key::Escape => {
            if alt_prefix {
                out.push(0x1b);
            }
            out.push(0x1b);
        }
        Key::Up | Key::Down | Key::Right | Key::Left => {
            let final_byte = match key {
                Key::Up => b'A',
                Key::Down => b'B',
                Key::Right => b'C',
                Key::Left => b'D',
                _ => unreachable!(),
            };
            if modifiers.any() {
                // Modified cursor keys are always CSI form, even in
                // application mode — `SS3 1;5A` is not a thing.
                out.extend_from_slice(b"\x1b[1;");
                out.extend_from_slice(modifiers.xterm_parameter().to_string().as_bytes());
                out.push(final_byte);
            } else {
                match config.cursor_keys {
                    CursorKeyMode::Normal => out.extend_from_slice(b"\x1b["),
                    CursorKeyMode::Application => out.extend_from_slice(b"\x1bO"),
                }
                out.push(final_byte);
            }
        }
        Key::Home | Key::End => {
            let final_byte = if matches!(key, Key::Home) { b'H' } else { b'F' };
            if modifiers.any() {
                out.extend_from_slice(b"\x1b[1;");
                out.extend_from_slice(modifiers.xterm_parameter().to_string().as_bytes());
                out.push(final_byte);
            } else {
                match config.cursor_keys {
                    CursorKeyMode::Normal => out.extend_from_slice(b"\x1b["),
                    CursorKeyMode::Application => out.extend_from_slice(b"\x1bO"),
                }
                out.push(final_byte);
            }
        }
        Key::Insert => out.extend_from_slice(&tilde(2, modifiers)),
        Key::Delete => out.extend_from_slice(&tilde(3, modifiers)),
        Key::PageUp => out.extend_from_slice(&tilde(5, modifiers)),
        Key::PageDown => out.extend_from_slice(&tilde(6, modifiers)),
        Key::Function(n) => out.extend_from_slice(&function_key(*n, modifiers)?),
    }

    (!out.is_empty()).then_some(out)
}

/// The `CSI <n> ~` family, with the modifier parameter when one applies.
fn tilde(number: u8, modifiers: Modifiers) -> Vec<u8> {
    let mut out = Vec::with_capacity(8);
    out.extend_from_slice(b"\x1b[");
    out.extend_from_slice(number.to_string().as_bytes());
    if modifiers.any() {
        out.push(b';');
        out.extend_from_slice(modifiers.xterm_parameter().to_string().as_bytes());
    }
    out.push(b'~');
    out
}

/// F1–F4 are `SS3` sequences and the rest are `CSI ~` — an inconsistency
/// inherited from the VT220 that every terminal reproduces, because every
/// application expects it.
fn function_key(n: u8, modifiers: Modifiers) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(8);
    match n {
        1..=4 => {
            let final_byte = b'P' + (n - 1);
            if modifiers.any() {
                out.extend_from_slice(b"\x1b[1;");
                out.extend_from_slice(modifiers.xterm_parameter().to_string().as_bytes());
                out.push(final_byte);
            } else {
                out.extend_from_slice(b"\x1bO");
                out.push(final_byte);
            }
        }
        5..=20 => {
            // The numbering has gaps at 16, 22, 27, 30, and 35. They are not a
            // mistake; they are what the VT220 did.
            const NUMBERS: [u8; 16] =
                [15, 17, 18, 19, 20, 21, 23, 24, 25, 26, 28, 29, 31, 32, 33, 34];
            let number = NUMBERS[(n - 5) as usize];
            return Some(tilde(number, modifiers));
        }
        _ => return None,
    }
    Some(out)
}

/// Applies Control to a character.
///
/// The control range is a mechanical fold of the ASCII table, and spelling out
/// the special cases (`^@`, `^[`, `^\`, `^]`, `^^`, `^_`, `^?`) is what makes
/// Ctrl-Space send NUL rather than nothing.
fn encode_char(ch: char, modifiers: Modifiers) -> Option<Vec<u8>> {
    if !modifiers.control {
        let mut buffer = [0u8; 4];
        return Some(ch.encode_utf8(&mut buffer).as_bytes().to_vec());
    }

    let byte = match ch {
        ' ' | '@' => Some(0x00),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        '?' => Some(0x7f),
        'a'..='z' => Some(ch as u8 - b'a' + 1),
        'A'..='Z' => Some(ch as u8 - b'A' + 1),
        // Ctrl with a digit or punctuation has no encoding; sending the bare
        // character would be worse than sending nothing, because it would
        // silently insert text the user did not type.
        _ => None,
    }?;
    Some(vec![byte])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(key: Key, modifiers: Modifiers) -> Option<Vec<u8>> {
        encode(&key, modifiers, KeyConfig::default())
    }

    fn bytes(key: Key, modifiers: Modifiers) -> Vec<u8> {
        let described = format!("{key:?}");
        press(key, modifiers).unwrap_or_else(|| panic!("{described} produced nothing"))
    }

    #[test]
    fn plain_characters_go_through_as_utf8() {
        assert_eq!(bytes(Key::Char('a'), Modifiers::NONE), b"a");
        assert_eq!(bytes(Key::Char('A'), Modifiers::shift()), b"A");
        assert_eq!(bytes(Key::Char('é'), Modifiers::NONE), "é".as_bytes());
        assert_eq!(bytes(Key::Char('世'), Modifiers::NONE), "世".as_bytes());
    }

    #[test]
    fn backspace_sends_del_because_that_is_what_the_terminfo_claims() {
        // `kbs=\177`. Claiming DEL and sending ^H is how a terminal ends up
        // with a backspace key that deletes forwards in half the programs on
        // the machine.
        assert_eq!(bytes(Key::Backspace, Modifiers::NONE), vec![0x7f]);
    }

    #[test]
    fn control_backspace_sends_the_other_one_for_delete_word() {
        assert_eq!(bytes(Key::Backspace, Modifiers::control()), vec![0x08]);
    }

    #[test]
    fn the_control_range_is_encoded_correctly() {
        assert_eq!(bytes(Key::Char('c'), Modifiers::control()), vec![0x03], "Ctrl-C");
        assert_eq!(bytes(Key::Char('d'), Modifiers::control()), vec![0x04], "Ctrl-D");
        assert_eq!(bytes(Key::Char('z'), Modifiers::control()), vec![0x1a], "Ctrl-Z");
        assert_eq!(bytes(Key::Char('a'), Modifiers::control()), vec![0x01], "Ctrl-A");
        // Case must not matter: Ctrl-Shift-C is still Ctrl-C.
        assert_eq!(bytes(Key::Char('C'), Modifiers::control()), vec![0x03]);
    }

    #[test]
    fn control_space_sends_nul() {
        // Used by readline to set the mark, and easy to lose.
        assert_eq!(bytes(Key::Char(' '), Modifiers::control()), vec![0x00]);
        assert_eq!(bytes(Key::Char('@'), Modifiers::control()), vec![0x00]);
    }

    #[test]
    fn the_control_punctuation_range_is_complete() {
        for (ch, expected) in
            [('[', 0x1b), ('\\', 0x1c), (']', 0x1d), ('^', 0x1e), ('_', 0x1f), ('?', 0x7f)]
        {
            assert_eq!(bytes(Key::Char(ch), Modifiers::control()), vec![expected], "Ctrl-{ch}");
        }
    }

    #[test]
    fn control_with_a_digit_sends_nothing_rather_than_the_digit() {
        // Sending the bare character would silently insert text the user did
        // not type, which is worse than doing nothing.
        assert_eq!(press(Key::Char('5'), Modifiers::control()), None);
    }

    #[test]
    fn alt_prefixes_with_escape() {
        assert_eq!(bytes(Key::Char('b'), Modifiers::alt()), vec![0x1b, b'b']);
        assert_eq!(bytes(Key::Char('f'), Modifiers::alt()), vec![0x1b, b'f']);
    }

    #[test]
    fn alt_can_be_turned_off_so_option_composes_characters() {
        let config = KeyConfig { alt_sends_escape: false, ..KeyConfig::default() };
        assert_eq!(encode(&Key::Char('é'), Modifiers::alt(), config).unwrap(), "é".as_bytes());
    }

    #[test]
    fn command_never_reaches_the_shell_except_for_the_one_documented_key() {
        // Cmd-C is a menu item, not a keystroke. If this ever returns bytes,
        // copying text would also send it to the running program.
        for key in [Key::Char('c'), Key::Char('v'), Key::Enter, Key::Left, Key::Delete] {
            assert_eq!(
                press(key.clone(), Modifiers { command: true, ..Modifiers::NONE }),
                None,
                "{key:?} leaked to the shell"
            );
        }
    }

    #[test]
    fn option_backspace_deletes_the_word_to_the_left() {
        // `ESC DEL` is readline's `backward-kill-word`, and it comes out of the
        // ordinary Option-sends-escape rule rather than a special case.
        assert_eq!(
            press(Key::Backspace, Modifiers { alt: true, ..Modifiers::NONE }),
            Some(vec![0x1b, 0x7f])
        );
    }

    #[test]
    fn command_backspace_deletes_to_the_start_of_the_line() {
        // The macOS convention every native text field obeys. Before this it
        // sent nothing at all, because Command returns early.
        assert_eq!(
            press(Key::Backspace, Modifiers { command: true, ..Modifiers::NONE }),
            Some(vec![0x15]),
            "Cmd-Backspace should send ^U"
        );
    }

    #[test]
    fn the_three_backspaces_are_three_different_keys() {
        // Plain, word, line. A terminal that collapses any two of these has
        // taken a key away from the user without telling them.
        let plain = press(Key::Backspace, Modifiers::NONE);
        let word = press(Key::Backspace, Modifiers { alt: true, ..Modifiers::NONE });
        let line = press(Key::Backspace, Modifiers { command: true, ..Modifiers::NONE });

        assert_eq!(plain, Some(vec![0x7f]));
        assert_ne!(plain, word);
        assert_ne!(word, line);
        assert_ne!(plain, line);
    }

    #[test]
    fn command_and_option_together_do_not_produce_a_third_thing_by_accident() {
        // Command wins, because Command is checked first. Worth pinning: the
        // alternative is `ESC ^U`, which no readline binds and which would
        // insert a literal control character in some programs.
        assert_eq!(
            press(
                Key::Backspace,
                Modifiers { command: true, alt: true, ..Modifiers::NONE }
            ),
            Some(vec![0x15])
        );
    }

    #[test]
    fn cursor_keys_follow_decckm() {
        let normal = KeyConfig::default();
        let application =
            KeyConfig { cursor_keys: CursorKeyMode::Application, ..KeyConfig::default() };

        assert_eq!(encode(&Key::Up, Modifiers::NONE, normal).unwrap(), b"\x1b[A");
        assert_eq!(encode(&Key::Up, Modifiers::NONE, application).unwrap(), b"\x1bOA");
        assert_eq!(encode(&Key::Left, Modifiers::NONE, normal).unwrap(), b"\x1b[D");
        assert_eq!(encode(&Key::Left, Modifiers::NONE, application).unwrap(), b"\x1bOD");
    }

    #[test]
    fn modified_cursor_keys_are_always_csi_even_in_application_mode() {
        // `SS3 1;5A` is not a sequence anything understands.
        let application =
            KeyConfig { cursor_keys: CursorKeyMode::Application, ..KeyConfig::default() };
        assert_eq!(
            encode(&Key::Right, Modifiers::control(), application).unwrap(),
            b"\x1b[1;5C"
        );
    }

    #[test]
    fn the_xterm_modifier_parameter_is_computed_correctly() {
        // 1 + shift(1) + alt(2) + control(4), which is the table every
        // application decodes against.
        assert_eq!(bytes(Key::Up, Modifiers::shift()), b"\x1b[1;2A");
        assert_eq!(bytes(Key::Up, Modifiers::alt()), b"\x1b[1;3A");
        assert_eq!(bytes(Key::Up, Modifiers::control()), b"\x1b[1;5A");
        assert_eq!(
            bytes(Key::Up, Modifiers { shift: true, control: true, ..Modifiers::NONE }),
            b"\x1b[1;6A"
        );
        assert_eq!(
            bytes(
                Key::Up,
                Modifiers { shift: true, alt: true, control: true, ..Modifiers::NONE }
            ),
            b"\x1b[1;8A"
        );
    }

    #[test]
    fn shift_tab_is_back_tab_not_a_flagged_tab() {
        assert_eq!(bytes(Key::Tab, Modifiers::NONE), b"\t");
        assert_eq!(bytes(Key::Tab, Modifiers::shift()), b"\x1b[Z");
    }

    #[test]
    fn enter_sends_carriage_return_not_line_feed() {
        // A shell reading a line wants CR; sending LF makes some readline
        // configurations insert a literal newline instead of executing.
        assert_eq!(bytes(Key::Enter, Modifiers::NONE), b"\r");
    }

    #[test]
    fn the_tilde_family_is_encoded_with_the_right_numbers() {
        assert_eq!(bytes(Key::Insert, Modifiers::NONE), b"\x1b[2~");
        assert_eq!(bytes(Key::Delete, Modifiers::NONE), b"\x1b[3~");
        assert_eq!(bytes(Key::PageUp, Modifiers::NONE), b"\x1b[5~");
        assert_eq!(bytes(Key::PageDown, Modifiers::NONE), b"\x1b[6~");
    }

    #[test]
    fn modified_tilde_keys_carry_the_parameter_before_the_tilde() {
        assert_eq!(bytes(Key::Delete, Modifiers::control()), b"\x1b[3;5~");
    }

    #[test]
    fn f1_to_f4_are_ss3_and_the_rest_are_csi_tilde() {
        assert_eq!(bytes(Key::Function(1), Modifiers::NONE), b"\x1bOP");
        assert_eq!(bytes(Key::Function(4), Modifiers::NONE), b"\x1bOS");
        assert_eq!(bytes(Key::Function(5), Modifiers::NONE), b"\x1b[15~");
        assert_eq!(bytes(Key::Function(12), Modifiers::NONE), b"\x1b[24~");
    }

    #[test]
    fn the_function_key_numbering_skips_the_numbers_the_vt220_skipped() {
        // F5 is 15, not 14 — and 16, 22, 27, 30, 35 are absent throughout.
        let numbers: Vec<Vec<u8>> =
            (5..=20).map(|n| bytes(Key::Function(n), Modifiers::NONE)).collect();
        assert_eq!(numbers[0], b"\x1b[15~", "F5");
        assert_eq!(numbers[1], b"\x1b[17~", "F6 — 16 is skipped");
        assert_eq!(numbers[15], b"\x1b[34~", "F20");
        // Every one distinct.
        let unique: std::collections::HashSet<_> = numbers.iter().collect();
        assert_eq!(unique.len(), 16);
    }

    #[test]
    fn an_out_of_range_function_key_produces_nothing() {
        assert_eq!(press(Key::Function(0), Modifiers::NONE), None);
        assert_eq!(press(Key::Function(21), Modifiers::NONE), None);
    }

    #[test]
    fn home_and_end_follow_the_cursor_key_mode_too() {
        let application =
            KeyConfig { cursor_keys: CursorKeyMode::Application, ..KeyConfig::default() };
        assert_eq!(bytes(Key::Home, Modifiers::NONE), b"\x1b[H");
        assert_eq!(bytes(Key::End, Modifiers::NONE), b"\x1b[F");
        assert_eq!(encode(&Key::Home, Modifiers::NONE, application).unwrap(), b"\x1bOH");
    }

    #[test]
    fn escape_is_one_byte_and_alt_escape_is_two() {
        assert_eq!(bytes(Key::Escape, Modifiers::NONE), vec![0x1b]);
        assert_eq!(bytes(Key::Escape, Modifiers::alt()), vec![0x1b, 0x1b]);
    }
}
