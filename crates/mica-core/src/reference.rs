//! The settings file Mica writes for you — a catalogue, then a configuration.
//!
//! `⌘,` opens this. It has two halves, and the split is the whole point:
//!
//! - **Config Flags** is a commented catalogue of every option Mica has and
//!   every value each one accepts. It is regenerated on every write, so it
//!   cannot drift from the code the way a hand-maintained example file does.
//!   Nothing in it is live; it is there to be read and copied from.
//! - **Config** is the live TOML, written out in full: every setting with its
//!   current value, defaults included, and every bindable action with the
//!   chord it answers to. Nothing is implied by absence. Edit a value in
//!   place — there is no line to go find and uncomment first.
//!
//! Writing the defaults out has a cost, and it is not hidden: a value in this
//! file is a value you own, so changing a default in the code will not reach an
//! install that has this file. Delete a key to hand it back, or delete the file
//! to hand all of them back — Mica writes a fresh one on the next launch.
//!
//! ## What this module does not know
//!
//! Keyboard chords. The grammar for `cmd+shift+p` belongs to the window layer,
//! which is the only layer that knows what a modifier key is — so the caller
//! passes the key catalogue in as [`KeyDoc`]s. This crate must not learn what
//! Command is.

use crate::material::builtin_themes;
use crate::motion::MotionStyle;
use crate::settings::{AmbientSettings, Settings, SettingsError};

/// One bindable action, as the window layer describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyDoc {
    /// `namespace.verb` — the key written in `[keys]`.
    pub action: String,
    /// What a person reads in the palette.
    pub label: String,
    /// The chord bound to it right now, in config spelling. Empty when the
    /// action is deliberately unbound.
    pub chord: String,
    /// False for actions Mica recognises but has not implemented yet. Saying
    /// so in the file is better than a binding that silently does nothing.
    pub implemented: bool,
}

/// The marker lines. Kept as constants because tests and any future
/// merge-preserving writer both need to agree on them exactly.
pub const FLAGS_START: &str = "# Config Flags Start";
pub const FLAGS_END: &str = "# Config Flags End";
pub const CONFIG_START: &str = "# Config Start";
pub const CONFIG_END: &str = "# Config End";

/// Renders the whole file.
pub fn document(settings: &Settings, keys: &[KeyDoc]) -> Result<String, SettingsError> {
    let mut out = String::with_capacity(8 * 1024);

    out.push_str("# Mica — settings\n");
    out.push_str("#\n");
    out.push_str("# Everything above `Config Start` is a comment: a catalogue of every\n");
    out.push_str("# option and the values it accepts. Read it to find out what a setting\n");
    out.push_str("# will take; change the setting itself below.\n");
    out.push_str("#\n");
    out.push_str("# The configuration below is complete: every setting, with the value\n");
    out.push_str("# it currently has. Edit any of them in place. A key you delete goes\n");
    out.push_str("# back to following Mica's default, and a file you delete is written\n");
    out.push_str("# again from scratch on the next launch.\n");
    out.push_str("#\n");
    out.push_str("# Mica rewrites this file when you change a setting from inside the\n");
    out.push_str("# app, and regenerates the catalogue each time — comments you add\n");
    out.push_str("# inside the configuration are not kept.\n");
    out.push_str("#\n");
    out.push_str("# A malformed file is reported and ignored rather than silently\n");
    out.push_str("# discarded, so a stray bracket cannot lose your theme.\n");
    out.push_str("#\n");
    out.push_str("# Edits apply when Mica next becomes the active application — save\n");
    out.push_str("# here, switch back, done. Theme, caret motion, ambient light and key\n");
    out.push_str("# bindings take effect immediately; font, grid size, scrollback and\n");
    out.push_str("# [shell] wait for the next launch.\n");
    out.push('\n');

    out.push_str(FLAGS_START);
    out.push('\n');
    flags(&mut out, keys);
    out.push_str(FLAGS_END);
    out.push_str("\n\n");

    out.push_str(CONFIG_START);
    out.push('\n');
    out.push_str(config(settings, keys)?.trim_end());
    out.push('\n');
    out.push_str(CONFIG_END);
    out.push('\n');

    Ok(out)
}

