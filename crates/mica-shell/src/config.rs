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
//! `⌘⇧K` opens the shortcut panel, which edits exactly one section of the same
//! file.

use std::path::{Path, PathBuf};

use mica_core::reference::KeyDoc;
use mica_core::settings::Settings;

use crate::bindings::{Bindings, Chord, BINDABLE};

/// Every bindable action, with what it is bound to right now.
pub fn key_docs(bindings: &Bindings) -> Vec<KeyDoc> {
    BINDABLE
        .iter()
        .map(|bindable| KeyDoc {
            action: bindable.id.to_owned(),
            label: bindable.label.to_owned(),
            chord: bindings.chord_for(bindable.id).map(Chord::to_config).unwrap_or_default(),
            implemented: bindable.implemented,
        })
        .collect()
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
        // is exactly what the palette used to do.
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
    fn the_palette_is_bound_to_f5_and_settings_to_command_comma() {
        // The two entry points, pinned. They are the only shortcuts a new user
        // has to be told about, so they are the two that must not drift.
        let bindings = Bindings::defaults();
        assert_eq!(
            bindings.chord_for("palette.toggle").map(Chord::to_config).as_deref(),
            Some("f5")
        );
        assert_eq!(
            bindings.chord_for("settings.open").map(Chord::to_config).as_deref(),
            Some("cmd+,")
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
}
