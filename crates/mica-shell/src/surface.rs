//! One terminal surface: a session, an atlas, a renderer, and a theme.
//!
//! Deliberately free of AppKit. The window layer owns a `Surface` and calls
//! into it; the surface never calls back out. That means the entire
//! session-to-pixels path can be driven from a test with no window, which is
//! what [`Surface::render_to_texture`] exists for.

use std::path::{Path, PathBuf};
use std::time::Duration;

use mica_atlas::atlas::Atlas;
use mica_atlas::fontset::FontSet;
use mica_core::backend::CursorShape;
use mica_core::material::{builtin, Material, Role};
use mica_core::motion::{Caret, Crossfade, MotionSettings};
use mica_core::pty::PtyConfig;
use mica_core::session::{Session, SessionEvent};
use mica_core::settings::Settings;
use mica_gpu::frame::Reason;
use mica_gpu::grid::{
    block_gutters, caret_decay, cursor_shape, Rgba, RowBuilder, SubstrateUniforms, Uniforms,
};
use mica_gpu::overlay::find::Find;
use mica_gpu::overlay::palette::{actions_with_themes, Action, Palette};
use mica_gpu::overlay::shortcuts::{self, ShortcutRow, ShortcutView};
use mica_gpu::overlay::OverlayMetrics;
use mica_gpu::renderer::Renderer;

use crate::bindings::{Bindings, Chord};
use crate::pane::{CellRect, Direction, Layout, PaneId, DIVIDER};
use crate::integration::{self, Integration, Shell};
use crate::keys::{Key, Modifiers};
use crate::shortcut_panel::{Handled, ShortcutPanel};
use crate::terminfo;

/// One shell, and everything that belongs to it alone.
///
/// A caret per pane, not per window: two panes both have a cursor, both are
/// animating it, and only one of them is focused. Sharing one would make the
/// unfocused pane's cursor chase the focused pane's.
struct Pane {
    id: PaneId,
    session: Session,
    caret: Caret,
    /// Where the layout last put it, in cells. Cached so a frame does not
    /// re-walk the tree per pane, and so a resize can tell whether the pane's
    /// grid actually changed.
    rect: CellRect,
    title: String,
}

/// A pane arriving.
struct SplitMotion {
    pane: PaneId,
    fade: Crossfade,
}

pub struct Surface {
    /// Every open shell in this window, in no particular order — [`Layout`]
    /// decides where they are, and it addresses them by [`PaneId`].
    panes: Vec<Pane>,
    layout: Layout,
    focus: PaneId,
    /// Ids are never reused. A pane that closes takes its id with it, so a
    /// stale reference resolves to nothing rather than to a different shell.
    next_pane: u32,
    /// The environment every pane's shell inherits: terminfo, `TERM_PROGRAM`,
    /// and the shell integration. Built once at open, because a new pane must
    /// come up in the same world as the first one.
    spawn_env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    /// How a pane tells the window layer it has output. Kept because a pane
    /// opened later has to be woken the same way as the first one — a split
    /// whose shell only redraws when you touch the trackpad is the bug this
    /// field exists to prevent.
    wakeup: Option<mica_core::pty::Wakeup>,
    atlas: Atlas,
    renderer: Renderer,
    /// The theme being switched *to*. What is actually drawn is
    /// `display_material`, which is this blended with the previous theme while
    /// a cross-fade is running.
    material: Material,
    /// The theme being switched *from*, for the duration of a cross-fade.
    previous_material: Option<Material>,
    /// What the renderer reads. Recomputed only while fading.
    display_material: Material,
    crossfade: Crossfade,
    /// A pane that has just opened, and how far through arriving it is.
    ///
    /// The rectangle itself does not animate — that would mean resizing a PTY
    /// every frame, and a shell being told its size sixty times in a fifth of
    /// a second is a shell that spends the whole split redrawing. What
    /// animates is the *arrival*: the new pane fades up and its divider draws
    /// itself out from the split. Bounded, so the animation count returns to
    /// zero and the terminal goes silent again.
    split_motion: Option<SplitMotion>,
    motion: MotionSettings,
    /// Whether an animation is currently registered with the frame scheduler.
    ///
    /// A `bool` mirroring a counter, so that registration is idempotent: the
    /// scheduler's count has to return to zero for the terminal to go silent
    /// again, and the surest way to leak a count is to call `begin_animation`
    /// twice for one animation.
    animating: bool,
    settings: Settings,
    /// Kept alive for the lifetime of the session: the generated ZDOTDIR is
    /// deleted when this is dropped.
    _integration: Integration,
    scale: f32,
    viewport: (u32, u32),
    focused: bool,
    title: String,
    palette: Palette,
    find: Find,
    /// The chord table the window dispatches through, and the panel that
    /// edits it.
    bindings: Bindings,
    shortcuts: ShortcutPanel,
    /// Where [`Surface::persist`] writes, and what `⌘,` opens.
    ///
    /// A field rather than a call to [`crate::config::path`] at each use, so a
    /// test can point a surface at a temporary directory. Without it, running
    /// the suite rewrote the developer's own settings file — which it did,
    /// once, before this existed.
    settings_path: PathBuf,
}