/// The live half: every setting, and every binding.
///
/// `[keys]` is built from `keys` rather than from `settings.keys`, because
/// `settings.keys` holds only the overrides. What belongs in a complete file is
/// the table the app is actually running — which only the window layer can
/// report, since it is the one that resolved the defaults.
fn config(settings: &Settings, keys: &[KeyDoc]) -> Result<String, SettingsError> {
    let mut file = settings.to_file_complete();
    file.keys = Some(keys.iter().map(|k| (k.action.clone(), k.chord.clone())).collect());
    toml::to_string_pretty(&file).map_err(|e| SettingsError::Serialize(e.to_string()))
}

fn section(out: &mut String, name: &str, blurb: &str) {
    out.push_str("#\n");
    out.push_str(&format!("# [{name}] — {blurb}\n"));
}

fn flag(out: &mut String, key: &str, values: &str) {
    out.push_str(&format!("#   {key:<28} {values}\n"));
}

fn flags(out: &mut String, keys: &[KeyDoc]) {
    let d = Settings::default();

    let themes = builtin_themes()
        .iter()
        .map(|t| t.id.clone())
        .collect::<Vec<_>>()
        .join(" | ");
    section(out, "appearance", "theme, typeface, size");
    flag(out, "theme", &format!("{themes}   (default: {})", d.theme));
    flag(out, "font_family", &format!("any installed monospace family   (default: {})", d.font_family));
    flag(out, "font_size", &format!("points, 6.0 -> 72.0   (default: {:.1})", d.font_size));

    section(out, "window", "the grid and the bell");
    flag(out, "bell", "audible | visual | off   (default: off)");
    flag(out, "columns", &format!("1 -> 500   (default: {})", d.columns));
    flag(out, "rows", &format!("1 -> 300   (default: {})", d.rows));
    flag(out, "scrollback", &format!("lines of history   (default: {})", d.scrollback));

    section(out, "shell", "what runs, where it starts, and what a new pane inherits");
    flag(out, "program", "absolute path   (default: $SHELL, else /bin/sh)");
    flag(out, "starting-dir", "absolute path or ~/…   (default: your home directory)");
    flag(out, "new-pane-from-focused-pane", "inherit | starting-dir   (default: inherit)");
    flag(out, "focus-new-panes", "true | false   (default: true)");

    section(out, "ambient", "the light behind the grid");
    flag(out, "enabled", "true | false   (default: true)");
    flag(
        out,
        "intensity",
        &format!(
            "{:.1} -> {:.1}   (default: {:.1} — what the toggle used to mean)",
            AmbientSettings::RANGE.start(),
            AmbientSettings::RANGE.end(),
            d.ambient.intensity
        ),
    );

    section(out, "renderer", "frame pacing and native substitutions");
    flag(out, "frame_cap", "0 follows the display; any other value implies a timer");
    flag(out, "substitute_progress", "true | false   (default: false)");
    flag(out, "substitute_spinner", "true | false   (default: false)");

    let styles = MotionStyle::ALL.iter().map(|s| s.id()).collect::<Vec<_>>().join(" | ");
    section(out, "motion", "caret physics");
    flag(out, "cursor", &format!("{styles}   (default: {})", d.motion.style.id()));
    flag(out, "speed", &format!("0.25 -> 4.0   (default: {:.2})", d.motion.speed));
    flag(out, "intensity", &format!("0.0 -> 1.0   (default: {:.2})", d.motion.intensity));
    flag(out, "decay", "true | false — the trail behind the caret");
    flag(out, "blink", "true | false");
    flag(out, "reduce", "true | false — also forced on by the system preference");

    section(out, "keys", "action = \"chord\"");
    out.push_str("#   Modifiers: cmd, ctrl, alt (also opt/option), shift — any order,\n");
    out.push_str("#   joined by `+`. Mica writes them back as cmd/ctrl/alt/shift.\n");
    out.push_str("#   Named keys: enter, tab, space, escape, backspace, delete, home, end,\n");
    out.push_str("#   pageup, pagedown, up, down, left, right, f1 … f12.\n");
    out.push_str("#   An empty value unbinds the action deliberately.\n");
    out.push_str("#\n");
    let width = keys.iter().map(|k| k.action.len()).max().unwrap_or(0).max(1);
    for key in keys {
        let chord = if key.chord.is_empty() { "".to_owned() } else { key.chord.clone() };
        let note = if key.implemented { "" } else { "   (recognised, not implemented yet)" };
        out.push_str(&format!(
            "#   {:<width$} = {:<18} # {}{note}\n",
            key.action,
            format!("\"{chord}\""),
            key.label,
            width = width
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Vec<KeyDoc> {
        vec![
            KeyDoc {
                action: "palette.toggle".into(),
                label: "Command Palette".into(),
                chord: "f5".into(),
                implemented: true,
            },
            KeyDoc {
                action: "pane.split_right".into(),
                label: "Split Right".into(),
                chord: "cmd+d".into(),
                implemented: false,
            },
        ]
    }

    /// What `Settings::parse` should hand back for a document built from `s`:
    /// `s` itself, plus the binding table spelled out.
    fn as_written(s: &Settings) -> Settings {
        Settings {
            keys: keys().iter().map(|k| (k.action.clone(), k.chord.clone())).collect(),
            ..s.clone()
        }
    }

    #[test]
    fn the_document_parses_as_the_settings_it_was_built_from() {
        // The catalogue is a comment. If any of it were live TOML the file
        // would either fail to parse or, far worse, quietly apply itself.
        let mut s = Settings::default();
        s.theme = "quartz".into();
        s.ambient.intensity = 0.4;

        let text = document(&s, &keys()).unwrap();
        assert_eq!(Settings::parse(&text).unwrap(), as_written(&s));
    }

    #[test]
    fn an_untouched_install_writes_its_defaults_out_rather_than_implying_them() {
        let text = document(&Settings::default(), &keys()).unwrap();
        assert_eq!(Settings::parse(&text).unwrap(), as_written(&Settings::default()));

        let config = text.split_once(CONFIG_START).unwrap().1;
        for section in ["[appearance]", "[window]", "[renderer]", "[motion]", "[ambient]", "[shell]", "[keys]"] {
            assert!(config.contains(section), "`{section}` is missing from the configuration:\n{config}");
        }
        assert!(
            config.contains(&format!("theme = \"{}\"", Settings::default().theme)),
            "the default theme is not written out:\n{config}"
        );
    }

    #[test]
    fn a_shell_that_follows_the_environment_still_gets_its_keys_written() {
        // `program` and `starting-dir` have no fixed default to print. They are
        // written empty rather than omitted, so the key is there to edit — and
        // empty has to read back as "follow the environment", not as a shell
        // called "".
        let text = document(&Settings::default(), &keys()).unwrap();
        let config = text.split_once(CONFIG_START).unwrap().1;
        assert!(config.contains("program = \"\""), "{config}");
        assert!(config.contains("starting-dir = \"\""), "{config}");
        assert_eq!(Settings::parse(&text).unwrap().shell.program, None);
        assert_eq!(Settings::parse(&text).unwrap().shell.starting_dir, None);
    }

    #[test]
    fn every_bindable_action_appears_in_the_live_configuration() {
        // The catalogue is a comment; a shortcut you can actually change has to
        // exist below the marker too.
        let text = document(&Settings::default(), &keys()).unwrap();
        let config = text.split_once(CONFIG_START).unwrap().1;
        for key in keys() {
            assert!(
                // TOML quotes a bare key containing a dot, and every action id has one.
                config.contains(&format!("\"{}\" = \"{}\"", key.action, key.chord)),
                "`{}` is bindable but not written out:\n{config}",
                key.action
            );
        }
    }

    #[test]
    fn every_section_appears_in_the_catalogue() {
        let text = document(&Settings::default(), &keys()).unwrap();
        let catalogue = text
            .split_once(FLAGS_START)
            .unwrap()
            .1
            .split_once(FLAGS_END)
            .unwrap()
            .0;
        for name in
            ["appearance", "window", "shell", "ambient", "renderer", "motion", "keys"]
        {
            assert!(catalogue.contains(&format!("[{name}]")), "`{name}` is undocumented");
        }
    }

    /// `# [name] — …` and `#   key   … (default: v)` out of the catalogue,
    /// and `key = v` out of the configuration, both keyed by section.
    /// `(section, key) -> value`, in file order.
    type Values = Vec<((String, String), String)>;

    fn defaults_by_section(text: &str) -> (Values, Values) {
        let (catalogue, config) = {
            let (head, rest) = text.split_once(FLAGS_END).unwrap();
            (head.split_once(FLAGS_START).unwrap().1, rest.split_once(CONFIG_START).unwrap().1)
        };

        let mut documented = Vec::new();
        let mut section = String::new();
        for line in catalogue.lines() {
            let line = line.trim_start_matches('#').trim();
            if let Some(rest) = line.strip_prefix('[') {
                section = rest.split(']').next().unwrap_or_default().to_owned();
                continue;
            }
            let Some((key, tail)) = line.split_once(char::is_whitespace) else { continue };
            let Some((_, default)) = tail.split_once("(default: ") else { continue };
            let value = default.split(')').next().unwrap_or_default().split(" —").next().unwrap_or_default();
            documented.push(((section.clone(), key.to_owned()), value.trim().to_owned()));
        }

        let mut live = Vec::new();
        let mut section = String::new();
        for line in config.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix('[') {
                section = rest.split(']').next().unwrap_or_default().to_owned();
                continue;
            }
            let Some((key, value)) = line.split_once(" = ") else { continue };
            live.push((
                (section.clone(), key.trim().trim_matches('"').to_owned()),
                value.trim().trim_matches('"').to_owned(),
            ));
        }
        (documented, live)
    }

    #[test]
    fn the_default_the_catalogue_states_is_the_default_the_configuration_writes() {
        // Found the hard way: the catalogue said motion intensity defaulted to
        // 1.0 while the code said 0.7, because the catalogue was written down
        // instead of generated. Anything a reader could act on has to agree.
        //
        // `[shell] program` and `starting-dir` are exempt: their default is
        // "whatever $SHELL and $HOME say at launch", which has no value to
        // print, so they are written empty on purpose.
        const DYNAMIC: [&str; 2] = ["program", "starting-dir"];

        let text = document(&Settings::default(), &keys()).unwrap();
        let (documented, live) = defaults_by_section(&text);
        assert!(documented.len() > 10, "the catalogue stopped stating defaults: {documented:?}");

        for (place, stated) in &documented {
            if DYNAMIC.contains(&place.1.as_str()) {
                continue;
            }
            let Some((_, written)) = live.iter().find(|(p, _)| p == place) else { continue };
            let agree = written == stated
                || match (written.parse::<f64>(), stated.parse::<f64>()) {
                    (Ok(a), Ok(b)) => (a - b).abs() < f64::EPSILON,
                    _ => false,
                };
            assert!(agree, "[{}] {} is documented as `{stated}` but written as `{written}`", place.0, place.1);
        }
    }

    #[test]
    fn every_theme_and_every_caret_style_is_named() {
        // The catalogue is generated rather than written down precisely so a
        // new theme cannot ship undocumented.
        let text = document(&Settings::default(), &keys()).unwrap();
        for theme in builtin_themes() {
            assert!(text.contains(&theme.id), "theme `{}` is missing", theme.id);
        }
        for style in MotionStyle::ALL {
            assert!(text.contains(style.id()), "caret style `{}` is missing", style.id());
        }
    }

    #[test]
    fn an_unimplemented_binding_says_so() {
        let text = document(&Settings::default(), &keys()).unwrap();
        let line = text
            .lines()
            .find(|l| l.contains("pane.split_right"))
            .expect("the action is missing from the catalogue");
        assert!(line.contains("not implemented"), "{line}");
        let live = text
            .lines()
            .find(|l| l.contains("palette.toggle"))
            .expect("the action is missing from the catalogue");
        assert!(!live.contains("not implemented"), "{live}");
    }

    #[test]
    fn the_markers_are_exactly_where_a_reader_expects_them() {
        let text = document(&Settings::default(), &keys()).unwrap();
        let flags_start = text.find(FLAGS_START).unwrap();
        let flags_end = text.find(FLAGS_END).unwrap();
        let config_start = text.find(CONFIG_START).unwrap();
        let config_end = text.find(CONFIG_END).unwrap();
        assert!(flags_start < flags_end && flags_end < config_start && config_start < config_end);
    }

    #[test]
    fn a_round_trip_through_the_document_is_stable() {
        // Save, load, save again: the second file must equal the first, or
        // opening the settings twice produces a spurious diff every time.
        let mut s = Settings::default();
        s.shell.starting_dir = Some("~/Projects".into());
        s.shell.focus_new_panes = false;

        let once = document(&s, &keys()).unwrap();
        let reparsed = Settings::parse(&once).unwrap();
        assert_eq!(document(&reparsed, &keys()).unwrap(), once);
    }
}
