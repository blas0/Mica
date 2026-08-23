//! `~/.config/mica/settings.toml`.
//!
//! One readable TOML file, hand-editable, committable, shareable, and written
//! out **in full**: every setting appears with its current value, defaults
//! included, so the file you open is the whole state of the app rather than a
//! list of the things you happened to change.
//!
//! The cost of that is real and worth naming. A file that spells out every
//! default pins today's defaults: change a default in the code and an
//! untouched install keeps the old value, because it is no longer untouched.
//! [`Settings::to_file`] still produces the differences-only shape, and is what
//! a future "reset to defaults" or an export would want.
//!
//! The on-disk shape ([`SettingsFile`], all-optional) and the resolved shape
//! ([`Settings`], no optionals) are separate types. Everything downstream reads
//! the resolved one and never has to think about absence.

use std::path::{Path, PathBuf};

use crate::motion::{MotionSettings, MotionStyle};
use serde::{Deserialize, Serialize};

use crate::material::DEFAULT_THEME;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Bell {
    Audible,
    Visual,
    Off,
}

/// Where a new pane's shell starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PaneOrigin {
    /// The working directory of the pane that was focused when it was opened.
    Inherit,
    /// Always `shell.starting-dir`, whatever the focused pane was doing.
    StartingDir,
}

/// The ambient light pass — the gradient behind everything.
///
/// Split from [`Settings`] because it is one thing with two knobs, and the
/// renderer wants both or neither.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AmbientSettings {
    pub enabled: bool,
    /// `0.1` is the setting the toggle used to mean, and it is the default.
    /// `1.0` is as loud as the pass goes.
    pub intensity: f32,
}

impl Default for AmbientSettings {
    fn default() -> AmbientSettings {
        AmbientSettings { enabled: true, intensity: 0.1 }
    }
}

impl AmbientSettings {
    /// The lowest and highest intensity a hand-edited file can ask for.
    pub const RANGE: std::ops::RangeInclusive<f32> = 0.1..=1.0;

    /// What the substrate pass should actually use.
    ///
    /// `intensity` is a dial from 0.1 to 1.0 rather than a raw shader
    /// constant, because the number in the file has to mean something to a
    /// person. `0.1` maps to `0.035`, which is what the pass was hardcoded to
    /// before it was configurable — so an untouched install looks identical.
    pub fn substrate_intensity(&self) -> f32 {
        const UNIT: f32 = 0.35;
        if !self.enabled {
            return 0.0;
        }
        self.intensity.clamp(*Self::RANGE.start(), *Self::RANGE.end()) * UNIT
    }

    fn sanitised(self) -> AmbientSettings {
        AmbientSettings {
            enabled: self.enabled,
            intensity: self.intensity.clamp(*Self::RANGE.start(), *Self::RANGE.end()),
        }
    }
}

/// What shell to run, where, and what a new pane inherits.
#[derive(Debug, Clone, PartialEq)]
pub struct ShellSettings {
    /// Absolute path to the shell. `None` means "follow `$SHELL`", falling
    /// back to [`crate::pty::ROOT_SHELL`].
    pub program: Option<String>,
    /// Where a shell starts. `None` means the home directory, which is what a
    /// login shell does on its own.
    pub starting_dir: Option<String>,
    pub new_pane_origin: PaneOrigin,
    pub focus_new_panes: bool,
}

