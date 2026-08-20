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
        assert_eq!(Settings::parse(&first).unwrap(), Settings::default());

        // A second call must not clobber whatever the user has since written.
        std::fs::write(&file, "[appearance]\ntheme = \"basalt\"\n").unwrap();
        ensure(&file, &Settings::default(), &keys).unwrap();
        assert_eq!(Settings::load(&file).unwrap().theme, "basalt");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
