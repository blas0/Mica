//! The settings file as a *document* — what `⌘,` opens.
//!
//! Two jobs, both of which need the window layer and so cannot live in
//! `mica-core`: describing the bindable actions in the spelling the config
//! file uses, and handing the file to the user's editor.
//!
//! ## Why `⌘,` opens a file rather than a panel
//!
//! `⌘,` is Settings on a Mac, and Mica's settings are a file. Opening the file
//! is the honest thing: everything is in one place, it is greppable and
//! committable, and there is no second surface that can disagree with it.
//! ## One surface
//!
//! There was also a command palette (`F5`) and a shortcut-capture panel
//! (`⌘⇧K`). Both are gone. Between them they held about 2,500 lines to offer a
//! second and third way to do what this file already did, and the boundary
//! between the three was never obvious enough to be worth the drift risk. The
//! file is now the only configuration surface: it persists, diffs, commits and
//! travels, and nothing else writes it. `docs/FOLLOW-UPS.md`, FU-2, has the
//! reasoning and what was given up.

use std::path::{Path, PathBuf};

use mica_core::reference::KeyDoc;
use mica_core::settings::Settings;

use crate::bindings::{Bindings, Chord, BINDABLE};

/// Every bindable action, with what it is bound to right now.
///
/// The user's `text:` bindings come last. They are not in [`BINDABLE`] — there
/// is no fixed catalogue of them — but the file is rewritten from this list, so
/// leaving them out would mean opening Settings deleted them.
pub fn key_docs(bindings: &Bindings) -> Vec<KeyDoc> {
    let mut docs: Vec<KeyDoc> = BINDABLE
        .iter()
        .map(|bindable| KeyDoc {
            action: bindable.id.to_owned(),
            label: bindable.label.to_owned(),
            chord: bindings.chord_for(bindable.id).map(Chord::to_config).unwrap_or_default(),
            implemented: bindable.implemented,
        })
        .collect();
    for (action, chord) in bindings.overrides() {
        if crate::bindings::text_payload(&action).is_some() {
            docs.push(KeyDoc {
                action,
                label: "Send text".to_owned(),
                chord,
                implemented: true,
            });
        }
    }
    docs
}

pub fn path() -> PathBuf {
    mica_core::settings::default_path()
}

/// Creates the file if it is not there, so `⌘,` always has something to open.
///
/// A first-run user opening Settings and being handed an empty document — or
/// worse, an editor error — learns nothing about what Mica can do. The
/// catalogue is the documentation.
pub fn ensure(path: &Path, settings: &Settings, keys: &[KeyDoc]) -> std::io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    settings
        .save(path, keys)
        .map_err(|e| std::io::Error::other(e.to_string()))
}

/// Brings the file up to date before it is opened.
///
/// The file is complete by design — every setting, every binding, spelled out —
/// which is only true if it is rewritten when the set of settings changes. So
/// `⌘,` re-renders it from its own contents: values the user has edited are
/// read back and written out again, and anything Mica has learned to configure
/// since the file was written appears at its default.
///
/// Two things it deliberately will not do. It will not touch a file it cannot
/// parse — a stray bracket should be handed back to the user to fix, not
/// overwritten with defaults. And it will not rewrite a file that is already
/// exactly right, so opening Settings twice does not disturb the mtime.
pub fn refresh(path: &Path, fallback: &Settings, keys: &[KeyDoc]) -> std::io::Result<()> {
    let existing = std::fs::read_to_string(path).ok();
    let settings = match existing.as_deref().map(Settings::parse) {
        Some(Ok(parsed)) => parsed,
        // Unreadable: leave it exactly as it is and let the editor show it.
        Some(Err(_)) => return Ok(()),
        None => fallback.clone(),
    };

    let wanted = mica_core::reference::document(&settings, keys)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    if existing.as_deref() == Some(wanted.as_str()) {
        return Ok(());
    }
    settings.save(path, keys).map_err(|e| std::io::Error::other(e.to_string()))
}