impl Default for ShellSettings {
    fn default() -> ShellSettings {
        ShellSettings {
            program: None,
            starting_dir: None,
            new_pane_origin: PaneOrigin::Inherit,
            focus_new_panes: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SelectionSettings {
    /// Copy completed mouse selections to the pasteboard. Disabled by default
    /// so merely pointing at text never mutates global clipboard state.
    pub copy_on_select: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub theme: String,
    pub font_family: String,
    pub font_size: f32,
    pub bell: Bell,
    pub columns: u16,
    pub rows: u16,
    pub scrollback: u32,
    /// Cap the render rate below the display refresh. `0` means "follow the
    /// display", which is the only setting that can honour the zero-idle-frame
    /// design; any other value implies a timer.
    pub frame_cap: u16,
    /// Recognise progress bars in the output stream and draw them natively.
    pub substitute_progress: bool,
    pub substitute_spinner: bool,
    /// Caret physics, trails, blinking, and Reduce Motion. The system
    /// preference is layered over `motion.reduce` at runtime by the shell,
    /// which is the only layer that can see AppKit.
    pub motion: MotionSettings,
    /// The ambient light pass behind the grid.
    pub ambient: AmbientSettings,
    /// What shell to run and where.
    pub shell: ShellSettings,
    /// Mouse-selection clipboard policy.
    pub selection: SelectionSettings,
    /// Keyboard bindings that differ from the defaults, as
    /// `action id -> chord`. Stored as strings because the chord grammar
    /// belongs to the window layer: this crate must not learn what a modifier
    /// key is. An empty value means "deliberately unbound".
    pub keys: std::collections::BTreeMap<String, String>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            theme: DEFAULT_THEME.to_owned(),
            font_family: "JetBrains Mono".to_owned(),
            font_size: 13.0,
            bell: Bell::Off,
            columns: 118,
            rows: 34,
            scrollback: 10_000,
            frame_cap: 0,
            substitute_progress: false,
            substitute_spinner: false,
            motion: MotionSettings::default(),
            ambient: AmbientSettings::default(),
            shell: ShellSettings::default(),
            selection: SelectionSettings::default(),
            keys: std::collections::BTreeMap::new(),
        }
    }
}

/// The on-disk shape. Every field optional, every section optional.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<Appearance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<Window>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<Renderer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion: Option<Motion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ambient: Option<Ambient>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<Shell>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<SelectionConfig>,
    /// `action id = "chord"`. Free-form on purpose — the window layer owns the
    /// grammar and validates it, and an entry this version does not recognise
    /// is carried rather than rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Appearance {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Window {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bell: Option<Bell>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub columns: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scrollback: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Renderer {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_cap: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub substitute_progress: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub substitute_spinner: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Motion {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reduce: Option<bool>,
    /// One of the seven [`MotionStyle`] ids. An unrecognised name is ignored
    /// rather than rejected: a settings file written by a newer Mica should
    /// still open in an older one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intensity: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decay: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blink: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ambient {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intensity: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Shell {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starting_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_pane_from_focused_pane: Option<PaneOrigin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_new_panes: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct SelectionConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_on_select: Option<bool>,
}

#[derive(Debug)]
pub enum SettingsError {
    Io(std::io::Error),
    Parse(String),
    Serialize(String),
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingsError::Io(e) => write!(f, "settings io: {e}"),
            SettingsError::Parse(e) => write!(f, "settings parse: {e}"),
            SettingsError::Serialize(e) => write!(f, "settings serialize: {e}"),
        }
    }
}

impl std::error::Error for SettingsError {}

impl From<std::io::Error> for SettingsError {
    fn from(e: std::io::Error) -> SettingsError {
        SettingsError::Io(e)
    }
}

