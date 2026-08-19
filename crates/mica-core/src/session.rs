//! One surface: a shell on a PTY, a terminal core, and the blocks derived from
//! it.
//!
//! A session owns no UI and no renderer. It is driven by [`Session::pump`],
//! which the shell layer calls when the reader thread signals activity — never
//! on a timer. That is the top of the chain that ends in "an idle window
//! submits zero command buffers": if nothing arrives on the PTY, `pump` does
//! nothing, no row is dirtied, and the renderer is never asked to draw.

use std::io;
use std::sync::mpsc::{Receiver, TryRecvError};

use crate::backend::{Backend, CursorState, RowRef, Selection, TerminalCore};
use crate::pty::{ExitStatus, Pty, PtyConfig, PtyEvent};
use crate::semantic::{Block, BlockTracker, SemanticEvent};
use crate::settings::Settings;
use crate::sidetable::SideTables;

/// Things the session cannot act on itself and hands to the shell layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    TitleChanged(String),
    CwdChanged(String),
    Notification { title: Option<String>, body: String },
    ClipboardWrite(String),
    Bell,
    /// The child exited. The window may close, or keep the scrollback.
    Exited(Option<i32>),
}

pub struct Session {
    core: Backend,
    pty: Pty,
    rx: Receiver<PtyEvent>,
    blocks: BlockTracker,
    events: Vec<SessionEvent>,
    cols: u16,
    rows: u16,
    exited: Option<ExitStatus>,
    /// Set when the user scrolls up; cleared on new output, which is why
    /// reading a log does not fight the shell for the viewport.
    pinned_to_history: bool,
}

impl Session {
    pub fn spawn(settings: &Settings, config: PtyConfig) -> io::Result<Session> {
        Session::spawn_with_wakeup(settings, config, None)
    }

    /// The same, with a callback the reader thread fires when output arrives.
    ///
    /// The window layer passes one that hops to the main thread, so the UI
    /// pumps because the PTY produced something rather than because a timer
    /// went off.
    pub fn spawn_with_wakeup(
        settings: &Settings,
        config: PtyConfig,
        wakeup: Option<crate::pty::Wakeup>,
    ) -> io::Result<Session> {
        let (cols, rows) = (config.cols.max(1), config.rows.max(1));
        let pty = Pty::spawn(&config)?;
        // 256 chunks of up to 64 KiB is roughly 16 MB of slack before the
        // reader blocks — enough to absorb a burst, small enough that a
        // runaway process cannot exhaust memory.
        let rx = pty.reader_with_wakeup(256, wakeup)?;
        Ok(Session {
            core: Backend::new(cols, rows, settings.scrollback),
            pty,
            rx,
            blocks: BlockTracker::new(4096),
            events: Vec::new(),
            cols,
            rows,
            exited: None,
            pinned_to_history: false,
        })
    }