impl std::fmt::Debug for Surface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Surface")
            .field("viewport", &self.viewport)
            .field("scale", &self.scale)
            .field("grid", &self.sess().dimensions())
            .field("panes", &self.panes.len())
            .field("blocks", &self.sess().blocks().len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum SurfaceError {
    Gpu(mica_gpu::context::GpuError),
    Io(std::io::Error),
    Theme(mica_core::material::ThemeError),
}

impl std::fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SurfaceError::Gpu(e) => write!(f, "{e}"),
            SurfaceError::Io(e) => write!(f, "{e}"),
            SurfaceError::Theme(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SurfaceError {}

impl From<mica_gpu::context::GpuError> for SurfaceError {
    fn from(e: mica_gpu::context::GpuError) -> SurfaceError {
        SurfaceError::Gpu(e)
    }
}

impl From<std::io::Error> for SurfaceError {
    fn from(e: std::io::Error) -> SurfaceError {
        SurfaceError::Io(e)
    }
}

impl Surface {
    /// Opens a surface: installs terminfo, generates shell integration, spawns
    /// the shell, and builds the renderer.
    ///
    /// `wakeup` is fired by the PTY reader thread when output arrives. It has
    /// to be supplied **here**, because the reader thread is started inside
    /// `Session::spawn` — an earlier version installed it afterwards by
    /// replacing the session, which spawned the user's shell twice and ran
    /// their rc files twice with it.
    pub fn open(
        settings: Settings,
        viewport: (u32, u32),
        scale: f32,
        integration_root: PathBuf,
        wakeup: Option<mica_core::pty::Wakeup>,
    ) -> Result<Surface, SurfaceError> {
        let fonts = FontSet::resolve(&settings.font_family, settings.font_size, scale);
        let metrics = fonts.metrics();
        let atlas = Atlas::new(fonts);
        let renderer = Renderer::new(atlas.page_size())?;

        let (cols, rows) = grid_size(viewport, metrics.width, metrics.height);

        let mut config = PtyConfig::for_shell_settings(cols, rows, &settings.shell);
        config.cols = cols;
        config.rows = rows;

        let shell = Shell::detect(&config.program);
        let integration = integration::generate(shell, &integration_root)?;

        // TERM is decided by whether `tic -x` actually succeeded — never
        // assumed. See `terminfo::install`.
        let install = terminfo::install();
        config.env.push(("TERM".into(), install.term().into()));
        if let Some(dir) = install.terminfo_dir() {
            config.env.push(("TERMINFO".into(), dir.as_os_str().to_owned()));
        }
        config.env.push(("COLORTERM".into(), "truecolor".into()));
        config.env.push(("TERM_PROGRAM".into(), mica_core::TERM_PROGRAM.into()));
        config.env.push(("TERM_PROGRAM_VERSION".into(), mica_core::VERSION.into()));
        for (key, value) in integration.env() {
            config.env.push((key.into(), value.into()));
        }

        let material = Material::from_theme(
            &builtin(&settings.theme)
                .unwrap_or_else(|| builtin(mica_core::material::DEFAULT_THEME).unwrap()),
        )
        .map_err(SurfaceError::Theme)?;

        let spawn_env = config.env.clone();
        let session = Session::spawn_with_wakeup(&settings, config, wakeup.clone())?;

        let motion = settings.motion.sanitised();
        let bindings = Bindings::from_overrides(&settings.keys);
        let root = PaneId(0);
        let mut surface = Surface {
            panes: vec![Pane {
                id: root,
                session,
                caret: Caret::new(0.0, 0.0),
                rect: CellRect::new(0, 0, cols, rows),
                title: String::from("Mica"),
            }],
            layout: Layout::new(root),
            focus: root,
            next_pane: 1,
            spawn_env,
            wakeup,
            atlas,
            renderer,
            display_material: material.clone(),
            previous_material: None,
            material,
            crossfade: Crossfade::done(),
            split_motion: None,
            motion,
            animating: false,
            settings,
            _integration: integration,
            scale,
            viewport,
            focused: true,
            title: String::from("Mica"),
            palette: Palette::new(actions_with_themes(&palette_actions(), &theme_ids())),
            find: Find::new(),
            bindings,
            shortcuts: ShortcutPanel::new(),
            settings_path: crate::config::path(),
        };
        // The palette prints accelerators from the table, so it has to be told
        // once at construction as well as after every edit — otherwise every
        // shortcut reads as unbound until the user opens the panel.
        surface.refresh_palette_accelerators();
        Ok(surface)
    }

    /// The focused pane. Every unqualified "the session" in this file means
    /// this one: input goes here, the caret drawn brightest is this one's, and
    /// a split divides this one.
    fn pane(&self) -> &Pane {
        self.panes
            .iter()
            .find(|p| p.id == self.focus)
            .unwrap_or_else(|| &self.panes[0])
    }

    fn pane_mut(&mut self) -> &mut Pane {
        let index = self.panes.iter().position(|p| p.id == self.focus).unwrap_or(0);
        &mut self.panes[index]
    }

    fn sess(&self) -> &Session {
        &self.pane().session
    }

    fn sess_mut(&mut self) -> &mut Session {
        &mut self.pane_mut().session
    }

    /// A pane's session by position, for tests that have to watch more than
    /// the focused one.
    ///
    /// Ordered by when the pane opened, which is not the order they appear on
    /// screen — [`Layout`] owns that, and a test that wants a rectangle should
    /// ask for one.
    pub fn pane_session(&mut self, index: usize) -> &mut Session {
        &mut self.panes[index].session
    }

    /// How many panes are open. One is the ordinary case.
    pub fn pane_count(&self) -> usize {
        self.panes.len()
    }

    /// Redirects where settings are written. Tests only — the app uses
    /// [`crate::config::path`], which is where a user expects to find it.
    pub fn set_settings_path(&mut self, path: PathBuf) {
        self.settings_path = path;
    }

    pub fn settings_path(&self) -> &Path {
        &self.settings_path
    }

    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    pub fn find(&self) -> &Find {
        &self.find
    }

    /// True while an overlay owns the keyboard. The window layer checks this
    /// before encoding a keystroke for the shell — otherwise typing a query
    /// would also run it.
    pub fn overlay_has_focus(&self) -> bool {
        self.palette.is_open() || self.find.is_open() || self.shortcuts.is_open()
    }

    pub fn bindings(&self) -> &Bindings {
        &self.bindings
    }

    pub fn shortcuts(&self) -> &ShortcutPanel {
        &self.shortcuts
    }

    pub fn toggle_shortcuts(&mut self) {
        if self.shortcuts.is_open() {
            self.shortcuts.close();
        } else {
            self.palette.close();
            self.find.close();
            self.shortcuts.open();
        }
        self.overlay_changed();
    }

    /// Routes a key to the shortcut panel. Returns whether it was consumed.
    ///
    /// **While capturing, everything is consumed.** That is what lets `⎋` and
    /// the arrow keys be bound at all: a panel that keeps its own meaning for
    /// those keys while waiting for a combination is a panel that cannot bind
    /// them.
    pub fn shortcut_key(&mut self, key: Key, modifiers: Modifiers) -> bool {
        if !self.shortcuts.is_open() {
            return false;
        }
        let handled = if self.shortcuts.is_capturing() {
            self.shortcuts.capture(Chord::new(modifiers, key), &mut self.bindings)
        } else {
            match key {
                Key::Escape => self.shortcuts.escape(),
                Key::Up => self.shortcuts.move_selection(-1),
                Key::Down => self.shortcuts.move_selection(1),
                Key::PageUp | Key::Home => self.shortcuts.move_selection(-100),
                Key::PageDown | Key::End => self.shortcuts.move_selection(100),
                Key::Char(' ') | Key::Enter => self.shortcuts.begin_capture(),
                Key::Backspace | Key::Delete => self.shortcuts.unbind(&mut self.bindings),
                _ => Handled::Ignored,
            }
        };

        if handled == Handled::Changed {
            self.persist_bindings();
        }
        if handled != Handled::Ignored {
            self.refresh_palette_accelerators();
            self.overlay_changed();
        }
        // Consumed regardless: an open panel owns the keyboard, and letting an
        // unrecognised key fall through would type it into the shell behind.
        true
    }

    /// Writes the changed bindings to `settings.toml`.
    ///
    /// A failure is reported and otherwise ignored: the binding is already
    /// live in this session, and refusing to rebind because the disk is full
    /// would be a worse answer than rebinding and saying it did not stick.
    fn persist_bindings(&mut self) {
        self.settings.keys = self.bindings.overrides();
        self.persist();
    }

    /// Writes the whole settings document — catalogue and configuration.
    fn persist(&mut self) {
        let path = self.settings_path.clone();
        let keys = crate::config::key_docs(&self.bindings);
        if let Err(error) = self.settings.save(&path, &keys) {
            eprintln!("mica: could not save {}: {error}", path.display());
        }
    }

    /// Keeps the accelerators the palette prints in step with the table.
    ///
    /// The palette used to carry hardcoded strings, and advertised `⌘↓` for
    /// two different actions and `⌘↑` for one that had no binding at all.
    /// Deriving them means they cannot drift again.
    fn refresh_palette_accelerators(&mut self) {
        let bindings = &self.bindings;
        self.palette.set_accelerators(&|id| {
            bindings.chord_for(id).map(Chord::to_display).unwrap_or_default()
        });
    }

    pub fn toggle_palette(&mut self) {
        if self.palette.is_open() {
            self.palette.close();
        } else {
            self.find.close();
            // Rebuild, then reapply the accelerators. `set_actions` drops them
            // on purpose — they are derived, and a palette that kept stale
            // ones is how it came to advertise shortcuts that did nothing.
            self.palette.set_actions(&palette_actions(), &theme_ids());
            self.refresh_palette_accelerators();
            self.palette.open();
        }
        self.overlay_changed();
    }

    pub fn toggle_find(&mut self) {
        if self.find.is_open() {
            self.find.close();
        } else {
            self.palette.close();
            self.find.open();
            self.refresh_search();
        }
        self.overlay_changed();
    }

    pub fn close_overlays(&mut self) -> bool {
        if !self.overlay_has_focus() {
            return false;
        }
        self.palette.close();
        self.find.close();
        self.shortcuts.close();
        self.overlay_changed();
        true
    }

    fn overlay_changed(&mut self) {
        // An overlay is drawn over the grid, so the rows beneath it have to be
        // repainted when it appears or disappears.
        self.sess_mut().damage_all();
        self.renderer.scheduler().request(Reason::Overlay);
    }

    /// Runs the search over the whole scrollback.
    ///
    /// Deliberately **not** called from the render path: it reads every
    /// retained line. It runs when the query changes, which is a keystroke, not
    /// a frame.
    fn refresh_search(&mut self) {
        if !self.find.is_open() {
            return;
        }
        let (top, bottom) = self.sess_mut().line_bounds();
        let lines: Vec<(i32, String)> = (top..=bottom)
            .filter_map(|line| self.sess_mut().line_text(line).map(|text| (line, text)))
            .collect();
        let focus = self.sess_mut().cursor().line as i32;
        self.find.run(lines.iter().map(|(l, t)| (*l, t.as_str())), focus);
    }

    /// Feeds a character to whichever overlay has focus.
    pub fn overlay_char(&mut self, ch: char) -> bool {
        let changed = if self.palette.is_open() {
            self.palette.type_char(ch)
        } else if self.find.is_open() {
            let changed = self.find.type_char(ch);
            if changed {
                self.refresh_search();
            }
            changed
        } else {
            false
        };
        if changed {
            self.overlay_changed();
        }
        changed
    }

    pub fn overlay_backspace(&mut self) -> bool {
        let changed = if self.palette.is_open() {
            self.palette.backspace()
        } else if self.find.is_open() {
            let changed = self.find.backspace();
            if changed {
                self.refresh_search();
            }
            changed
        } else {
            false
        };
        if changed {
            self.overlay_changed();
        }
        changed
    }

    /// Moves the selection, or steps between matches.
    pub fn overlay_step(&mut self, forward: bool) -> bool {
        let changed = if self.palette.is_open() {
            if forward { self.palette.select_next() } else { self.palette.select_previous() }
        } else if self.find.is_open() {
            let target = if forward { self.find.next() } else { self.find.previous() };
            if let Some(m) = target {
                self.scroll_to_line(m.line);
                true
            } else {
                false
            }
        } else {
            false
        };
        if changed {
            self.overlay_changed();
        }
        changed
    }

    /// Brings an absolute line into view.
    fn scroll_to_line(&mut self, line: i32) {
        let (_, rows) = self.sess_mut().dimensions();
        // Centre it rather than putting it at an edge: a match at the very top
        // of the window has no context above it.
        let delta = -(line - rows as i32 / 2);
        if delta != 0 {
            self.sess_mut().scroll(delta);
        }
    }

    /// Accepts the overlay's current entry. Returns an action id if one ran.
    pub fn overlay_accept(&mut self) -> Option<String> {
        if self.palette.is_open() {
            let id = self.palette.accept();
            self.overlay_changed();
            return id;
        }
        if self.find.is_open() {
            self.overlay_step(true);
            return None;
        }
        None
    }

    /// Runs a palette action. Returns whether it was recognised.
    pub fn dispatch(&mut self, id: &str) -> bool {
        if let Some(theme) = id.strip_prefix("theme.") {
            return self.set_theme(theme);
        }
        // A `text:` binding types for the user. Deliberately not wrapped in
        // bracketed paste: that sequence exists to tell a program "this arrived
        // from a clipboard, do not run it", and a chord the user bound is the
        // other kind — it is a keystroke they configured. Wrapping it would
        // also make an escape-prefixed binding unusable, which is half the
        // point of the feature.
        if let Some(text) = crate::bindings::text_payload(id) {
            self.write_input(text.as_bytes());
            return true;
        }
        match id {
            "session.scroll_bottom" => {
                self.sess_mut().scroll_to_bottom();
                self.renderer.scheduler().request(Reason::Damage);
                true
            }
            "session.clear_selection" => {
                self.sess_mut().set_selection(None);
                self.renderer.scheduler().request(Reason::Selection);
                true
            }
            "blocks.next" | "blocks.previous" => {
                let row = self.sess_mut().cursor().line as u64;
                let target = if id.ends_with("next") {
                    self.sess_mut().block_tracker_mut().next_block_after(row).map(|b| b.start_row)
                } else {
                    self.sess_mut()
                        .block_tracker_mut()
                        .previous_block_before(row)
                        .map(|b| b.start_row)
                };
                if let Some(row) = target {
                    self.scroll_to_line(row as i32);
                    self.renderer.scheduler().request(Reason::Damage);
                }
                true
            }
            "palette.toggle" => {
                self.toggle_palette();
                true
            }
            "find.toggle" => {
                self.toggle_find();
                true
            }
            "find.next" => self.overlay_step(true),
            "find.previous" => self.overlay_step(false),
            "keys.open" => {
                self.toggle_shortcuts();
                true
            }
            "settings.open" => self.open_settings_file(),
            "session.scroll_top" => {
                // `history_len` is the whole buffer; scrolling by it saturates
                // at the oldest line rather than needing to be exact.
                let top = self.sess_mut().history_len() as i32;
                self.scroll(top);
                true
            }
            "settings.fx.ambient" => {
                self.settings.ambient.enabled = !self.settings.ambient.enabled;
                self.persist();
                self.renderer.scheduler().request(Reason::Damage);
                self.sess_mut().damage_all();
                true
            }
            "settings.fx.cursor" => {
                // Cycles rather than toggles: with seven styles, a toggle
                // would only ever reach two of them.
                let styles = mica_core::motion::MotionStyle::ALL;
                let next = styles
                    .iter()
                    .position(|s| *s == self.motion.style)
                    .map(|i| styles[(i + 1) % styles.len()])
                    .unwrap_or_default();
                self.set_motion(MotionSettings { style: next, ..self.motion });
                true
            }
            "settings.fx.decay" => {
                self.set_motion(MotionSettings { decay: !self.motion.decay, ..self.motion });
                true
            }
            "settings.fx.blink" => {
                self.set_motion(MotionSettings { blink: !self.motion.blink, ..self.motion });
                true
            }
            "settings.fx.reduce" => {
                self.set_motion(MotionSettings { reduce: !self.motion.reduce, ..self.motion });
                true
            }
            "pane.split_right" => self.split(Direction::Right),
            "pane.split_left" => self.split(Direction::Left),
            "pane.split_down" => self.split(Direction::Down),
            "pane.split_up" => self.split(Direction::Up),
            "pane.focus_right" => self.focus_direction(Direction::Right),
            "pane.focus_left" => self.focus_direction(Direction::Left),
            "pane.focus_down" => self.focus_direction(Direction::Down),
            "pane.focus_up" => self.focus_direction(Direction::Up),
            "pane.close" => {
                let focus = self.focus;
                self.close_pane(focus)
            }
            // Recognised but not implemented in v0.1. Returning false is the
            // honest answer — it lets the caller say so rather than silently
            // doing nothing.
            _ => false,
        }
    }

    /// Opens the settings file in the user's text editor, writing it first if
    /// it is not there yet.
    ///
    /// Failure is reported rather than swallowed: `⌘,` doing nothing at all,
    /// with no message anywhere, is the worst version of this.
    fn open_settings_file(&mut self) -> bool {
        let path = self.settings_path.clone();
        let keys = crate::config::key_docs(&self.bindings);
        if let Err(error) = crate::config::refresh(&path, &self.settings, &keys) {
            eprintln!("mica: could not write {}: {error}", path.display());
            return true;
        }
        if let Err(error) = crate::config::reveal(&path) {
            eprintln!("mica: could not open {}: {error}", path.display());
        }
        true
    }

    pub fn selection_text(&self) -> Option<String> {
        self.sess().selection_text()
    }

    pub fn session(&mut self) -> &mut Session {
        self.sess_mut()
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn has_exited(&self) -> bool {
        self.panes.iter().all(|p| p.session.has_exited())
    }

    pub fn set_focused(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            self.renderer.scheduler().request(Reason::Focus);
        }
    }

    /// Reads whatever the PTY produced and schedules a frame if anything
    /// changed. Returns the events the window layer has to act on.
    pub fn pump(&mut self) -> Vec<SessionEvent> {
        // Every pane, not just the focused one. A background pane still has a
        // shell in it, and a build running in a pane you are not looking at
        // has to keep printing.
        let mut damaged = false;
        let focus = self.focus;
        let mut events = Vec::new();
        let mut focus_title = None;
        for pane in &mut self.panes {
            if pane.session.pump() && pane.session.has_damage() {
                damaged = true;
            }
            for event in pane.session.drain_events() {
                if let SessionEvent::TitleChanged(title) = &event {
                    pane.title = if title.is_empty() { "Mica".into() } else { title.clone() };
                    if pane.id == focus {
                        focus_title = Some(pane.title.clone());
                    }
                }
                // Only the focused pane's events reach the window layer. A
                // background pane ringing the bell or renaming itself is not
                // the window's business, and letting those through would let
                // an unfocused shell retitle the window.
                if pane.id == focus {
                    events.push(event);
                }
            }
        }
        if let Some(title) = focus_title {
            self.title = title;
        }
        if damaged {
            self.renderer.scheduler().request(Reason::Damage);
        }
        self.close_exited_panes();
        events
    }

    /// Reaps panes whose shell has exited.
    ///
    /// A pane whose shell is gone is a rectangle of dead text. The last one is
    /// left alone: that is the window closing, and [`Surface::has_exited`] is
    /// what says so.
    fn close_exited_panes(&mut self) {
        if self.panes.len() < 2 {
            return;
        }
        let gone: Vec<PaneId> =
            self.panes.iter().filter(|p| p.session.has_exited()).map(|p| p.id).collect();
        for id in gone {
            if self.panes.len() < 2 {
                break;
            }
            self.close_pane(id);
        }
    }

    pub fn write_input(&mut self, bytes: &[u8]) {
        let _ = self.sess_mut().write_input(bytes);
    }

    /// One scroll gesture, already converted to whole lines.
    ///
    /// Positive is toward history. What this *does* depends on what is on
    /// screen — see [`crate::scroll::route`]. On the alternate screen there is
    /// no scrollback to move through, so the gesture becomes arrow keys; that
    /// is the whole reason scrolling inside a full-screen CLI used to do
    /// nothing at all.
    pub fn scroll(&mut self, delta: i32) {
        let focus = self.focus;
        self.scroll_pane(focus, delta);
    }

    /// A scroll gesture with a pointer behind it.
    ///
    /// Falls back to the focused pane when the pointer is over a divider or
    /// off the grid, so a gesture is never simply dropped.
    pub fn scroll_at(&mut self, delta: i32, x: f32, y: f32) {
        let id = self.pane_at(x, y).unwrap_or(self.focus);
        self.scroll_pane(id, delta);
    }

    /// Resizes to a new drawable size, reflowing the grid.
    pub fn resize(&mut self, viewport: (u32, u32), scale: f32) {
        let scale_changed = (self.scale - scale).abs() > f32::EPSILON;
        if self.viewport == viewport && !scale_changed {
            return;
        }
        self.viewport = viewport;

        if scale_changed {
            // Metrics are in device pixels, so a move to a different display
            // invalidates every rasterised glyph.
            self.scale = scale;
            self.atlas.rebuild(FontSet::resolve(
                &self.settings.font_family,
                self.settings.font_size,
                scale,
            ));
        }

        // Every pane's share of the window changed, so every pane's shell has
        // to be told. `relayout` skips the ones whose rectangle came out the
        // same, which for a single-pane window is never.
        self.relayout();
        for pane in &mut self.panes {
            pane.session.damage_all();
        }
        self.renderer.scheduler().request(Reason::Resize);
    }

    /// How long a theme change takes to cross-fade.
    ///
    /// Long enough to read as a transition rather than a flicker, short enough
    /// that switching themes in the palette still feels like a switch.
    const THEME_FADE: Duration = Duration::from_millis(220);

    /// How long a new pane takes to arrive.
    ///
    /// Shorter than a theme change: a split is a thing you did on purpose and
    /// want to start typing into, so the motion has to be over before your
    /// hands are. Reduce Motion collapses it — `Crossfade::start` handles
    /// that, the same way it does for the theme.
    const SPLIT_FADE: Duration = Duration::from_millis(160);

    /// Swaps the theme, cross-fading rather than cutting.
    ///
    /// The whole grid repaints for the duration, which is what makes the
    /// eight-role model visible all at once — and is also why this is a
    /// bounded animation rather than something that runs while the user reads.
    pub fn set_theme(&mut self, id: &str) -> bool {
        let Some(theme) = builtin(id) else { return false };
        let Ok(material) = Material::from_theme(&theme) else { return false };
        if material == self.material {
            return true;
        }
        self.previous_material = Some(self.display_material.clone());
        self.material = material;
        self.crossfade = Crossfade::start(Surface::THEME_FADE, &self.motion);
        self.settings.theme = id.to_owned();
        // Take the blend at t=0 straight away. It carries the old colours, but
        // it carries the *new* identity, so anything asking which theme is on
        // gets the answer the user just chose rather than the one that is
        // still fading out.
        self.refresh_display_material();
        self.sess_mut().damage_all();
        self.renderer.scheduler().request(Reason::Animation);
        self.sync_animation();
        true
    }

    /// The material actually being drawn, which during a cross-fade is
    /// somewhere between the old theme and the new one.
    pub fn theme(&self) -> &Material {
        &self.display_material
    }

    /// The theme the surface is settling on, ignoring any fade in progress.
    pub fn target_theme(&self) -> &Material {
        &self.material
    }

    pub fn motion(&self) -> &MotionSettings {
        &self.motion
    }

    /// Replaces the motion settings — a settings-file reload, a palette
    /// action, or the system Reduce Motion preference changing under us.
    ///
    /// Switching to a style that does not interpolate has to land the caret
    /// immediately: leaving it mid-flight would strand it between two cells
    /// with nothing left running to move it.
    /// Applies a settings file that changed on disk.
    ///
    /// Only what can change under a running shell: theme, caret physics,
    /// ambient light, and the key bindings. Font, grid size and scrollback are
    /// deliberately left for the next launch — reflowing a live grid under a
    /// program that is drawing into it is a different feature, and a settings
    /// reload is the wrong place to discover that.
    ///
    /// Returns what was *not* applied, so the caller can say so rather than
    /// leaving the user to wonder why their font did not change.
    pub fn apply_settings(&mut self, settings: &Settings) -> Vec<&'static str> {
        let mut deferred = Vec::new();
        if settings.font_family != self.settings.font_family
            || settings.font_size != self.settings.font_size
        {
            deferred.push("font");
        }
        if settings.columns != self.settings.columns || settings.rows != self.settings.rows {
            deferred.push("window size");
        }
        if settings.scrollback != self.settings.scrollback {
            deferred.push("scrollback");
        }
        if settings.shell != self.settings.shell {
            deferred.push("shell");
        }

        let ambient_changed = settings.ambient != self.settings.ambient;
        self.settings.ambient = settings.ambient;
        self.settings.bell = settings.bell;
        self.settings.keys = settings.keys.clone();
        self.bindings = Bindings::from_overrides(&settings.keys);
        self.refresh_palette_accelerators();

        if settings.motion != self.motion {
            self.set_motion(settings.motion);
        }
        self.set_theme(&settings.theme);
        if ambient_changed {
            self.sess_mut().damage_all();
            self.renderer.scheduler().request(Reason::Damage);
        }
        deferred
    }

    pub fn set_motion(&mut self, motion: MotionSettings) {
        self.motion = motion.sanitised();
        self.settings.motion = self.motion;
        if !self.motion.effective_style().interpolates() {
            let (column, line) = self.caret_target();
            self.pane_mut().caret.teleport(column, line);
        }
        self.renderer.scheduler().request(Reason::Cursor);
        self.sync_animation();
    }

    /// Where the caret is trying to be, in fractional cell coordinates.
    fn caret_target(&self) -> (f32, f32) {
        let cursor = self.sess().cursor();
        (cursor.column as f32, cursor.line as f32)
    }

    /// Steps every animation by `dt`.
    ///
    /// Called once per frame by the window layer, which is the only place that
    /// can measure real elapsed time. Everything downstream of here is a pure
    /// function of `dt`, which is why the physics can be tested without a
    /// window and why a frame that took 300 ms does not fling the caret across
    /// the screen — see `MAX_STEP` in `mica-core::motion`.
    pub fn advance(&mut self, dt: Duration) {
        let (column, line) = self.caret_target();
        if [column, line] != self.pane_mut().caret.target() {
            let motion = self.motion;
            self.pane_mut().caret.retarget(column, line, &motion);
            self.renderer.scheduler().request(Reason::Cursor);
        }

        let motion = self.motion;
        if self.pane_mut().caret.advance(dt, &motion) {
            self.renderer.scheduler().request(Reason::Cursor);
        }

        if let Some(split) = &mut self.split_motion {
            if split.fade.advance(dt) {
                // Only the arriving pane is changing, so only it is damaged.
                let id = split.pane;
                if let Some(pane) = self.panes.iter_mut().find(|p| p.id == id) {
                    pane.session.damage_all();
                }
                self.renderer.scheduler().request(Reason::Animation);
            } else {
                self.split_motion = None;
                // One last frame at full opacity, or the pane finishes a
                // fraction short of solid and stays there until something
                // else damages it.
                for pane in &mut self.panes {
                    pane.session.damage_all();
                }
                self.renderer.scheduler().request(Reason::Animation);
            }
        }

        if self.crossfade.is_running() {
            self.crossfade.advance(dt);
            // Every cell's colour is changing, so every cell is damaged. This
            // is the one animation in Mica that is genuinely full-screen, and
            // it is why it is measured in a couple of hundred milliseconds.
            self.sess_mut().damage_all();
            self.renderer.scheduler().request(Reason::Animation);
        }
        self.refresh_display_material();
        self.sync_animation();
    }

    fn refresh_display_material(&mut self) {
        match (&self.previous_material, self.crossfade.is_running()) {
            (Some(previous), true) => {
                self.display_material = previous.blend(&self.material, self.crossfade.t());
            }
            (Some(_), false) => {
                // The fade finished: drop the old theme so the blend stops
                // being recomputed and the surface stops holding a Material
                // nobody can see any more.
                self.previous_material = None;
                self.display_material = self.material.clone();
            }
            (None, _) => {}
        }
    }

    /// Keeps the scheduler's animation count in step with reality.
    ///
    /// Registered exactly once while something is moving and released exactly
    /// once when everything stops, because the count reaching zero is what
    /// makes an idle terminal go silent.
    fn sync_animation(&mut self) {
        let motion = self.motion;
        let running = self.panes.iter().any(|p| p.caret.is_animating(&motion))
            || self.crossfade.is_running()
            || self.split_motion.is_some();
        if running == self.animating {
            return;
        }
        self.animating = running;
        if running {
            self.renderer.scheduler().begin_animation();
        } else {
            self.renderer.scheduler().end_animation();
        }
    }

    pub fn scheduler(&mut self) -> &mut mica_gpu::frame::FrameScheduler {
        self.renderer.scheduler()
    }

    pub fn stats(&self) -> mica_gpu::frame::FrameStats {
        self.renderer.stats()
    }

    /// Opens a shell for a new pane, in the same world as the first one.
    ///
    /// Same environment, same terminfo, same shell integration — a pane that
    /// came up without those would be a subtly different terminal sitting next
    /// to the real one.
    fn spawn_session(
        &self,
        cols: u16,
        rows: u16,
        cwd: Option<PathBuf>,
    ) -> Result<Session, SurfaceError> {
        let mut config = PtyConfig::for_shell_settings(cols, rows, &self.settings.shell);
        config.cols = cols;
        config.rows = rows;
        config.env = self.spawn_env.clone();
        if let Some(dir) = cwd {
            if dir.is_dir() {
                config.cwd = Some(dir);
            }
        }
        Ok(Session::spawn_with_wakeup(&self.settings, config, self.wakeup.clone())?)
    }

    /// Splits the focused pane, putting the new one on the given side.
    ///
    /// Returns false when the split was refused — the pane is already as small
    /// as a pane gets, or the shell would not start. Nothing is half-applied
    /// in either case: the layout is only replaced once the shell is running.
    pub fn split(&mut self, direction: Direction) -> bool {
        let area = self.area();
        let id = PaneId(self.next_pane);

        // Try the split on a copy first. A layout that refuses is a layout
        // that has not changed, and it must not cost a spawned shell to find
        // that out.
        let mut layout = self.layout.clone();
        if !layout.split(area, self.focus, direction, id) {
            return false;
        }
        let Some(rect) = layout.rect_of(area, id) else { return false };

        let cwd = match self.settings.shell.new_pane_origin {
            mica_core::settings::PaneOrigin::Inherit => {
                self.sess().cwd().map(PathBuf::from)
            }
            mica_core::settings::PaneOrigin::StartingDir => None,
        };
        let session = match self.spawn_session(rect.cols, rect.rows, cwd) {
            Ok(session) => session,
            Err(error) => {
                eprintln!("mica: could not open a pane: {error}");
                return false;
            }
        };

        self.next_pane += 1;
        self.layout = layout;
        self.panes.push(Pane {
            id,
            session,
            caret: Caret::new(0.0, 0.0),
            rect,
            title: String::from("Mica"),
        });
        if self.settings.shell.focus_new_panes {
            self.set_focus(id);
        }
        self.relayout();
        self.split_motion =
            Some(SplitMotion { pane: id, fade: Crossfade::start(Surface::SPLIT_FADE, &self.motion) });
        self.renderer.scheduler().request(Reason::Animation);
        self.sync_animation();
        true
    }

    /// Closes a pane and gives its space back to its sibling.
    ///
    /// Returns false when that was the last pane, which is the window layer's
    /// signal to close the window rather than leave an empty one.
    pub fn close_pane(&mut self, id: PaneId) -> bool {
        let area = self.area();
        let Some(next) = self.layout.close(area, id) else { return false };
        self.panes.retain(|p| p.id != id);
        if self.focus == id {
            self.set_focus(next);
        }
        self.relayout();
        true
    }

    /// Moves focus to the pane in that direction, if there is one.
    pub fn focus_direction(&mut self, direction: Direction) -> bool {
        let area = self.area();
        let Some(next) = self.layout.neighbour(area, self.focus, direction) else {
            return false;
        };
        self.set_focus(next);
        true
    }

    fn set_focus(&mut self, id: PaneId) {
        if self.focus == id || !self.panes.iter().any(|p| p.id == id) {
            return;
        }
        self.focus = id;
        // Both carets change: one lights up, one hollows out. And the divider
        // beside the newly focused pane changes colour, which is a quad rather
        // than a cell, so damage alone would not redraw it.
        for pane in &mut self.panes {
            pane.session.damage_all();
        }
        self.renderer.scheduler().request(Reason::Focus);
    }

    /// Recomputes every pane's rectangle and tells its shell the new size.
    ///
    /// A pane whose rectangle did not change is left alone — a `SIGWINCH` to a
    /// shell that is the same size as it was is noise a full-screen program
    /// will redraw itself over.
    fn relayout(&mut self) {
        let area = self.area();
        for (id, rect) in self.layout.rects(area) {
            let Some(pane) = self.panes.iter_mut().find(|p| p.id == id) else { continue };
            if pane.rect == rect {
                continue;
            }
            pane.rect = rect;
            let _ = pane.session.resize(rect.cols, rect.rows);
            pane.session.damage_all();
        }
        self.renderer.scheduler().request(Reason::Resize);
    }

    /// The whole grid the window has to give, in cells.
    ///
    /// One number for the window, not per pane: the layout divides it, and
    /// every pane's rectangle is a slice of it. Panes exist inside one grid,
    /// which is what lets them be drawn in a single pass.
    fn area(&self) -> CellRect {
        let metrics = self.atlas.metrics();
        let (cols, rows) = grid_size(self.viewport, metrics.width, metrics.height);
        CellRect::new(0, 0, cols, rows)
    }

    /// A hairline down the middle of every divider cell.
    ///
    /// A whole cell would be a wall; what a split needs is the thinnest mark
    /// that still separates. The line is drawn in `Dim` and the one bordering
    /// the focused pane in `Accent`, so where you are typing is visible
    /// without a title bar or a highlight over the text.
    fn build_dividers(&mut self, metrics: mica_atlas::fontset::CellMetrics) {
        if self.panes.len() < 2 {
            return;
        }
        let cell = (metrics.width as f32, metrics.height as f32);
        let thickness = (cell.0 / 8.0).clamp(1.0, 3.0);
        let dim = Rgba::with_alpha(self.display_material.role(Role::Dim), 0.55);
        let area = self.area();
        let focused = self.layout.rect_of(area, self.focus);

        let arriving = self
            .split_motion
            .as_ref()
            .and_then(|m| self.layout.rect_of(area, m.pane));

        let mut quads = std::mem::take(&mut self.renderer.buffers().quads);
        for divider in self.layout.dividers(area) {
            let touches_new = arriving.is_some_and(|r| touches(divider, r));
            let touches_focus = focused.is_some_and(|r| touches(divider, r));
            let fill = if touches_focus && self.focused {
                Rgba::with_alpha(self.display_material.role(Role::Accent), 0.7)
            } else {
                dim
            };
            // A divider next to an arriving pane draws itself out from the
            // middle rather than appearing whole. `t` is eased, so the line
            // arrives at the same moment the text in the pane does.
            let extent = match (&self.split_motion, touches_new) {
                (Some(motion), true) => motion.fade.t(),
                _ => 1.0,
            };
            let (origin, size) = match divider.axis {
                crate::pane::Axis::Columns => {
                    let full = divider.len as f32 * cell.1;
                    let drawn = full * extent;
                    (
                        [
                            (divider.at as f32 + 0.5) * cell.0 - thickness / 2.0,
                            divider.start as f32 * cell.1 + (full - drawn) / 2.0,
                        ],
                        [thickness, drawn],
                    )
                }
                crate::pane::Axis::Rows => {
                    let full = divider.len as f32 * cell.0;
                    let drawn = full * extent;
                    (
                        [
                            divider.start as f32 * cell.0 + (full - drawn) / 2.0,
                            (divider.at as f32 + 0.5) * cell.1 - thickness / 2.0,
                        ],
                        [drawn, thickness],
                    )
                }
            };
            quads.push(mica_gpu::grid::QuadInstance {
                origin,
                size,
                fill,
                border: Rgba([0, 0, 0, 0]),
                radius: 0.0,
                border_width: 0.0,
            });
        }
        self.renderer.buffers().quads = quads;
    }

    /// Builds one frame's instances from the damaged rows.
    ///
    /// Note what is *not* here: no full-grid walk. Only rows the terminal
    /// reported dirty are visited, so the cost of a frame is proportional to
    /// what changed rather than to what is on screen.
    fn build_frame(&mut self) {
        let metrics = self.atlas.metrics();
        // Only the caret, the gutter and the overlays are rebuilt
        // unconditionally. The grid's own instances survive a frame that has
        // nothing new to say — see below.
        self.renderer.buffers().clear_transient();

        // **A frame with no damage keeps the rows it already has.**
        //
        // The render pass clears the whole target, so every frame paints from
        // nothing: an instance that is not re-emitted is gone. Rows are only
        // emitted when the terminal reports them damaged, which is fine as
        // long as every frame is a damage frame — and every frame was, until
        // the caret started animating. An animation frame has no new output,
        // so it emitted no rows, so it painted a blank screen with a caret on
        // it. While typing, that alternates with real frames at 60 Hz and the
        // text appears to flash away as you write it.
        //
        // When there *is* damage the whole grid is rebuilt rather than the
        // damaged rows alone. Rebuilding a subset would mean holding the rest
        // from previous frames, and those instances point into atlas slots
        // that nothing has touched in the meantime and that eviction is free
        // to reuse. Repainting everything keeps every glyph's use current, and
        // costs a hash lookup per cell on frames that were going to redraw
        // anyway.
        if self.panes.iter().any(|p| p.session.has_damage()) {
            self.atlas.begin_frame();
            self.renderer.buffers().clear_rows();
            for pane in &mut self.panes {
                pane.session.damage_all();
            }

            // Every pane, one pass. Instances carry cell coordinates, so a
            // pane is an offset rectangle of one shared grid — which is why a
            // four-way split still costs the single draw call a lone terminal
            // does.
            let material = self.display_material.clone();
            let mut buffers = std::mem::take(self.renderer.buffers());
            let arriving = self.split_motion.as_ref().map(|m| (m.pane, m.fade.t()));
            for pane in &self.panes {
                let alpha = match arriving {
                    Some((id, t)) if id == pane.id => t,
                    _ => 1.0,
                };
                let builder = RowBuilder {
                    material: &material,
                    tables: pane.session.side_tables(),
                    metrics,
                    alpha,
                    origin: (pane.rect.col, pane.rect.row),
                };
                // The borrow checker is doing real work here: `dirty_rows`
                // borrows the session while the atlas is borrowed mutably, so
                // the rows are collected first.
                let rows: Vec<_> = pane.session.dirty_rows().collect();
                for row in rows {
                    builder.build_row(row, &mut self.atlas, &mut buffers);
                }
            }
            *self.renderer.buffers() = buffers;
        }

        // The lines between the panes. Drawn from the layout rather than from
        // the panes, because the divider cell belongs to neither of them.
        self.build_dividers(metrics);

        // A caret per pane. The unfocused ones are drawn hollow by
        // `cursor_shape`, which is how you can tell at a glance where typing
        // will land.
        let motion = self.motion;
        let cell = (metrics.width as f32, metrics.height as f32);
        let material = self.display_material.clone();
        let focus = self.focus;
        let window_focused = self.focused;
        let mut decays = std::mem::take(&mut self.renderer.buffers().decays);
        let mut shapes = std::mem::take(&mut self.renderer.buffers().shapes);
        let mut gutters = Vec::new();
        for pane in &mut self.panes {
            // The caret and its wake read the physics rather than the cursor's
            // cell: while an animation is in flight those are different
            // places, and that difference is the whole effect.
            let cursor = pane.session.cursor();
            let presentation = pane.caret.presentation(&motion);
            let origin = (pane.rect.col as f32 * cell.0, pane.rect.row as f32 * cell.1);
            let lit = window_focused && pane.id == focus;
            caret_decay(cursor, pane.caret.trail(), metrics, &material, origin, &mut decays);
            if let Some(shape) =
                cursor_shape(cursor, presentation, metrics, &material, origin, lit)
            {
                shapes.push(shape);
            }
            let blocks = pane.session.blocks().to_vec();
            block_gutters(
                &blocks,
                0,
                pane.rect.rows,
                &material,
                metrics,
                (pane.rect.col, pane.rect.row),
                &mut gutters,
            );
        }
        self.renderer.buffers().decays = decays;
        self.renderer.buffers().shapes = shapes;
        self.renderer.buffers().gutters = gutters;
        let (_, rows) = self.sess().dimensions();

        // Overlays last, so their quads land over the grid.
        if self.shortcuts.is_open() {
            let overlay_metrics = OverlayMetrics::from_atlas(
                &self.atlas,
                (self.viewport.0 as f32, self.viewport.1 as f32),
            );
            let rows = self.shortcuts.rows(&self.bindings);
            let view_rows: Vec<ShortcutRow<'_>> = rows
                .iter()
                .map(|(label, chord)| ShortcutRow { label, chord })
                .collect();
            let mut buffers = std::mem::take(self.renderer.buffers());
            shortcuts::render(
                ShortcutView {
                    rows: &view_rows,
                    selected: self.shortcuts.selected(),
                    capturing: self.shortcuts.is_capturing(),
                    notice: self.shortcuts.notice(),
                },
                &mut self.atlas,
                &self.display_material,
                overlay_metrics,
                &mut buffers,
            );
            *self.renderer.buffers() = buffers;
        }
        if self.palette.is_open() || self.find.is_open() {
            let overlay_metrics = OverlayMetrics::from_atlas(
                &self.atlas,
                (self.viewport.0 as f32, self.viewport.1 as f32),
            );
            let mut buffers = std::mem::take(self.renderer.buffers());
            self.palette.render(
                &mut self.atlas,
                &self.display_material,
                overlay_metrics,
                &mut buffers,
            );
            self.find.render(
                &mut self.atlas,
                &self.display_material,
                overlay_metrics,
                0,
                rows,
                (0.0, 0.0),
                &mut buffers,
            );
            *self.renderer.buffers() = buffers;
        }
    }

    fn uniforms(&self) -> Uniforms {
        Uniforms::new(
            (self.viewport.0 as f32, self.viewport.1 as f32),
            self.atlas.metrics(),
            (0.0, 0.0),
            self.atlas.page_size() as f32,
            0.0,
            1.0,
        )
    }

    fn substrate(&self) -> SubstrateUniforms {
        let background = self.display_material.role(Role::Background);
        let accent = self.display_material.role(Role::Accent);
        SubstrateUniforms {
            background: background.to_linear(),
            tint: accent.to_linear(),
            focus: [0.5, 0.25],
            // `[ambient] intensity` is a dial from 0.1 to 1.0 rather than a
            // raw shader constant, because the number in the settings file has
            // to mean something to a person. 0.1 — the default — is the 0.035
            // this pass was hardcoded to before it was configurable, so an
            // untouched install looks exactly as it did.
            intensity: self.settings.ambient.substrate_intensity(),
            vignette: 0.12,
        }
    }

    /// Renders into a texture. Used by the window layer with a drawable's
    /// texture, and by tests with an offscreen one.
    /// Renders one frame into a drawable and returns without waiting.
    ///
    /// The live path. `render_to_texture` is the offscreen twin, kept for
    /// tests that read pixels back.
    pub fn render_to_drawable(
        &mut self,
        drawable: &objc2::runtime::ProtocolObject<dyn objc2_metal::MTLDrawable>,
        target: &objc2::runtime::ProtocolObject<dyn objc2_metal::MTLTexture>,
    ) -> Result<(), SurfaceError> {
        self.build_frame();
        self.renderer.sync_atlas(&mut self.atlas)?;
        let (uniforms, substrate) = (self.uniforms(), self.substrate());
        self.renderer.render_to_drawable(drawable, target, uniforms, substrate)?;
        self.sess_mut().clear_damage();
        Ok(())
    }

    pub fn render_to_texture(
        &mut self,
        target: &objc2::runtime::ProtocolObject<dyn objc2_metal::MTLTexture>,
    ) -> Result<(), SurfaceError> {
        self.build_frame();
        self.renderer.sync_atlas(&mut self.atlas)?;
        let (uniforms, substrate) = (self.uniforms(), self.substrate());
        self.renderer.render_to_texture(target, uniforms, substrate)?;
        self.sess_mut().clear_damage();
        Ok(())
    }

    /// The Metal device the renderer is using.
    ///
    /// The window layer must be given *this* device, not a freshly created
    /// one: a drawable from a different device cannot be rendered into by
    /// this renderer's command queue.
    pub fn device(&self) -> &objc2::runtime::ProtocolObject<dyn objc2_metal::MTLDevice> {
        self.renderer.context().device()
    }

    pub fn renderer(&mut self) -> &mut Renderer {
        &mut self.renderer
    }

    pub fn atlas(&mut self) -> &mut Atlas {
        &mut self.atlas
    }

    /// The column the *terminal's* cursor is in, which during an animation is
    /// not where the caret is drawn.
    /// How many trail samples the caret is currently carrying.
    pub fn caret_trail_len(&self) -> usize {
        self.pane().caret.trail().count()
    }

    /// How far a newly opened pane is through arriving, or `None` when
    /// nothing is arriving.
    #[cfg(test)]
    pub(crate) fn split_progress(&self) -> Option<f32> {
        self.split_motion.as_ref().map(|m| m.fade.t())
    }

    #[cfg(test)]
    pub(crate) fn has_damage_for_test(&self) -> bool {
        self.panes.iter().any(|p| p.session.has_damage())
    }

    /// The cell height in **points**, not device pixels.
    ///
    /// Scroll deltas arrive in points, so this is the unit they must be
    /// divided by; using device pixels would make scrolling twice as fast on a
    /// Retina display.
    pub fn cell_height_points(&self) -> f32 {
        let height = self.atlas.metrics().height as f32;
        if self.scale > 0.0 { height / self.scale } else { height }
    }

    /// The drawable size in points, which is what a mouse event is measured
    /// in.
    pub fn viewport_points(&self) -> (f32, f32) {
        let scale = if self.scale > 0.0 { self.scale } else { 1.0 };
        (self.viewport.0 as f32 / scale, self.viewport.1 as f32 / scale)
    }

    pub fn cell_width_points(&self) -> f32 {
        let width = self.atlas.metrics().width as f32;
        if self.scale > 0.0 { width / self.scale } else { width }
    }

    /// Which pane is under a point, in view points from the top-left.
    ///
    /// `None` for the divider between two panes — a click on the line belongs
    /// to neither, and moving focus because the user grabbed a divider would
    /// be the wrong answer to the wrong question.
    pub fn pane_at(&self, x: f32, y: f32) -> Option<PaneId> {
        let (w, h) = (self.cell_width_points(), self.cell_height_points());
        if w <= 0.0 || h <= 0.0 || x < 0.0 || y < 0.0 {
            return None;
        }
        self.layout.at(self.area(), (x / w) as u16, (y / h) as u16)
    }

    /// Focuses whatever pane is under the pointer. Returns whether focus moved.
    pub fn focus_pane_at(&mut self, x: f32, y: f32) -> bool {
        let Some(id) = self.pane_at(x, y) else { return false };
        if id == self.focus {
            return false;
        }
        self.set_focus(id);
        true
    }

    /// A scroll gesture aimed at one particular pane.
    ///
    /// The pointer decides, not the focus: every terminal with panes scrolls
    /// what you are pointing at, and scrolling the focused pane while the
    /// mouse is over a different one reads as the gesture being ignored.
    pub fn scroll_pane(&mut self, id: PaneId, delta: i32) {
        let Some(index) = self.panes.iter().position(|p| p.id == id) else { return };
        let modes = self.panes[index].session.modes();
        match crate::scroll::route(delta, modes) {
            crate::scroll::ScrollTarget::Viewport(lines) => {
                self.panes[index].session.scroll(lines);
                if self.panes[index].session.has_damage() {
                    self.renderer.scheduler().request(Reason::Damage);
                }
            }
            crate::scroll::ScrollTarget::Keys(bytes) => {
                let _ = self.panes[index].session.write_input(&bytes);
            }
            crate::scroll::ScrollTarget::Nothing => {}
        }
    }

    pub fn cursor_line(&self) -> u16 {
        self.sess().cursor().line
    }

    pub fn cursor_column(&self) -> u16 {
        self.sess().cursor().column
    }

    /// Where the caret is actually drawn, in fractional cell coordinates.
    pub fn caret_position(&self) -> [f32; 2] {
        self.pane().caret.position()
    }

    pub fn cursor_shape(&self) -> CursorShape {
        self.sess().cursor().shape
    }
}

/// The palette's base entries — the same catalogue the shortcut panel and the
/// settings file are built from.
///
/// One list, three surfaces. The palette used to carry its own, which is how
/// it advertised `⌃⇥` for tabs that do not exist and `⌘↓` for two different
/// actions at once.
fn palette_actions() -> Vec<Action> {
    crate::bindings::BINDABLE
        .iter()
        .filter(|b| b.implemented)
        .map(|b| Action::plain(b.id, b.label))
        .collect()
}

/// The ids of the built-in themes, for the palette.
fn theme_ids() -> Vec<String> {
    mica_core::material::builtin_themes().into_iter().map(|t| t.id).collect()
}

/// How many cells fit in a drawable.
///
/// Rounded down, and floored at 1×1: a window dragged to zero height must not
/// produce a zero-row grid, because a terminal with no rows makes every
/// downstream index calculation undefined.
/// Whether a divider runs along one of a rectangle's edges.
fn touches(divider: crate::pane::Divider, rect: CellRect) -> bool {
    match divider.axis {
        crate::pane::Axis::Columns => {
            divider.at + DIVIDER == rect.col || divider.at == rect.col + rect.cols
        }
        crate::pane::Axis::Rows => {
            divider.at + DIVIDER == rect.row || divider.at == rect.row + rect.rows
        }
    }
}

pub fn grid_size(viewport: (u32, u32), cell_width: u16, cell_height: u16) -> (u16, u16) {
    let cols = (viewport.0 / cell_width.max(1) as u32).max(1);
    let rows = (viewport.1 / cell_height.max(1) as u32).max(1);
    (cols.min(u16::MAX as u32) as u16, rows.min(u16::MAX as u32) as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mica_core::motion::MotionStyle;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("mica-surface-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn surface(name: &str) -> Surface {
        let root = temp_root(name);
        let mut surface = Surface::open(Settings::default(), (800, 480), 2.0, root.clone(), None)
            .expect("a surface should open on this machine");
        // Never the real one. The suite rewrote the developer's own
        // `~/.config/mica/settings.toml` before this line existed, and left a
        // test's rebinding in it.
        surface.set_settings_path(root.join("settings.toml"));
        surface
    }

    #[test]
    fn grid_size_rounds_down_and_never_reaches_zero() {
        assert_eq!(grid_size((800, 480), 10, 20), (80, 24));
        assert_eq!(grid_size((805, 489), 10, 20), (80, 24));
        // A window dragged to nothing must still have a grid.
        assert_eq!(grid_size((0, 0), 10, 20), (1, 1));
        assert_eq!(grid_size((5, 5), 10, 20), (1, 1));
    }

    #[test]
    fn a_surface_opens_with_a_shell_running() {
        let mut s = surface("open");
        assert!(!s.has_exited());
        let (cols, rows) = s.session().dimensions();
        assert!(cols > 1 && rows > 1, "grid is {cols}x{rows}");
    }

    #[test]
    fn a_surface_renders_a_frame_and_then_goes_quiet() {
        // The end-to-end version of the idle-frame claim: a real shell, a real
        // atlas, a real Metal device — and once it has settled, nothing.
        let mut s = surface("idle");
        let target = s.renderer().context().offscreen_target(800, 480).unwrap();

        // Let the shell start up and draw whatever it wants to.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            s.pump();
            if s.scheduler().is_dirty() {
                s.render_to_texture(&target).unwrap();
                s.scheduler().begin_frame();
                s.scheduler().end_frame();
            }
        }
        let settled = s.stats().submitted;

        // Now stop touching it and poll as a 120 Hz display would.
        for _ in 0..1200 {
            s.pump();
            if s.scheduler().is_dirty() {
                s.render_to_texture(&target).unwrap();
                s.scheduler().begin_frame();
                s.scheduler().end_frame();
            }
        }
        assert_eq!(
            s.stats().submitted,
            settled,
            "an idle terminal submitted {} extra command buffers",
            s.stats().submitted - settled
        );
    }

    #[test]
    fn typing_produces_output_and_a_frame() {
        let mut s = surface("typing");
        let target = s.renderer().context().offscreen_target(800, 480).unwrap();

        // Drain the shell's startup.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            s.pump();
        }
        s.render_to_texture(&target).unwrap();
        s.scheduler().begin_frame();
        s.scheduler().end_frame();
        let before = s.stats().submitted;

        s.write_input(b"echo mica-surface-probe\n");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut drew = false;
        while std::time::Instant::now() < deadline && !drew {
            s.pump();
            if s.scheduler().is_dirty() {
                s.render_to_texture(&target).unwrap();
                s.scheduler().begin_frame();
                s.scheduler().end_frame();
                drew = true;
            }
        }
        assert!(drew, "typing never produced a frame");
        assert!(s.stats().submitted > before);
    }

    /// Runs the animation clock until nothing is moving, or gives up.
    fn settle(s: &mut Surface) -> usize {
        const FRAME: Duration = Duration::from_millis(8);
        for step in 1..=600 {
            s.advance(FRAME);
            if !s.scheduler().is_dirty() {
                return step;
            }
            s.scheduler().begin_frame();
            s.scheduler().end_frame();
        }
        panic!("the surface never stopped animating");
    }

    #[test]
    fn switching_the_theme_cross_fades_and_lands_on_the_new_one() {
        let mut s = surface("theme");
        settle(&mut s);

        assert!(s.set_theme("quartz"));
        assert_eq!(s.target_theme().id, "quartz");
        assert!(!s.target_theme().is_dark(), "quartz is supposed to be the light one");
        assert!(s.scheduler().is_dirty(), "a theme change did not schedule a frame");

        // Mid-fade the drawn colours are neither theme — that is the point of
        // fading rather than cutting.
        s.advance(Duration::from_millis(8));
        let mid = s.theme().role(Role::Background);
        assert_ne!(mid, s.target_theme().role(Role::Background), "the theme cut instead of fading");

        settle(&mut s);
        assert_eq!(
            s.theme().role(Role::Background),
            s.target_theme().role(Role::Background),
            "the fade did not finish on the new theme"
        );
        assert!(!s.theme().is_dark(), "the light theme did not take effect");
    }

    #[test]
    fn a_finished_cross_fade_releases_its_animation_and_the_surface_goes_silent() {
        // The cross-fade is the one full-screen animation in Mica. If it
        // failed to withdraw, every window that had ever changed theme would
        // render forever.
        let mut s = surface("theme-silence");
        settle(&mut s);
        assert!(s.set_theme("quartz"));

        let frames = settle(&mut s);
        assert!(frames > 3, "the cross-fade finished in {frames} frames — it cut");
        assert_eq!(s.scheduler().animations(), 0, "the cross-fade leaked an animation");

        let before = s.stats().submitted;
        for _ in 0..600 {
            s.advance(Duration::from_millis(8));
            assert!(!s.scheduler().is_dirty(), "a settled surface asked for a frame");
        }
        assert_eq!(s.stats().submitted, before);
    }

    #[test]
    fn the_caret_motion_action_cycles_through_every_style_and_returns() {
        // A toggle would only ever reach two of the seven, which is the bug
        // this action shipped with in the reference app's own palette naming.
        let mut s = surface("fx-cycle");
        let first = s.motion().style;
        let mut seen = vec![first];
        for _ in 0..MotionStyle::ALL.len() - 1 {
            assert!(s.dispatch("settings.fx.cursor"));
            seen.push(s.motion().style);
        }
        seen.sort_by_key(|style| style.id());
        seen.dedup();
        assert_eq!(seen.len(), MotionStyle::ALL.len(), "the cycle skipped a style");

        assert!(s.dispatch("settings.fx.cursor"));
        assert_eq!(s.motion().style, first, "the cycle did not come back round");
    }

    #[test]
    fn the_effect_toggles_actually_toggle() {
        let mut s = surface("fx-toggles");
        for (id, read) in [
            ("settings.fx.decay", (|m: &MotionSettings| m.decay) as fn(&MotionSettings) -> bool),
            ("settings.fx.blink", |m| m.blink),
            ("settings.fx.reduce", |m| m.reduce),
        ] {
            let before = read(s.motion());
            assert!(s.dispatch(id), "{id} was not recognised");
            assert_eq!(read(s.motion()), !before, "{id} did not change anything");
            assert!(s.dispatch(id));
            assert_eq!(read(s.motion()), before, "{id} did not toggle back");
        }
    }

    #[test]
    fn a_frame_with_no_new_output_still_draws_the_text() {
        // Regression test for the bug that made typing look like the text was
        // flashing away as it was written.
        //
        // The render pass clears the whole target, so a frame paints only what
        // it emits. Rows were emitted only when the terminal reported damage,
        // which was invisible for as long as every frame was a damage frame.
        // The caret animation broke that: an animation frame has no new
        // output, emitted no rows, and painted a blank screen with a caret on
        // it — at 60 Hz, alternating with real frames, while the user typed.
        let mut s = surface("undamaged-frame");
        s.write_input(b"echo hello");
        for _ in 0..400 {
            s.pump();
            if s.cursor_column() > 4 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(s.cursor_column() > 4, "the shell never echoed anything back");

        // Rendering, not just building: `clear_damage` is part of the frame,
        // and it is what makes the *next* frame an undamaged one.
        let frame = |s: &mut Surface| {
            let target = s.renderer().context().offscreen_target(400, 240).unwrap();
            s.render_to_texture(&target).unwrap();
            s.renderer().buffers().glyphs.len()
        };

        s.advance(Duration::from_millis(8));
        let with_output = frame(&mut s);
        assert!(with_output > 4, "the first frame drew almost nothing: {with_output}");

        // A second frame with nothing new to say. This is every animation
        // frame, and it must still paint the screen.
        assert!(!s.has_damage_for_test(), "the test did not reach an undamaged frame");
        s.advance(Duration::from_millis(8));
        let without_output = frame(&mut s);

        assert_eq!(
            without_output, with_output,
            "a frame with no new output dropped {} of {with_output} glyphs — that \
             is a blank screen for one frame in every two while typing",
            with_output - without_output
        );
    }

    #[test]
    fn the_caret_does_not_erase_the_character_underneath_it() {
        // A block caret drawn over the glyph is opaque and hides it. That is
        // survivable while the caret only ever sits on the empty cell past the
        // last character; once it interpolates it sweeps across everything
        // just typed, wiping each glyph as it passes. The passes are ordered
        // so the caret goes underneath — measured at 79 foreground pixels in
        // the cell before, zero with the caret on top.
        let order = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join("mica-gpu/src/renderer.rs"),
        )
        .expect("renderer.rs");
        let caret = order.find("pass!(shape").expect("the caret pass");
        let text = order.find("pass!(cell,").expect("the glyph pass");
        assert!(
            caret < text,
            "the caret pass is encoded after the glyph pass, so it paints over \
             the character it is sitting on"
        );
    }

    #[test]
    fn the_shortcut_panel_opens_from_its_own_binding() {
        let mut s = surface("keys-open");
        let chord = s.bindings().chord_for("keys.open").expect("keys.open is unbound");
        let id = s.bindings().action(chord).unwrap().to_owned();
        assert!(s.dispatch(&id));
        assert!(s.shortcuts().is_open(), "⌘, did not open the panel");
        assert!(s.overlay_has_focus(), "the panel does not hold the keyboard");
    }

    #[test]
    fn an_edited_settings_file_applies_what_it_can_and_names_what_it_cannot() {
        let mut s = surface("apply");
        let mut edited = Settings::default();
        edited.theme = "quartz".into();
        edited.ambient.intensity = 0.5;
        edited.motion.blink = !Settings::default().motion.blink;
        edited.keys.insert("find.toggle".into(), "cmd+j".into());
        // Two that cannot be applied to a live grid.
        edited.font_size = 18.0;
        edited.scrollback = 999;

        let deferred = s.apply_settings(&edited);

        assert_eq!(s.target_theme().id, "quartz");
        assert_eq!(s.substrate().intensity, edited.ambient.substrate_intensity());
        assert_eq!(s.motion().blink, edited.motion.blink);
        assert_eq!(
            s.bindings().chord_for("find.toggle"),
            Chord::parse("cmd+j"),
            "an edited binding did not reach the table"
        );
        assert!(deferred.contains(&"font"), "{deferred:?}");
        assert!(deferred.contains(&"scrollback"), "{deferred:?}");
        assert!(!deferred.contains(&"shell"), "{deferred:?}");
    }

    #[test]
    fn reloading_an_unchanged_file_defers_nothing() {
        // Otherwise every switch back to Mica prints a line telling the user
        // their font will apply on the next launch.
        let mut s = surface("apply-noop");
        assert!(s.apply_settings(&Settings::default()).is_empty());
    }

    #[test]
    fn no_test_surface_can_write_the_developers_own_settings_file() {
        // This is not hypothetical. Before `set_settings_path` existed, the
        // rebinding test below persisted through `mica_core::settings::
        // default_path()` and left `find.toggle = "cmd+j"` in the real
        // `~/.config/mica/settings.toml` of whoever ran `cargo test`.
        let s = surface("settings-path");
        assert_ne!(s.settings_path(), crate::config::path());
        assert!(
            s.settings_path().starts_with(std::env::temp_dir()),
            "a test surface persists to {}",
            s.settings_path().display()
        );
    }

    #[test]
    fn a_real_surface_persists_where_the_user_looks_for_it() {
        // The other half: the redirect above must not have moved the app's
        // own settings somewhere nobody can find them.
        let root = temp_root("real-path");
        let s = Surface::open(Settings::default(), (800, 480), 2.0, root, None).unwrap();
        assert_eq!(s.settings_path(), crate::config::path());
    }

    #[test]
    fn the_palette_still_prints_its_accelerators_after_being_reopened() {
        // Regression. `set_theme_ids` rebuilt the action list from scratch,
        // which dropped every accelerator the binding table had supplied — so
        // the shortcuts were printed exactly once, at construction, and every
        // subsequent open showed a palette with a blank right-hand column.
        let mut s = surface("palette-accel");
        s.toggle_palette();
        s.toggle_palette();
        s.toggle_palette();

        let action = s
            .palette()
            .actions()
            .iter()
            .find(|a| a.id == "find.toggle")
            .expect("find.toggle left the palette");
        assert_eq!(
            action.shortcut,
            Chord::new(Modifiers { command: true, ..Modifiers::NONE }, Key::Char('f'))
                .to_display(),
            "the palette reopened with no accelerator"
        );
    }

    #[test]
    fn the_palette_offers_exactly_the_actions_that_do_something() {
        // The palette is a list of things you can pick. An entry that does
        // nothing when you pick it is the failure; an implemented action
        // missing from the list is the other one. Both are checked here
        // because `BINDABLE` is the single source for both directions.
        let mut s = surface("palette-real");
        s.toggle_palette();
        let ids: Vec<&str> = s.palette().actions().iter().map(|a| a.id.as_str()).collect();

        for bindable in crate::bindings::BINDABLE {
            let listed = ids.contains(&bindable.id);
            assert_eq!(
                listed, bindable.implemented,
                "`{}` is {} in BINDABLE and {} in the palette",
                bindable.id,
                if bindable.implemented { "implemented" } else { "not implemented" },
                if listed { "listed" } else { "missing" }
            );
        }
        assert!(ids.contains(&"find.toggle"), "the palette came up empty");
    }

    #[test]
    fn toggling_ambient_light_changes_what_the_substrate_asks_for() {
        let mut s = surface("ambient");
        let lit = s.substrate().intensity;
        assert!(lit > 0.0, "the ambient pass starts off");
        assert!(s.dispatch("settings.fx.ambient"));
        assert_eq!(s.substrate().intensity, 0.0);
        assert!(s.dispatch("settings.fx.ambient"));
        assert_eq!(s.substrate().intensity, lit);
    }

    #[test]
    fn the_default_ambient_intensity_is_what_the_pass_was_hardcoded_to() {
        // The dial in the settings file runs 0.1 to 1.0 so the number means
        // something to a person. 0.1 has to keep looking exactly like the
        // constant it replaced, or every existing install changes appearance
        // on upgrade.
        assert_eq!(
            mica_core::settings::AmbientSettings::default().substrate_intensity(),
            0.035
        );
    }

    #[test]
    fn arrows_space_and_a_combination_rebind_an_action_end_to_end() {
        // The whole interaction the panel exists for, driven through the same
        // entry point the window uses.
        let mut s = surface("keys-rebind");
        s.toggle_shortcuts();

        // Walk down to a known row rather than assuming it is the second
        // one: the order of `BINDABLE` is presentation, not contract.
        let target = crate::bindings::BINDABLE
            .iter()
            .position(|b| b.id == "find.toggle")
            .expect("find.toggle left the bindable set");
        for _ in 0..target {
            s.shortcut_key(Key::Down, Modifiers::NONE);
        }
        let id = s.shortcuts().selected_id().expect("nothing selected");
        assert_eq!(id, "find.toggle");

        s.shortcut_key(Key::Char(' '), Modifiers::NONE);
        assert!(s.shortcuts().is_capturing());

        let chord = Chord::new(Modifiers { command: true, ..Modifiers::NONE }, Key::Char('j'));
        s.shortcut_key(chord.key(), chord.modifiers());
        assert_eq!(s.bindings().chord_for("find.toggle"), Some(chord));
        assert!(!s.shortcuts().is_capturing());
    }

    #[test]
    fn the_panel_swallows_every_key_while_it_is_open() {
        // Anything falling through would be typed into the shell behind the
        // panel, which the user cannot see happening.
        let mut s = surface("keys-swallow");
        s.toggle_shortcuts();
        for key in [Key::Char('x'), Key::Enter, Key::Tab, Key::Left, Key::Function(5)] {
            assert!(s.shortcut_key(key, Modifiers::NONE), "{key:?} escaped the panel");
        }
    }

    #[test]
    fn a_closed_panel_consumes_nothing() {
        let mut s = surface("keys-closed");
        assert!(!s.shortcut_key(Key::Down, Modifiers::NONE));
    }

    #[test]
    fn the_palette_prints_the_accelerators_the_table_actually_holds() {
        // The drift this table exists to end: the palette used to advertise
        // `⌘↓` for two different actions and `⌘↑` for one that had no binding.
        let mut s = surface("keys-palette");
        let check = |s: &Surface| {
            for action in s.palette().actions() {
                let expected = s
                    .bindings()
                    .chord_for(&action.id)
                    .map(Chord::to_display)
                    .unwrap_or_default();
                assert_eq!(
                    action.shortcut, expected,
                    "the palette shows {:?} for {} but the table says {:?}",
                    action.shortcut, action.id, expected
                );
            }
        };
        check(&s);

        // And after an edit, not just at startup.
        s.toggle_shortcuts();
        s.shortcut_key(Key::Char(' '), Modifiers::NONE);
        s.shortcut_key(Key::Char('j'), Modifiers { command: true, ..Modifiers::NONE });
        check(&s);
    }

    #[test]
    fn no_two_actions_advertise_the_same_accelerator() {
        let s = surface("keys-unique");
        let mut seen = std::collections::HashMap::new();
        for action in s.palette().actions() {
            if action.shortcut.is_empty() {
                continue;
            }
            if let Some(other) = seen.insert(action.shortcut.clone(), action.id.clone()) {
                panic!("{} and {} both claim {}", other, action.id, action.shortcut);
            }
        }
    }

    #[test]
    fn an_idle_surface_with_a_still_caret_submits_nothing() {
        // The flagship property, restated now that the surface owns an
        // animation clock. `advance` is called every frame whether or not
        // anything is moving, so it is the obvious place for the zero-idle
        // guarantee to be lost.
        let mut s = surface("idle-caret");
        settle(&mut s);

        let before = s.stats().submitted;
        for _ in 0..1200 {
            s.advance(Duration::from_millis(8));
            assert!(!s.scheduler().is_dirty(), "an idle surface asked for a frame");
        }
        assert_eq!(s.stats().submitted, before, "an idle surface submitted a command buffer");
        assert_eq!(s.scheduler().animations(), 0);
    }

    #[test]
    fn the_caret_animates_to_a_new_cell_and_then_stops() {
        let mut s = surface("caret-move");
        settle(&mut s);
        assert_eq!(s.scheduler().animations(), 0);

        // Move the cursor the way the shell would, then let the clock run.
        s.write_input(b"echo hello");
        for _ in 0..200 {
            s.pump();
            if s.cursor_column() > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(s.cursor_column() > 0, "the shell never echoed anything back");

        s.advance(Duration::from_millis(8));
        assert!(
            s.scheduler().animations() > 0,
            "the caret moved without registering an animation, so nothing will draw it"
        );

        let frames = settle(&mut s);
        assert!(frames > 1, "the caret arrived instantly under the default style");
        assert_eq!(s.scheduler().animations(), 0, "the caret leaked an animation");
    }

    #[test]
    fn the_animation_count_is_balanced_however_often_the_clock_runs() {
        // `sync_animation` is called from several places, and an unbalanced
        // begin/end there is invisible until a window has been open for an
        // hour and is drawing at 120 Hz for no reason.
        let mut s = surface("caret-balance");
        settle(&mut s);
        for _ in 0..50 {
            s.advance(Duration::ZERO);
            s.advance(Duration::from_millis(8));
        }
        assert_eq!(s.scheduler().animations(), 0);
    }

    #[test]
    fn switching_to_a_non_interpolating_style_lands_the_caret_rather_than_stranding_it() {
        // Nothing runs to finish an in-flight animation once the style that
        // was driving it is gone, so the caret has to be put down immediately.
        let mut s = surface("caret-style");
        settle(&mut s);

        s.set_motion(MotionSettings { style: MotionStyle::Snap, ..*s.motion() });
        settle(&mut s);
        assert_eq!(s.scheduler().animations(), 0);
        assert_eq!(s.motion().style, MotionStyle::Snap);
    }

    #[test]
    fn motion_settings_are_clamped_when_they_reach_the_surface() {
        let mut s = surface("caret-clamp");
        s.set_motion(MotionSettings { speed: 0.0, intensity: 9.0, ..*s.motion() });
        assert!(s.motion().speed >= 0.25);
        assert_eq!(s.motion().intensity, 1.0);
    }

    #[test]
    fn re_selecting_the_current_theme_starts_no_fade() {
        let mut s = surface("theme-same");
        settle(&mut s);
        let id = s.target_theme().id.clone();
        assert!(s.set_theme(&id));
        assert_eq!(s.scheduler().animations(), 0, "a no-op theme change started an animation");
    }

    #[test]
    fn an_unknown_theme_is_refused_rather_than_leaving_the_surface_half_themed() {
        let mut s = surface("bad-theme");
        let before = s.theme().id.clone();
        assert!(!s.set_theme("no-such-theme"));
        assert_eq!(s.theme().id, before);
    }

    #[test]
    fn resizing_reflows_the_grid_and_schedules_a_frame() {
        let mut s = surface("resize");
        let (cols, _) = s.session().dimensions();
        s.resize((400, 480), 2.0);
        let (narrower, _) = s.session().dimensions();
        assert!(narrower < cols, "{narrower} is not narrower than {cols}");
        assert!(s.scheduler().is_dirty());
    }

    #[test]
    fn a_resize_to_the_same_size_does_not_schedule_anything() {
        let mut s = surface("resize-noop");
        while s.scheduler().is_dirty() {
            s.scheduler().begin_frame();
            s.scheduler().end_frame();
        }
        s.resize((800, 480), 2.0);
        assert!(!s.scheduler().is_dirty(), "a no-op resize woke the renderer");
    }

    #[test]
    fn an_overlay_takes_the_keyboard_and_gives_it_back() {
        // Without this, typing a search query would also type it into the
        // shell — the single most confusing failure an overlay can have.
        let mut s = surface("overlay-focus");
        assert!(!s.overlay_has_focus());

        s.toggle_palette();
        assert!(s.overlay_has_focus());
        assert!(s.palette().is_open());

        assert!(s.close_overlays());
        assert!(!s.overlay_has_focus());
        assert!(!s.close_overlays(), "closing nothing must report nothing happened");
    }

    #[test]
    fn opening_one_overlay_closes_the_other() {
        let mut s = surface("overlay-exclusive");
        s.toggle_palette();
        s.toggle_find();
        assert!(s.find().is_open());
        assert!(!s.palette().is_open(), "two overlays had the keyboard at once");
    }

    #[test]
    fn typing_into_the_palette_narrows_it_and_enter_dispatches() {
        let mut s = surface("palette-type");
        s.toggle_palette();
        for ch in "quartz".chars() {
            assert!(s.overlay_char(ch));
        }
        let id = s.overlay_accept().expect("the palette selected nothing");
        assert_eq!(id, "theme.quartz");
        assert!(s.dispatch(&id), "the theme action was not recognised");
        assert_eq!(s.target_theme().id, "quartz");
        assert!(!s.palette().is_open(), "accepting did not close the palette");
    }

    #[test]
    fn an_unimplemented_action_reports_itself_rather_than_doing_nothing_quietly() {
        let mut s = surface("palette-unimplemented");
        assert!(!s.dispatch("settings.fx.depth"));
        assert!(!s.dispatch("no.such.action"));
    }

    #[test]
    fn find_searches_the_real_scrollback() {
        const TOKEN: &str = "micafindprobetoken";
        let mut s = surface("find");
        s.write_input(format!("echo {TOKEN}\n").as_bytes());

        // Wait for the shell to actually print it. The query is typed once,
        // afterwards: `Find` deliberately remembers its query across close and
        // reopen, so typing it on every retry would concatenate it with itself.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        let mut printed = false;
        while std::time::Instant::now() < deadline && !printed {
            s.pump();
            let (top, bottom) = s.session().line_bounds();
            printed = (top..=bottom)
                .filter_map(|l| s.session().line_text(l))
                // Twice: once as the echoed command line, once as its output.
                .filter(|text| text.contains(TOKEN))
                .count()
                >= 2;
        }
        assert!(printed, "the shell never printed the probe token");

        s.toggle_find();
        for ch in TOKEN.chars() {
            assert!(s.overlay_char(ch));
        }
        assert!(!s.find().search().is_empty(), "find located nothing");
        assert_eq!(s.find().query(), TOKEN);
        assert!(s.find().status().contains('/'), "status was {:?}", s.find().status());
    }

    #[test]
    fn opening_an_overlay_repaints_the_rows_beneath_it() {
        let mut s = surface("overlay-damage");
        while s.scheduler().is_dirty() {
            s.scheduler().begin_frame();
            s.scheduler().end_frame();
        }
        s.toggle_palette();
        assert!(s.scheduler().is_dirty(), "the overlay did not schedule a frame");
    }

    #[test]
    fn losing_focus_schedules_a_frame_but_only_once() {
        let mut s = surface("focus");
        while s.scheduler().is_dirty() {
            s.scheduler().begin_frame();
            s.scheduler().end_frame();
        }
        s.set_focused(false);
        assert!(s.scheduler().is_dirty());

        s.scheduler().begin_frame();
        s.scheduler().end_frame();
        s.set_focused(false);
        assert!(!s.scheduler().is_dirty(), "an unchanged focus state woke the renderer");
    }

    #[test]
    fn a_new_pane_animates_in_and_then_the_window_goes_quiet_again() {
        // The house rule: an animation is a counter that has to come back to
        // zero. A split that left the scheduler animating would keep the GPU
        // awake for as long as the window was open.
        let mut s = surface("split-motion");
        let idle = s.scheduler().animations();
        assert!(s.split(Direction::Right), "the split was refused");

        assert!(s.split_progress().is_some(), "the new pane arrived with no motion at all");
        assert!(s.scheduler().animations() > idle, "the arrival was never registered");

        let mut seen = vec![s.split_progress().unwrap()];
        for _ in 0..40 {
            s.advance(Duration::from_millis(16));
            if let Some(t) = s.split_progress() {
                seen.push(t);
            }
        }
        assert!(seen.len() > 2, "the motion finished in a single frame");
        assert!(
            seen.windows(2).all(|w| w[1] >= w[0]),
            "the pane faded backwards at some point: {seen:?}"
        );
        assert!(*seen.last().unwrap() > 0.9, "the pane never reached full opacity");
        assert_eq!(s.split_progress(), None, "the arrival never finished");
        assert_eq!(s.scheduler().animations(), idle, "the animation count leaked");
    }

    #[test]
    fn reduce_motion_collapses_the_arrival_rather_than_skipping_it() {
        // Reduce Motion is a system preference about vestibular safety, not a
        // preference for a different feature. The pane still arrives; it just
        // does not travel.
        fn frames_to_arrive(s: &mut Surface) -> usize {
            let mut frames = 0;
            while s.split_progress().is_some() && frames < 200 {
                s.advance(Duration::from_millis(16));
                frames += 1;
            }
            frames
        }

        let mut plain = surface("split-plain");
        assert!(plain.split(Direction::Down));
        let ordinary = frames_to_arrive(&mut plain);

        let mut s = surface("split-reduce");
        s.set_motion(MotionSettings { reduce: true, ..*s.motion() });
        assert!(s.split(Direction::Down));
        assert!(s.split_progress().is_some(), "Reduce Motion skipped the arrival entirely");
        let reduced = frames_to_arrive(&mut s);

        assert!(
            reduced < ordinary,
            "Reduce Motion took {reduced} frames against the usual {ordinary}"
        );
        assert_eq!(s.pane_count(), 2);
    }

    #[test]
    fn a_pane_too_small_to_split_refuses_and_keeps_the_pane_it_has() {
        // A window this narrow cannot hold two panes. The refusal has to leave
        // the one shell that is there alone — half-applying it would leave a
        // layout with a pane in it and no session behind it.
        let root = temp_root("too-small");
        let mut s = Surface::open(Settings::default(), (240, 480), 2.0, root.clone(), None)
            .expect("a surface should open on this machine");
        s.set_settings_path(root.join("settings.toml"));
        assert!(!s.split(Direction::Right), "a 15-column window split in two");
        assert_eq!(s.pane_count(), 1);
        assert!(s.dispatch("pane.split_right") || s.pane_count() == 1);
    }

    #[test]
    fn the_pointer_decides_which_pane_a_scroll_reaches() {
        // Scrolling the focused pane while the mouse is over another one reads
        // as the gesture being ignored.
        let mut s = surface("pointer");
        assert!(s.split(Direction::Right));
        let (w, h) = (s.cell_width_points(), s.cell_height_points());

        let left = s.pane_at(w * 2.0, h * 2.0).expect("no pane at the left edge");
        let right = s
            .pane_at(s.viewport_points().0 - w * 2.0, h * 2.0)
            .expect("no pane at the right edge");
        assert_ne!(left, right, "both ends of the window resolved to one pane");

        // Focus is on the new pane; the pointer is over the other one.
        assert!(s.focus_pane_at(w * 2.0, h * 2.0), "clicking the other pane did not move focus");
        assert!(!s.focus_pane_at(w * 2.0, h * 2.0), "clicking the focused pane moved focus again");
    }
}
