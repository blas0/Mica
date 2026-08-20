//! Mica — a GPU-rendered terminal for macOS.

use mica_core::settings::{self, Settings};

fn main() {
    // A malformed settings file is reported and then ignored, rather than
    // stopping the terminal from opening. Refusing to start is a bad failure
    // mode for the program you would use to fix the file.
    let path = settings::default_path();
    let settings = match Settings::load(&path) {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("mica: {} — {error}", path.display());
            eprintln!("mica: continuing with default settings");
            Settings::default()
        }
    };

    // Written on first launch so `⌘,` always has something to open, and so the
    // catalogue of every option is discoverable without reading the source.
    if let Err(error) = mica_shell::config::ensure(
        &path,
        &settings,
        &mica_shell::config::key_docs(&mica_shell::bindings::Bindings::from_overrides(
            &settings.keys,
        )),
    ) {
        eprintln!("mica: could not write {} — {error}", path.display());
    }

    mica_shell::app::run(settings);
}