impl Settings {
    /// Applies a parsed file over the defaults.
    pub fn from_file(file: &SettingsFile) -> Settings {
        let mut s = Settings::default();
        if let Some(a) = &file.appearance {
            if let Some(v) = &a.theme {
                s.theme = v.clone();
            }
            if let Some(v) = &a.font_family {
                s.font_family = v.clone();
            }
            if let Some(v) = a.font_size {
                s.font_size = v;
            }
        }
        if let Some(w) = &file.window {
            if let Some(v) = w.bell {
                s.bell = v;
            }
            if let Some(v) = w.columns {
                s.columns = v.max(1);
            }
            if let Some(v) = w.rows {
                s.rows = v.max(1);
            }
            if let Some(v) = w.scrollback {
                s.scrollback = v;
            }
        }
        if let Some(r) = &file.renderer {
            if let Some(v) = r.frame_cap {
                s.frame_cap = v;
            }
            if let Some(v) = r.substitute_progress {
                s.substitute_progress = v;
            }
            if let Some(v) = r.substitute_spinner {
                s.substitute_spinner = v;
            }
        }
        if let Some(m) = &file.motion {
            if let Some(v) = m.reduce {
                s.motion.reduce = v;
            }
            if let Some(v) = m.cursor.as_deref().and_then(MotionStyle::from_id) {
                s.motion.style = v;
            }
            if let Some(v) = m.speed {
                s.motion.speed = v;
            }
            if let Some(v) = m.intensity {
                s.motion.intensity = v;
            }
            if let Some(v) = m.decay {
                s.motion.decay = v;
            }
            if let Some(v) = m.blink {
                s.motion.blink = v;
            }
        }
        if let Some(a) = &file.ambient {
            if let Some(v) = a.enabled {
                s.ambient.enabled = v;
            }
            if let Some(v) = a.intensity {
                s.ambient.intensity = v;
            }
        }
        if let Some(sh) = &file.shell {
            if let Some(v) = &sh.program {
                s.shell.program = (!v.is_empty()).then(|| v.clone());
            }
            if let Some(v) = &sh.starting_dir {
                s.shell.starting_dir = (!v.is_empty()).then(|| v.clone());
            }
            if let Some(v) = sh.new_pane_from_focused_pane {
                s.shell.new_pane_origin = v;
            }
            if let Some(v) = sh.focus_new_panes {
                s.shell.focus_new_panes = v;
            }
        }
        if let Some(selection) = &file.selection {
            if let Some(v) = selection.copy_on_select {
                s.selection.copy_on_select = v;
            }
        }
        if let Some(keys) = &file.keys {
            s.keys = keys.clone();
        }
        // Clamped here rather than trusted: this is the boundary a
        // hand-edited file crosses.
        s.motion = s.motion.sanitised();
        s.ambient = s.ambient.sanitised();
        s
    }

    /// The inverse: only what differs from the defaults survives.
    pub fn to_file(&self) -> SettingsFile {
        let d = Settings::default();
        let appearance = Appearance {
            theme: (self.theme != d.theme).then(|| self.theme.clone()),
            font_family: (self.font_family != d.font_family).then(|| self.font_family.clone()),
            font_size: (self.font_size != d.font_size).then_some(self.font_size),
        };
        let window = Window {
            bell: (self.bell != d.bell).then_some(self.bell),
            columns: (self.columns != d.columns).then_some(self.columns),
            rows: (self.rows != d.rows).then_some(self.rows),
            scrollback: (self.scrollback != d.scrollback).then_some(self.scrollback),
        };
        let renderer = Renderer {
            frame_cap: (self.frame_cap != d.frame_cap).then_some(self.frame_cap),
            substitute_progress: (self.substitute_progress != d.substitute_progress)
                .then_some(self.substitute_progress),
            substitute_spinner: (self.substitute_spinner != d.substitute_spinner)
                .then_some(self.substitute_spinner),
        };
        let m = &self.motion;
        let dm = &d.motion;
        let motion = Motion {
            reduce: (m.reduce != dm.reduce).then_some(m.reduce),
            cursor: (m.style != dm.style).then(|| m.style.id().to_owned()),
            speed: (m.speed != dm.speed).then_some(m.speed),
            intensity: (m.intensity != dm.intensity).then_some(m.intensity),
            decay: (m.decay != dm.decay).then_some(m.decay),
            blink: (m.blink != dm.blink).then_some(m.blink),
        };

        let ambient = Ambient {
            enabled: (self.ambient.enabled != d.ambient.enabled).then_some(self.ambient.enabled),
            intensity: (self.ambient.intensity != d.ambient.intensity)
                .then_some(self.ambient.intensity),
        };
        let shell = Shell {
            program: (self.shell.program != d.shell.program).then(|| {
                self.shell.program.clone().unwrap_or_default()
            }),
            starting_dir: (self.shell.starting_dir != d.shell.starting_dir).then(|| {
                self.shell.starting_dir.clone().unwrap_or_default()
            }),
            new_pane_from_focused_pane: (self.shell.new_pane_origin != d.shell.new_pane_origin)
                .then_some(self.shell.new_pane_origin),
            focus_new_panes: (self.shell.focus_new_panes != d.shell.focus_new_panes)
                .then_some(self.shell.focus_new_panes),
        };
        let selection = SelectionConfig {
            copy_on_select: (self.selection.copy_on_select != d.selection.copy_on_select)
                .then_some(self.selection.copy_on_select),
        };

        SettingsFile {
            appearance: (appearance != Appearance::default()).then_some(appearance),
            window: (window != Window::default()).then_some(window),
            renderer: (renderer != Renderer::default()).then_some(renderer),
            motion: (motion != Motion::default()).then_some(motion),
            ambient: (ambient != Ambient::default()).then_some(ambient),
            shell: (shell != Shell::default()).then_some(shell),
            selection: (selection != SelectionConfig::default()).then_some(selection),
            keys: (!self.keys.is_empty()).then(|| self.keys.clone()),
        }
    }