    /// Consumes whatever the PTY has produced.
    ///
    /// Returns `true` if anything changed — the shell layer uses this, not a
    /// display link, to decide whether to ask for a frame.
    pub fn pump(&mut self) -> bool {
        let mut received = false;
        loop {
            match self.rx.try_recv() {
                Ok(PtyEvent::Output(bytes)) => {
                    self.core.write(&bytes);
                    received = true;
                }
                Ok(PtyEvent::Hangup) | Err(TryRecvError::Disconnected) => {
                    self.finish();
                    received = true;
                    break;
                }
                Ok(PtyEvent::Error(err)) => {
                    self.events.push(SessionEvent::Notification {
                        title: Some("Terminal error".into()),
                        body: err.to_string(),
                    });
                    self.finish();
                    received = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }

        if received {
            self.absorb_semantics();
            // The terminal owes the child answers to DA1, DSR, and friends.
            let replies = self.core.take_replies();
            if !replies.is_empty() {
                let _ = self.pty.write(&replies);
            }
            // New output pulls the viewport back to the bottom, unless the
            // user is deliberately reading history.
            if !self.pinned_to_history {
                self.scroll_to_bottom();
            }
        }
        received
    }

    fn absorb_semantics(&mut self) {
        let row = self.absolute_cursor_row();
        for event in self.core.drain_semantic_events() {
            match &event {
                SemanticEvent::Title(title) => {
                    self.events.push(SessionEvent::TitleChanged(title.clone()))
                }
                SemanticEvent::Cwd(path) => {
                    self.events.push(SessionEvent::CwdChanged(path.clone()))
                }
                SemanticEvent::Notification { title, body } => {
                    self.events.push(SessionEvent::Notification {
                        title: title.clone(),
                        body: body.clone(),
                    })
                }
                SemanticEvent::ClipboardWrite(text) => {
                    self.events.push(SessionEvent::ClipboardWrite(text.clone()))
                }
                SemanticEvent::Bell => self.events.push(SessionEvent::Bell),
                _ => {}
            }
            self.blocks.apply(event, row);
        }
    }

    fn finish(&mut self) {
        if self.exited.is_some() {
            return;
        }
        let status = self.pty.wait().ok();
        self.exited = status;
        self.events.push(SessionEvent::Exited(status.and_then(ExitStatus::code)));
    }

    /// Row index counted from the oldest retained line.
    ///
    /// Once scrollback saturates this shifts as old lines are dropped, so it is
    /// a stable handle only for as long as the rows it names are still
    /// retained — which is exactly as long as a block is worth pointing at.
    fn absolute_cursor_row(&self) -> u64 {
        let history = self.core.history_len() as u64;
        let rows = self.rows as u64;
        history.saturating_sub(rows) + self.core.cursor().line as u64
    }

    pub fn write_input(&mut self, bytes: &[u8]) -> io::Result<()> {
        // Typing always returns the user to the live view; a keystroke that
        // vanished into scrollback is the single most disorienting thing a
        // terminal can do.
        if self.pinned_to_history {
            self.scroll_to_bottom();
        }
        self.pty.write(bytes)
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if (cols, rows) == (self.cols, self.rows) {
            return Ok(());
        }
        self.cols = cols;
        self.rows = rows;
        self.core.resize(cols, rows);
        self.pty.resize(cols, rows)
    }

    pub fn scroll(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        self.core.scroll_viewport(delta);
        self.pinned_to_history = delta > 0 || self.pinned_to_history;
    }

    pub fn scroll_to_bottom(&mut self) {
        if self.pinned_to_history {
            self.core.scroll_viewport(-(self.core.history_len() as i32));
        }
        self.pinned_to_history = false;
    }

    pub fn drain_events(&mut self) -> Vec<SessionEvent> {
        std::mem::take(&mut self.events)
    }

    pub fn blocks(&self) -> &[Block] {
        self.blocks.blocks()
    }

    pub fn block_tracker_mut(&mut self) -> &mut BlockTracker {
        &mut self.blocks
    }

    pub fn cwd(&self) -> Option<&str> {
        self.blocks.cwd()
    }

    pub fn dimensions(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    pub fn has_exited(&self) -> bool {
        self.exited.is_some()
    }

    // --- what the renderer sees -------------------------------------------

    pub fn dirty_rows(&self) -> impl Iterator<Item = RowRef<'_>> {
        self.core.dirty_rows()
    }

    pub fn has_damage(&self) -> bool {
        self.core.has_damage()
    }

    pub fn damage_all(&mut self) {
        self.core.damage_all();
    }

    pub fn clear_damage(&mut self) {
        self.core.clear_damage();
    }

    pub fn cursor(&self) -> CursorState {
        self.core.cursor()
    }

    pub fn selection(&self) -> Option<Selection> {
        self.core.selection()
    }

    pub fn set_selection(&mut self, selection: Option<Selection>) {
        self.core.set_selection(selection);
    }

    pub fn selection_text(&self) -> Option<String> {
        self.core.selection_text()
    }

    pub fn side_tables(&self) -> &SideTables {
        self.core.side_tables()
    }

    /// The range of absolute line indices that exist. Search only — see
    /// [`TerminalCore::line_text`].
    pub fn line_bounds(&self) -> (i32, i32) {
        self.core.line_bounds()
    }

    /// The text of one line. **Search only**; never call this per frame.
    pub fn line_text(&self, line: i32) -> Option<String> {
        self.core.line_text(line)
    }

    /// Test seam: feed bytes as if they had arrived on the PTY.
    #[cfg(test)]
    fn feed_for_test(&mut self, bytes: &[u8]) {
        self.core.write(bytes);
        self.absorb_semantics();
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("dimensions", &(self.cols, self.rows))
            .field("blocks", &self.blocks.blocks().len())
            .field("exited", &self.exited)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::BlockStatus;
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn session() -> Session {
        let config = PtyConfig {
            program: PathBuf::from("/bin/sh"),
            args: vec![OsString::from("sh")],
            cwd: None,
            env: vec![(OsString::from("PS1"), OsString::from(""))],
            env_remove: vec![OsString::from("ENV")],
            cols: 80,
            rows: 24,
        };
        Session::spawn(&Settings::default(), config).unwrap()
    }

    #[test]
    fn osc133_markers_arriving_as_output_become_blocks() {
        let mut s = session();
        s.feed_for_test(b"\x1b]133;A\x07$ false\r\n\x1b]133;C\x07");
        s.feed_for_test(b"\x1b]133;D;1\x07");

        let blocks = s.blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].status, BlockStatus::Failed(1));
    }

    #[test]
    fn a_title_sequence_surfaces_as_a_session_event() {
        let mut s = session();
        s.feed_for_test(b"\x1b]0;build\x07");
        assert!(s
            .drain_events()
            .iter()
            .any(|e| *e == SessionEvent::TitleChanged("build".into())));
    }

    #[test]
    fn an_osc7_report_updates_the_working_directory() {
        let mut s = session();
        s.feed_for_test(b"\x1b]7;file://host/tmp/work\x07");
        assert_eq!(s.cwd(), Some("/tmp/work"));
    }

    #[test]
    fn a_fresh_session_has_no_damage_once_the_first_frame_is_drawn() {
        let mut s = session();
        s.clear_damage();
        assert!(!s.has_damage(), "an idle session must not ask for a frame");
    }

    #[test]
    fn feeding_identical_content_twice_does_not_redirty() {
        let mut s = session();
        s.feed_for_test(b"steady\r\n");
        s.clear_damage();
        // Rewriting the same row with the same bytes: the cursor moved and came
        // back, the pixels did not change.
        s.feed_for_test(b"\x1b[1;1Hsteady");
        assert!(!s.has_damage(), "an unchanged repaint must not wake the renderer");
    }
}
