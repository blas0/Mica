//! Generating and installing Mica's terminfo entry.
//!
//! The entry is compiled with `tic -x` into the user's own `~/.terminfo` at
//! session start, and **`TERM` is only set to `mica` once that has succeeded**.
//!
//! That ordering is the whole point. Claiming a `TERM` the child cannot look up
//! is strictly worse than claiming none, because every curses program then
//! falls back to `dumb` — no colour, no cursor addressing, no alternate screen.
//! A terminal that advertises capabilities it cannot be asked about is a
//! terminal that breaks `vim` on a machine where `~/.terminfo` is not writable.
//!
//! ## On cancelling capabilities
//!
//! Several capabilities are cancelled with a trailing `@`. This is not tidying:
//! applications choose their redraw and input paths from this contract, so a
//! silent no-op is not harmless. `flash@` means a program picks the audible
//! bell instead of waiting for a visual one that never comes; `initc@` means it
//! uses the palette it was given rather than trying to redefine it.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The `TERM` value, claimed only after a successful install.
pub const TERM_NAME: &str = "mica";

/// The fallback. `xterm-256color` is present on every macOS and understates
/// what Mica does, which is the safe direction to be wrong in.
pub const FALLBACK_TERM: &str = "xterm-256color";

/// The terminfo source.
///
/// Built on `xterm-256color` rather than from nothing: reimplementing 200
/// capabilities to arrive at the same place would be a way to introduce
/// mistakes, not a way to be precise.
pub fn source() -> String {
    let mut entry = String::with_capacity(2048);
    entry.push_str("mica|Mica terminal,\n");

    for line in [
        // --- what Mica adds over xterm-256color ---------------------------
        //
        // Truecolor, in both the forms that exist: `Tc` is what tmux reads,
        // `RGB` is what ncurses reads. Claiming only one leaves half the
        // ecosystem in 256-colour mode.
        "\tTc,",
        "\tRGB#8,",
        // DEC 2026 synchronized output. A program wrapping a redraw in these
        // gets an atomic frame instead of tearing — the single most valuable
        // extension for a full-screen TUI.
        "\tSync=\\E[?2026%?%p1%{1}%-%th%el%;,",
        // DECSCUSR cursor shape.
        "\tSs=\\E[%p1%d q,",
        "\tSe=\\E[2 q,",
        // Styled underlines and per-cell underline colour.
        "\tSmulx=\\E[4:%p1%dm,",
        "\tSetulc=\\E[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%dm,",
        "\tSetulc1=\\E[58:5:%p1%dm,",
        "\tol=\\E[59m,",
        // Overline.
        "\tSmol=\\E[53m,",
        "\tRmol=\\E[55m,",
        // OSC 52 clipboard.
        "\tMs=\\E]52;%p1%s;%p2%s\\007,",
        // Clear scrollback, so `clear` really clears.
        "\tE3=\\E[3J,",
        // Backspace is DEL. This must agree with `keys.rs`; the two are a
        // contract with each other, and disagreeing is invisible until
        // somebody's backspace key starts deleting forwards.
        "\tkbs=\\177,",
        "\tkdch1=\\E[3~,",
        // Bracketed paste.
        "\tBE=\\E[?2004h,",
        "\tBD=\\E[?2004l,",
        "\tPS=\\E[200~,",
        "\tPE=\\E[201~,",
        // Focus reporting.
        "\tfe=\\E[?1004h,",
        "\tfd=\\E[?1004l,",
        //
        // --- cancelled: capabilities Mica has no subsystem to honour -------
        //
        // Cursor colour: Mica's caret takes the theme's accent role and is not
        // separately addressable.
        "\tCr@,",
        "\tCs@,",
        // Palette mutation: the 256 colours are derived from the theme's eight
        // roles, so a program cannot redefine one without breaking that.
        "\tinitc@,",
        "\tinitp@,",
        // No visual bell.
        "\tflash@,",
        // No memory lock.
        "\tmeml@,",
        "\tmemu@,",
        // No meta mode: Alt sends an ESC prefix, which `keys.rs` guarantees.
        "\tsmm@,",
        "\trmm@,",
        //
        "\tuse=xterm-256color,\n",
    ] {
        entry.push_str(line);
        entry.push('\n');
    }
    entry
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    /// `tic` succeeded; `TERM` may be set to `mica`.
    Installed { term: String, terminfo_dir: PathBuf },
    /// It did not. The reason is kept so it can be reported rather than
    /// guessed at, and `TERM` stays on the fallback.
    Failed { term: String, reason: String },
}

impl Install {
    /// The `TERM` value to give the child. Always safe to use.
    pub fn term(&self) -> &str {
        match self {
            Install::Installed { term, .. } | Install::Failed { term, .. } => term,
        }
    }

    pub fn succeeded(&self) -> bool {
        matches!(self, Install::Installed { .. })
    }

    /// The directory to put on `TERMINFO`, when one was used.
    pub fn terminfo_dir(&self) -> Option<&Path> {
        match self {
            Install::Installed { terminfo_dir, .. } => Some(terminfo_dir),
            Install::Failed { .. } => None,
        }
    }
}

/// Compiles the entry into `~/.terminfo`.
///
/// Never fails outright: a terminal that refuses to open because `tic` is
/// missing is worse than one that opens as `xterm-256color`.
pub fn install() -> Install {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home {
        Some(home) => install_into(&home.join(".terminfo")),
        None => Install::Failed {
            term: FALLBACK_TERM.to_owned(),
            reason: "no $HOME to install a terminfo database into".to_owned(),
        },
    }
}