    /// Every setting, present. This is the shape written to disk.
    ///
    /// The two `Option` fields in `[shell]` have no fixed default to print —
    /// the shell follows `$SHELL` and the directory follows `$HOME`, both
    /// resolved at launch. They are written as an empty string, which
    /// [`Settings::from_file`] reads back as unset, so the round trip holds and
    /// the key is still there to edit.
    pub fn to_file_complete(&self) -> SettingsFile {
        SettingsFile {
            appearance: Some(Appearance {
                theme: Some(self.theme.clone()),
                font_family: Some(self.font_family.clone()),
                font_size: Some(self.font_size),
            }),
            window: Some(Window {
                bell: Some(self.bell),
                columns: Some(self.columns),
                rows: Some(self.rows),
                scrollback: Some(self.scrollback),
            }),
            renderer: Some(Renderer {
                frame_cap: Some(self.frame_cap),
                substitute_progress: Some(self.substitute_progress),
                substitute_spinner: Some(self.substitute_spinner),
            }),
            motion: Some(Motion {
                reduce: Some(self.motion.reduce),
                cursor: Some(self.motion.style.id().to_owned()),
                speed: Some(self.motion.speed),
                intensity: Some(self.motion.intensity),
                decay: Some(self.motion.decay),
                blink: Some(self.motion.blink),
            }),
            ambient: Some(Ambient {
                enabled: Some(self.ambient.enabled),
                intensity: Some(self.ambient.intensity),
            }),
            shell: Some(Shell {
                program: Some(self.shell.program.clone().unwrap_or_default()),
                starting_dir: Some(self.shell.starting_dir.clone().unwrap_or_default()),
                new_pane_from_focused_pane: Some(self.shell.new_pane_origin),
                focus_new_panes: Some(self.shell.focus_new_panes),
            }),
            selection: Some(SelectionConfig {
                copy_on_select: Some(self.selection.copy_on_select),
            }),
            keys: Some(self.keys.clone()),
        }
    }

    pub fn parse(text: &str) -> Result<Settings, SettingsError> {
        let file: SettingsFile =
            toml::from_str(text).map_err(|e| SettingsError::Parse(e.to_string()))?;
        Ok(Settings::from_file(&file))
    }

    /// The differences-only shape, as TOML. Not what gets written to disk —
    /// see [`crate::reference::document`] for that.
    pub fn serialize(&self) -> Result<String, SettingsError> {
        toml::to_string_pretty(&self.to_file())
            .map_err(|e| SettingsError::Serialize(e.to_string()))
    }

    /// Every setting, as TOML. The body of the `Config` section.
    pub fn serialize_complete(&self) -> Result<String, SettingsError> {
        toml::to_string_pretty(&self.to_file_complete())
            .map_err(|e| SettingsError::Serialize(e.to_string()))
    }

