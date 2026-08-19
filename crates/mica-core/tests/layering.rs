//! The crate boundary, enforced.
//!
//! From the build plan's risk register: *"Crate boundary erodes — high over
//! time."* Every other property in this codebase is protected by a test; this
//! one was protected only by whoever was reviewing, which is exactly the kind of
//! rule that holds for six weeks and then quietly stops holding.
//!
//! The check is textual and deliberately crude. A cleverer version would parse
//! the crate graph, and it would also be a thing that needs maintaining. This
//! one fails loudly, names the file, and takes twenty milliseconds.

use std::path::{Path, PathBuf};

/// What each crate is forbidden from mentioning.
const RULES: &[(&str, &[&str])] = &[
    // The bottom of the stack: terminal state, PTY, semantics, settings.
    // Neither the GPU nor the window may leak into it.
    ("mica-core", &["objc2_metal", "objc2_app_kit", "objc2_quartz_core", "mica_gpu", "mica_atlas"]),
    // Rasterisation is CoreText's job. Metal belongs one crate up, which is
    // what lets glyph packing and box geometry be tested headlessly.
    ("mica-atlas", &["objc2_metal", "objc2_app_kit", "objc2_quartz_core", "mica_gpu"]),
    // The renderer must not know a window exists — that is what makes the
    // offscreen render path, and therefore the pixel tests, possible.
    ("mica-gpu", &["objc2_app_kit"]),
];

fn crates_dir() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `.../crates/mica-core`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .to_path_buf()
}

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out
}

#[test]
fn no_crate_imports_a_layer_above_itself() {
    let crates = crates_dir();
    let mut violations: Vec<String> = Vec::new();
    let mut checked = 0;

    for (crate_name, forbidden) in RULES {
        let src = crates.join(crate_name).join("src");
        assert!(src.is_dir(), "{} does not exist — did a crate get renamed?", src.display());

        for file in rust_sources(&src) {
            let Ok(text) = std::fs::read_to_string(&file) else { continue };
            checked += 1;
            for (number, line) in text.lines().enumerate() {
                let trimmed = line.trim_start();
                // Only `use` statements and paths, not prose. Several modules
                // discuss the layering in their own documentation, and a doc
                // comment naming a crate is the opposite of a violation.
                if trimmed.starts_with("//") || trimmed.starts_with("*") {
                    continue;
                }
                for name in *forbidden {
                    if line.contains(name) {
                        violations.push(format!(
                            "{crate_name} must not depend on {name}: {}:{}\n    {}",
                            file.strip_prefix(&crates).unwrap_or(&file).display(),
                            number + 1,
                            line.trim()
                        ));
                    }
                }
            }
        }
    }

    assert!(checked > 20, "only scanned {checked} files — the walk is not finding sources");
    assert!(
        violations.is_empty(),
        "the crate boundary has eroded:\n\n{}\n\nDependencies point inward: \
         mica-shell → mica-gpu → mica-atlas → mica-core.",
        violations.join("\n")
    );
}

#[test]
fn only_one_file_is_allowed_to_spawn_a_process() {
    // Isolating dangerous behaviour is worth nothing if a second copy appears
    // somewhere else. `fork` and `execve` live in pty.rs and nowhere else.
    let crates = crates_dir();
    let mut offenders: Vec<String> = Vec::new();

    // `src` only. Test files legitimately mention these names — this one names
    // them in order to look for them, and flagged itself on the first run.
    let sources: Vec<PathBuf> = ["mica-core", "mica-atlas", "mica-gpu", "mica-shell"]
        .iter()
        .flat_map(|name| rust_sources(&crates.join(name).join("src")))
        .collect();

    for file in sources {
        let name = file.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if name == "pty.rs" {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        for (number, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("*") {
                continue;
            }
            if line.contains("libc::fork") || line.contains("libc::execve") {
                offenders.push(format!(
                    "{}:{}",
                    file.strip_prefix(&crates).unwrap_or(&file).display(),
                    number + 1
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "process spawning escaped pty.rs:\n{}\n\nIt lives in one file so that it \
         can be reviewed in one sitting.",
        offenders.join("\n")
    );
}

#[test]
fn the_terminal_trait_never_grows_a_full_grid_accessor() {
    // The rule the whole damage-driven design rests on, written down where it
    // will fail rather than only in a doc comment.
    let backend = crates_dir().join("mica-core").join("src").join("backend").join("mod.rs");
    let text = std::fs::read_to_string(&backend).expect("backend/mod.rs");

    let declarations: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("fn ") && !line.starts_with("//"))
        .collect();

    for banned in ["fn full_grid", "fn all_rows", "fn grid("] {
        assert!(
            !declarations.iter().any(|line| line.starts_with(banned)),
            "`{banned}` was added to TerminalCore. Something will call it every \
             frame and the zero-frames-when-idle property will die quietly. Use \
             damage_all() plus dirty_rows() instead."
        );
    }
    assert!(
        declarations.iter().any(|line| line.starts_with("fn dirty_rows")),
        "dirty_rows disappeared from the trait"
    );
    assert!(
        declarations.iter().any(|line| line.starts_with("fn damage_all")),
        "damage_all disappeared from the trait"
    );
}
