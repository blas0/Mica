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

    mica_shell::app::run(settings);
}