/// Hands the file to whatever the user opens text with.
///
/// `open -t` rather than an in-app editor: writing a text editor is not what a
/// terminal is for, and the user already has one they prefer. `-t` forces the
/// default *text* editor rather than whatever claims `.toml`, which on a
/// developer's machine is often something that would rather run it.
pub fn reveal(path: &Path) -> std::io::Result<()> {
    let status = std::process::Command::new("/usr/bin/open").arg("-t").arg(path).status()?;
    if status.success() {
        return Ok(());
    }
    Err(std::io::Error::other(format!("open -t exited with {status}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bindable_action_is_documented() {
        let docs = key_docs(&Bindings::defaults());
        assert_eq!(docs.len(), BINDABLE.len());
        for bindable in BINDABLE {
            let doc = docs
                .iter()
                .find(|d| d.action == bindable.id)
                .unwrap_or_else(|| panic!("`{}` is missing from the catalogue", bindable.id));
            assert_eq!(doc.label, bindable.label);
        }
    }

    #[test]
    fn the_documented_chords_are_the_ones_the_table_holds() {
        // The catalogue is generated from the table rather than written down,
        // so the file cannot advertise a shortcut that does not work — which
        // is exactly what the old palette used to do.
        let bindings = Bindings::defaults();
        for doc in key_docs(&bindings) {
            if doc.chord.is_empty() {
                assert!(
                    bindings.chord_for(&doc.action).is_none(),
                    "`{}` is bound but documented as unbound",
                    doc.action
                );
                continue;
            }
            let chord = Chord::parse(&doc.chord)
                .unwrap_or_else(|| panic!("`{}` is not parseable", doc.chord));
            assert_eq!(bindings.action(chord), Some(doc.action.as_str()));
        }
    }

    #[test]
    fn settings_is_bound_to_command_comma_and_reload_beside_it() {
        // The two shortcuts a new user has to be told about, and the only two
        // now that configuration is one surface. They are the ones that must
        // not drift.
        let bindings = Bindings::defaults();
        assert_eq!(
            bindings.chord_for("settings.open").map(Chord::to_config).as_deref(),
            Some("cmd+,")
        );
        assert_eq!(
            bindings.chord_for("settings.reload"),
            Chord::parse("cmd+shift+,")
        );
    }

    #[test]
    fn ensure_writes_a_readable_file_once_and_then_leaves_it_alone() {
        let dir = std::env::temp_dir().join(format!("mica-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("settings.toml");
        let keys = key_docs(&Bindings::defaults());

        ensure(&file, &Settings::default(), &keys).unwrap();
        let first = std::fs::read_to_string(&file).unwrap();
        assert!(first.contains(mica_core::reference::FLAGS_START));

        // The written file spells its defaults out, bindings included, so what
        // comes back is the defaults with the whole key table attached.
        let parsed = Settings::parse(&first).unwrap();
        let expected = Settings {
            keys: keys.iter().map(|k| (k.action.clone(), k.chord.clone())).collect(),
            ..Settings::default()
        };
        assert_eq!(parsed, expected);

        // And re-reading it has to reproduce the bindings it was written from,
        // or a first launch would quietly rebind the app to itself.
        assert_eq!(key_docs(&Bindings::from_overrides(&parsed.keys)), keys);

        // A second call must not clobber whatever the user has since written.
        std::fs::write(&file, "[appearance]\ntheme = \"basalt\"\n").unwrap();
        ensure(&file, &Settings::default(), &keys).unwrap();
        assert_eq!(Settings::load(&file).unwrap().theme, "basalt");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opening_settings_brings_an_older_file_up_to_date_without_losing_its_values() {
        // The file promises to be complete. A file written before a setting
        // existed is not, so opening Settings has to re-render it — carrying
        // the values the user chose, filling in the rest.
        let dir = std::env::temp_dir().join(format!("mica-refresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("settings.toml");
        let keys = key_docs(&Bindings::defaults());

        std::fs::write(&file, "[appearance]\ntheme = \"basalt\"\n").unwrap();
        refresh(&file, &Settings::default(), &keys).unwrap();

        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains(mica_core::reference::FLAGS_START), "the catalogue is missing");
        assert!(text.contains("theme = \"basalt\""), "the user's theme was lost:\n{text}");
        assert!(text.contains("[ambient]"), "the new section was not filled in:\n{text}");

        // Idempotent: a second open must not rewrite it.
        let before = std::fs::metadata(&file).unwrap().modified().unwrap();
        refresh(&file, &Settings::default(), &keys).unwrap();
        assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), before);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_does_not_parse_is_handed_back_untouched() {
        // Overwriting it with defaults would silently discard the settings the
        // user was in the middle of editing.
        let dir = std::env::temp_dir().join(format!("mica-broken-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("settings.toml");

        let broken = "[appearance\ntheme = \"basalt\"\n";
        std::fs::write(&file, broken).unwrap();
        refresh(&file, &Settings::default(), &key_docs(&Bindings::defaults())).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), broken);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_text_binding_survives_the_settings_file() {
        // `⌘,` re-renders the whole file from `key_docs`. A text binding is not
        // in BINDABLE, so if `key_docs` did not carry it, opening Settings
        // would delete every one of the user's text bindings — silently, and in
        // the act of showing them the file.
        let dir = std::env::temp_dir().join(format!("mica-textbind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("settings.toml");

        let mut overrides = std::collections::BTreeMap::new();
        overrides.insert("text:probe-payload-42\n".to_owned(), "cmd+shift+l".to_owned());
        let bindings = Bindings::from_overrides(&overrides);
        let keys = key_docs(&bindings);

        ensure(&file, &Settings::default(), &keys).unwrap();
        refresh(&file, &Settings::default(), &keys).unwrap();

        let written = std::fs::read_to_string(&file).unwrap();
        let parsed = Settings::parse(&written).unwrap();
        let chord = Chord::parse("cmd+shift+l").unwrap();
        assert_eq!(
            Bindings::from_overrides(&parsed.keys).action(chord),
            Some("text:probe-payload-42\n"),
            "the text binding did not come back out of the file:\n{written}"
        );

        // The payload holds a newline. It belongs in the value half of the
        // file, where TOML escapes it — never in the comment catalogue, where a
        // raw newline would end the comment and take the rest of the catalogue
        // with it.
        let catalogue = written
            .split(mica_core::reference::FLAGS_END)
            .next()
            .expect("the file has a catalogue");
        // The catalogue explains what `text:` means; it must not print the
        // user's payload, which is the part that can carry a newline.
        assert!(
            !catalogue.contains("probe-payload-42"),
            "a text binding was printed into the catalogue:\n{catalogue}"
        );
        for line in catalogue.lines().skip(1) {
            assert!(line.starts_with('#') || line.is_empty(), "uncommented line: {line:?}");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