    /// Reads the file, falling back to defaults when it does not exist.
    ///
    /// A *malformed* file is an error rather than a silent fallback — losing a
    /// user's theme because of a stray bracket, with no explanation, is worse
    /// than refusing to start with a message that names the line.
    pub fn load(path: &Path) -> Result<Settings, SettingsError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Settings::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(SettingsError::Io(e)),
        }
    }

    /// Writes the documented file — the catalogue of every flag, then the
    /// live configuration. See [`crate::reference`].
    ///
    /// `keys` is the bindable-action catalogue, which this crate cannot build
    /// on its own: the chord grammar belongs to the window layer.
    ///
    /// There is deliberately only one writer of this file. A second one that
    /// wrote bare TOML would silently delete the catalogue the first time a
    /// setting changed.
    pub fn save(&self, path: &Path, keys: &[crate::reference::KeyDoc]) -> Result<(), SettingsError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Write-then-rename: a crash mid-write must not leave the user with a
        // truncated settings file.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, crate::reference::document(self, keys)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// `$XDG_CONFIG_HOME/mica/settings.toml`, else `~/.config/mica/settings.toml`.
pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("mica").join("settings.toml")
}

/// Polls a file's modification time so an external edit hot-reloads.
///
/// Polling rather than FSEvents on purpose: one `stat` a second against one
/// path is unmeasurable, and it keeps a filesystem-notification dependency —
/// plus its background thread and its editor-specific write-then-rename
/// quirks — out of a crate that is supposed to have almost no dependencies.
#[derive(Debug)]
pub struct SettingsWatcher {
    path: PathBuf,
    last_modified: Option<std::time::SystemTime>,
}