/// Compiles into a specific directory. Separated out so tests can install into
/// a temporary tree instead of the developer's own `~/.terminfo`.
pub fn install_into(terminfo_dir: &Path) -> Install {
    let fail = |reason: String| Install::Failed { term: FALLBACK_TERM.to_owned(), reason };

    if let Err(e) = std::fs::create_dir_all(terminfo_dir) {
        return fail(format!("could not create {}: {e}", terminfo_dir.display()));
    }

    let source_path = terminfo_dir.join("mica.terminfo");
    if let Err(e) = std::fs::write(&source_path, source()) {
        return fail(format!("could not write {}: {e}", source_path.display()));
    }

    // `-x` keeps the extended capabilities; without it every `Tc`, `Sync`, and
    // `Smulx` above is silently discarded and the entry becomes pointless.
    let output = Command::new("tic")
        .arg("-x")
        .arg("-o")
        .arg(terminfo_dir)
        .arg(&source_path)
        .output();

    match output {
        Ok(output) if output.status.success() => Install::Installed {
            term: TERM_NAME.to_owned(),
            terminfo_dir: terminfo_dir.to_path_buf(),
        },
        Ok(output) => fail(format!(
            "tic -x failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Err(e) => fail(format!("could not run tic: {e}")),
    }
}

/// The DA1 response: `CSI ? 62 ; 22 c`.
///
/// 62 is "VT220", 22 is "ANSI colour". Claiming more than that invites
/// applications to try things Mica does not implement.
pub const DEVICE_ATTRIBUTES: &[u8] = b"\x1b[?62;22c";

/// The light/dark reply for `CSI ? 996 n`, per the colour-scheme extension.
pub fn color_scheme_report(dark: bool) -> &'static [u8] {
    if dark {
        b"\x1b[?997;1n"
    } else {
        b"\x1b[?997;2n"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("mica-terminfo-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_source_names_the_terminal_and_inherits_from_xterm() {
        let text = source();
        assert!(text.starts_with("mica|Mica terminal,"));
        assert!(text.contains("use=xterm-256color,"));
    }

    #[test]
    fn truecolor_is_claimed_in_both_forms() {
        // `Tc` for tmux, `RGB` for ncurses. Half the ecosystem reads each.
        let text = source();
        assert!(text.contains("\tTc,"));
        assert!(text.contains("\tRGB#8,"));
    }

    #[test]
    fn backspace_is_del_and_agrees_with_the_key_encoder() {
        // These two are a contract with each other. Disagreeing is invisible
        // until somebody's backspace starts deleting forwards.
        assert!(source().contains("kbs=\\177"));
        let encoded = crate::keys::encode(
            &crate::keys::Key::Backspace,
            crate::keys::Modifiers::NONE,
            crate::keys::KeyConfig::default(),
        )
        .unwrap();
        assert_eq!(encoded, vec![0x7f], "the encoder disagrees with the terminfo entry");
    }

    #[test]
    fn every_capability_mica_cannot_honour_is_cancelled() {
        let text = source();
        for cancelled in ["Cr@", "Cs@", "initc@", "flash@", "meml@", "memu@", "smm@", "rmm@"] {
            assert!(text.contains(cancelled), "{cancelled} is not cancelled");
        }
    }

    #[test]
    fn tic_compiles_the_entry_and_infocmp_reads_it_back() {
        // The Phase 8 exit test. Not a string check: `tic` really runs, and
        // `infocmp -x` really round-trips what it produced.
        let dir = temp_dir("roundtrip");
        let install = install_into(&dir);
        assert!(install.succeeded(), "{install:?}");
        assert_eq!(install.term(), TERM_NAME);

        let output = Command::new("infocmp")
            .arg("-x")
            .arg(TERM_NAME)
            .env("TERMINFO", &dir)
            .output()
            .expect("infocmp should be installed on macOS");
        assert!(
            output.status.success(),
            "infocmp failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let described = String::from_utf8_lossy(&output.stdout);
        assert!(described.contains("Tc"), "Tc did not survive tic -x:\n{described}");
        assert!(described.contains("Sync="), "Sync did not survive:\n{described}");
        assert!(described.contains("Smulx="), "Smulx did not survive:\n{described}");
        assert!(described.contains("kbs=\\177"), "kbs is wrong:\n{described}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_compiled_entry_lands_where_terminfo_expects_it() {
        // `~/.terminfo/6d/mica` — hashed by the first letter of the name.
        let dir = temp_dir("layout");
        assert!(install_into(&dir).succeeded());
        let hashed = dir.join("6d").join("mica");
        let lettered = dir.join("m").join("mica");
        assert!(
            hashed.exists() || lettered.exists(),
            "compiled entry not found under {}",
            dir.display()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_install_falls_back_rather_than_claiming_a_term_that_does_not_exist() {
        // The load-bearing behaviour: claiming `mica` when nothing can look it
        // up drops every curses program to `dumb`.
        let install = install_into(Path::new("/dev/null/not-a-directory"));
        assert!(!install.succeeded());
        assert_eq!(install.term(), FALLBACK_TERM);
        assert!(install.terminfo_dir().is_none());
        match install {
            Install::Failed { reason, .. } => assert!(!reason.is_empty(), "no reason recorded"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn the_device_attributes_reply_claims_vt220_with_colour() {
        assert_eq!(DEVICE_ATTRIBUTES, b"\x1b[?62;22c");
    }

    #[test]
    fn the_colour_scheme_report_distinguishes_light_from_dark() {
        assert_eq!(color_scheme_report(true), b"\x1b[?997;1n");
        assert_eq!(color_scheme_report(false), b"\x1b[?997;2n");
    }
}