impl SettingsWatcher {
    pub fn new(path: PathBuf) -> SettingsWatcher {
        let last_modified = modified_time(&path);
        SettingsWatcher { path, last_modified }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the new settings if the file changed since the last poll.
    ///
    /// A parse failure is reported and the *previous* settings stay in force —
    /// a half-saved file from an editor must not blank the user's theme.
    pub fn poll(&mut self) -> Option<Result<Settings, SettingsError>> {
        let current = modified_time(&self.path);
        if current == self.last_modified {
            return None;
        }
        self.last_modified = current;
        Some(Settings::load(&self.path))
    }

    /// Reads the file whatever its mtime says, for an explicit reload.
    ///
    /// [`poll`](Self::poll) answering `None` means "nothing changed", which is
    /// the right answer for a background check and the wrong one for a key the
    /// user deliberately pressed: *nothing happened* and *nothing needed to
    /// happen* must not look the same when somebody asked. This also
    /// resynchronises the watcher, so the next activation does not then apply
    /// the same file a second time.
    pub fn reread(&mut self) -> Result<Settings, SettingsError> {
        self.last_modified = modified_time(&self.path);
        Settings::load(&self.path)
    }
}

fn modified_time(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_yields_defaults() {
        let s = Settings::load(Path::new("/nonexistent/mica/settings.toml")).unwrap();
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn defaults_serialize_to_an_empty_file() {
        // The whole point: an untouched configuration writes nothing at all.
        assert_eq!(Settings::default().serialize().unwrap().trim(), "");
    }

    #[test]
    fn only_changed_values_are_written() {
        let mut s = Settings::default();
        s.theme = "basalt".into();
        s.substitute_spinner = true;

        let text = s.serialize().unwrap();
        assert!(text.contains("theme = \"basalt\""), "{text}");
        assert!(text.contains("substitute_spinner = true"), "{text}");
        assert!(!text.contains("font_size"), "untouched settings must not be written:\n{text}");
        assert!(!text.contains("[motion]"), "an all-default section must be omitted:\n{text}");
    }

    #[test]
    fn a_settings_file_round_trips() {
        let mut s = Settings::default();
        s.theme = "quartz".into();
        s.bell = Bell::Audible;
        s.font_size = 15.5;
        s.frame_cap = 60;
        s.motion.reduce = true;
        s.selection.copy_on_select = true;

        let text = s.serialize().unwrap();
        assert_eq!(Settings::parse(&text).unwrap(), s);
        assert!(text.contains("[selection]"));
        assert!(text.contains("copy-on-select = true"));
    }

    #[test]
    fn key_bindings_round_trip_and_only_appear_when_set() {
        let mut s = Settings::default();
        assert!(!s.serialize().unwrap().contains("[keys]"), "an empty keymap was written out");

        s.keys.insert("find.toggle".into(), "cmd+j".into());
        // An empty value is how a deliberately unbound action is recorded, and
        // it has to survive the round trip or the removal is silently undone.
        s.keys.insert("find.toggle".into(), String::new());

        let text = s.serialize().unwrap();
        assert!(text.contains("[keys]"), "{text}");
        assert_eq!(Settings::parse(&text).unwrap(), s);
    }

    #[test]
    fn an_unrecognised_binding_is_carried_rather_than_rejected() {
        // This crate does not know the chord grammar, so it must not have an
        // opinion about which ones are valid.
        let s = Settings::parse("[keys]\n\"future.action\" = \"hyper+q\"\n").unwrap();
        assert_eq!(s.keys.get("future.action").map(String::as_str), Some("hyper+q"));
    }

    #[test]
    fn every_motion_setting_round_trips() {
        let mut s = Settings::default();
        s.motion = MotionSettings {
            style: MotionStyle::Phosphor,
            speed: 1.75,
            intensity: 0.4,
            decay: false,
            blink: true,
            reduce: true,
        };
        let text = s.serialize().unwrap();
        assert!(text.contains("cursor = \"phosphor\""), "the style is not in the file:\n{text}");
        assert_eq!(Settings::parse(&text).unwrap(), s);
    }

    #[test]
    fn an_unknown_motion_style_falls_back_rather_than_failing_to_load() {
        // A file written by a newer Mica must still open in an older one. The
        // alternative is a terminal that will not start because of a caret.
        let s = Settings::parse("[motion]\ncursor = \"tesseract\"\n").unwrap();
        assert_eq!(s.motion.style, MotionSettings::default().style);
    }

    #[test]
    fn a_hand_edited_file_cannot_produce_a_caret_that_never_arrives() {
        let s = Settings::parse("[motion]\nspeed = 0.0\nintensity = 99.0\n").unwrap();
        assert!(s.motion.speed >= 0.25, "speed survived as {}", s.motion.speed);
        assert_eq!(s.motion.intensity, 1.0);
    }

    #[test]
    fn the_reference_shaped_file_parses() {
        let s = Settings::parse(
            r#"
[appearance]
theme = "basalt"

[window]
bell = "audible"

[renderer]
substitute_progress = false
substitute_spinner = true
"#,
        )
        .unwrap();
        assert_eq!(s.theme, "basalt");
        assert_eq!(s.bell, Bell::Audible);
        assert!(!s.substitute_progress);
        assert!(s.substitute_spinner);
        // Everything unmentioned keeps its default.
        assert_eq!(s.scrollback, Settings::default().scrollback);
    }

    #[test]
    fn a_typo_is_an_error_rather_than_a_silent_default() {
        let err = Settings::parse("[appearance]\nthme = \"basalt\"\n").unwrap_err();
        assert!(matches!(err, SettingsError::Parse(_)), "{err:?}");
    }

    #[test]
    fn zero_columns_is_clamped_rather_than_accepted() {
        let s = Settings::parse("[window]\ncolumns = 0\n").unwrap();
        assert_eq!(s.columns, 1);
    }

    #[test]
    fn saving_and_loading_a_real_file_round_trips() {
        let dir = std::env::temp_dir().join(format!("mica-settings-{}", std::process::id()));
        let path = dir.join("settings.toml");
        let mut s = Settings::default();
        s.theme = "quartz".into();

        s.save(&path, &[]).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rereading_returns_the_file_even_when_the_mtime_has_not_moved() {
        // The difference between a background poll and a key the user pressed:
        // "nothing changed" is a fine answer to the first and a useless one to
        // the second.
        let dir = std::env::temp_dir().join(format!("mica-reread-{}", std::process::id()));
        let path = dir.join("settings.toml");
        let s = Settings { theme: "quartz".into(), ..Settings::default() };
        s.save(&path, &[]).unwrap();

        let mut watcher = SettingsWatcher::new(path.clone());
        assert!(watcher.poll().is_none(), "a file that has not moved has nothing to report");
        assert_eq!(watcher.reread().unwrap().theme, "quartz");
        // And the manual read resynchronised the watcher, so the next
        // activation does not apply the same file a second time.
        assert!(watcher.poll().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
