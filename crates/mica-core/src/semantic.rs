//! OSC 133 semantic prompts, and the command blocks built from them.
//!
//! A block is one command, its output, and its exit status, tracked as a single
//! region with a measured wall-clock duration. The shell tells us where the
//! boundaries are — we do not guess by parsing prompts, which is why this works
//! identically under zsh, bash, and fish.
//!
//! ```text
//! OSC 133 ; A          prompt start
//! OSC 133 ; B          command start (end of prompt, start of user input)
//! OSC 133 ; C          output start  (user pressed return)
//! OSC 133 ; D ; <exit> command done
//! OSC 7  ; file://…    working directory
//! ```

use std::time::{SystemTime, UNIX_EPOCH};

/// Events the VT layer hands up. Deliberately flat: the backend emits them,
/// [`BlockTracker`] gives them meaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticEvent {
    PromptStart,
    CommandStart,
    /// Output began. The shell may include the command text via
    /// `OSC 133 ; C ; cmd=…`, but most do not, so it is optional.
    OutputStart { command: Option<String> },
    CommandDone { exit: Option<i32> },
    /// OSC 7 — the child reporting its working directory.
    Cwd(String),
    /// OSC 0 / OSC 2.
    Title(String),
    /// OSC 9 or OSC 777 — surfaced as a real macOS notification.
    Notification { title: Option<String>, body: String },
    /// OSC 52 — the child asking to write the system clipboard.
    ClipboardWrite(String),
    Bell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockStatus {
    /// Prompt is up, the user is typing.
    Prompting,
    /// The command is running.
    Running,
    Succeeded,
    /// Keeps a red gutter mark until cleared.
    Failed(i32),
    /// The command finished but the shell reported no status.
    Unknown,
}

impl BlockStatus {
    pub fn is_failure(self) -> bool {
        matches!(self, BlockStatus::Failed(_))
    }
}

/// One tracked command region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub id: u64,
    /// Viewport-independent absolute row where the prompt began.
    pub start_row: u64,
    /// Absolute row where output began, once it has.
    pub output_row: Option<u64>,
    /// Absolute row after the last output line, once the block is done.
    pub end_row: Option<u64>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub status: BlockStatus,
    pub started_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub folded: bool,
}

impl Block {
    fn new(id: u64, start_row: u64, cwd: Option<String>) -> Block {
        Block {
            id,
            start_row,
            output_row: None,
            end_row: None,
            command: None,
            cwd,
            status: BlockStatus::Prompting,
            started_ms: None,
            duration_ms: None,
            folded: false,
        }
    }
}

/// Wall-clock source, injectable so block-duration tests are deterministic.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// Turns a stream of [`SemanticEvent`] into blocks.
pub struct BlockTracker {
    blocks: Vec<Block>,
    next_id: u64,
    cwd: Option<String>,
    clock: Box<dyn Clock>,
    /// Ring capacity — blocks are dropped from the front along with scrollback.
    capacity: usize,
}

impl std::fmt::Debug for BlockTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockTracker")
            .field("blocks", &self.blocks.len())
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

impl BlockTracker {
    pub fn new(capacity: usize) -> BlockTracker {
        BlockTracker::with_clock(capacity, Box::new(SystemClock))
    }

    pub fn with_clock(capacity: usize, clock: Box<dyn Clock>) -> BlockTracker {
        BlockTracker { blocks: Vec::new(), next_id: 1, cwd: None, clock, capacity }
    }

    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    fn current_mut(&mut self) -> Option<&mut Block> {
        self.blocks.last_mut()
    }

    /// Feeds one event. `row` is the absolute row the terminal cursor was on
    /// when it arrived — the caller knows this, the tracker does not.
    pub fn apply(&mut self, event: SemanticEvent, row: u64) {
        match event {
            SemanticEvent::PromptStart => {
                // A prompt with an unfinished block above it means the shell
                // never sent D — Ctrl-C, or a shell that only implements half
                // the protocol. Close it as unknown rather than leaking it.
                if let Some(open) = self.current_mut() {
                    if matches!(open.status, BlockStatus::Running | BlockStatus::Prompting) {
                        open.status = BlockStatus::Unknown;
                        open.end_row = Some(row);
                    }
                }
                let id = self.next_id;
                self.next_id += 1;
                let cwd = self.cwd.clone();
                self.blocks.push(Block::new(id, row, cwd));
                if self.blocks.len() > self.capacity {
                    self.blocks.remove(0);
                }
            }
            SemanticEvent::CommandStart => {}
            SemanticEvent::OutputStart { command } => {
                let now = self.clock.now_ms();
                if let Some(b) = self.current_mut() {
                    b.output_row = Some(row);
                    b.status = BlockStatus::Running;
                    b.started_ms = Some(now);
                    if command.is_some() {
                        b.command = command;
                    }
                }
            }
            SemanticEvent::CommandDone { exit } => {
                let now = self.clock.now_ms();
                if let Some(b) = self.current_mut() {
                    b.end_row = Some(row);
                    b.duration_ms = b.started_ms.map(|s| now.saturating_sub(s));
                    b.status = match exit {
                        Some(0) => BlockStatus::Succeeded,
                        Some(code) => BlockStatus::Failed(code),
                        None => BlockStatus::Unknown,
                    };
                }
            }
            SemanticEvent::Cwd(path) => {
                self.cwd = Some(path.clone());
                if let Some(b) = self.current_mut() {
                    if b.status == BlockStatus::Prompting {
                        b.cwd = Some(path);
                    }
                }
            }
            // Not block-shaped; the session layer handles these.
            SemanticEvent::Title(_)
            | SemanticEvent::Notification { .. }
            | SemanticEvent::ClipboardWrite(_)
            | SemanticEvent::Bell => {}
        }
    }

    /// Records the command line the user actually ran. The shell rarely reports
    /// it, so the session reads it out of the grid between the B and C markers.
    pub fn set_current_command(&mut self, command: String) {
        if let Some(b) = self.current_mut() {
            if b.command.is_none() {
                b.command = Some(command);
            }
        }
    }

    pub fn next_block_after(&self, row: u64) -> Option<&Block> {
        self.blocks.iter().find(|b| b.start_row > row)
    }

    pub fn previous_block_before(&self, row: u64) -> Option<&Block> {
        self.blocks.iter().rev().find(|b| b.start_row < row)
    }

    pub fn toggle_fold(&mut self, id: u64) {
        if let Some(b) = self.blocks.iter_mut().find(|b| b.id == id) {
            b.folded = !b.folded;
        }
    }
}

/// Parses the payload of an `OSC 133` sequence (everything after `133;`).
pub fn parse_osc133(payload: &str) -> Option<SemanticEvent> {
    let mut parts = payload.split(';');
    let kind = parts.next()?;
    match kind {
        "A" => Some(SemanticEvent::PromptStart),
        "B" => Some(SemanticEvent::CommandStart),
        "C" => {
            let command = parts.find_map(|p| p.strip_prefix("cmd=")).map(str::to_owned);
            Some(SemanticEvent::OutputStart { command })
        }
        "D" => {
            // `D` alone means "done, status unknown"; `D;<n>` carries it.
            let exit = parts.next().and_then(|s| s.parse::<i32>().ok());
            Some(SemanticEvent::CommandDone { exit })
        }
        _ => None,
    }
}

/// Parses `OSC 7` — `file://<host>/<path>`.
pub fn parse_osc7(payload: &str) -> Option<SemanticEvent> {
    let rest = payload.strip_prefix("file://")?;
    let path = rest.find('/').map(|i| &rest[i..])?;
    Some(SemanticEvent::Cwd(percent_decode(path)))
}

/// Scans the PTY byte stream for the OSC sequences Mica cares about.
///
/// Why a separate scanner rather than a VT handler callback: neither backend
/// surfaces OSC 133 on its own — alacritty has no notion of semantic prompts,
/// and libghostty routes them through a different channel. One sniffer sitting
/// in front of both is ~100 lines and behaves identically under each, which is
/// exactly the property the Phase 2 exit test demands.
///
/// It is a *sniffer*, not a parser: bytes are passed through to the real VT
/// implementation untouched. It only needs to recognise OSC introducers well
/// enough to extract payloads, and it keeps its state across chunk boundaries
/// so a sequence split across two PTY reads still resolves.
#[derive(Debug, Default)]
pub struct OscSniffer {
    state: SnifferState,
    buf: Vec<u8>,
    events: Vec<SemanticEvent>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum SnifferState {
    #[default]
    Ground,
    /// Saw `ESC`.
    Escape,
    /// Inside an OSC payload.
    Osc,
    /// Saw `ESC` while inside an OSC payload — a String Terminator if the next
    /// byte is `\`.
    OscEscape,
}

/// A clipboard write can legitimately be large; anything past this is a
/// runaway and is dropped rather than grown without bound.
const MAX_OSC_PAYLOAD: usize = 64 * 1024;

impl OscSniffer {
    pub fn new() -> OscSniffer {
        OscSniffer::default()
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            match self.state {
                SnifferState::Ground => match b {
                    0x1B => self.state = SnifferState::Escape,
                    0x07 => self.events.push(SemanticEvent::Bell),
                    // 8-bit OSC introducer.
                    0x9D => {
                        self.buf.clear();
                        self.state = SnifferState::Osc;
                    }
                    _ => {}
                },
                SnifferState::Escape => match b {
                    b']' => {
                        self.buf.clear();
                        self.state = SnifferState::Osc;
                    }
                    // A second ESC restarts the escape; anything else is a
                    // sequence we do not care about.
                    0x1B => {}
                    _ => self.state = SnifferState::Ground,
                },
                SnifferState::Osc => match b {
                    0x07 => self.finish(),
                    0x1B => self.state = SnifferState::OscEscape,
                    0x9C => self.finish(),
                    _ => {
                        if self.buf.len() < MAX_OSC_PAYLOAD {
                            self.buf.push(b);
                        }
                    }
                },
                SnifferState::OscEscape => {
                    if b == b'\\' {
                        self.finish();
                    } else {
                        // Not a String Terminator after all — the ESC was part
                        // of the payload, which is malformed. Abandon it.
                        self.buf.clear();
                        self.state = SnifferState::Ground;
                    }
                }
            }
        }
    }

    fn finish(&mut self) {
        let payload = std::mem::take(&mut self.buf);
        self.state = SnifferState::Ground;
        let Ok(text) = String::from_utf8(payload) else { return };
        if let Some(event) = parse_osc(&text) {
            self.events.push(event);
        }
    }

    pub fn drain(&mut self) -> Vec<SemanticEvent> {
        std::mem::take(&mut self.events)
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// Dispatches a complete OSC payload (`<number>;<rest>`).
///
/// OSC 52 is deliberately absent: both backends already handle clipboard
/// requests through their own event channel, and duplicating it here would
/// write the clipboard twice.
pub fn parse_osc(payload: &str) -> Option<SemanticEvent> {
    let (number, rest) = match payload.split_once(';') {
        Some((n, r)) => (n, r),
        None => (payload, ""),
    };
    match number {
        "0" | "2" => Some(SemanticEvent::Title(rest.to_owned())),
        "7" => parse_osc7(rest),
        "9" => (!rest.is_empty())
            .then(|| SemanticEvent::Notification { title: None, body: rest.to_owned() }),
        // OSC 777 is a multiplexed namespace; `notify` is the only member.
        "777" => {
            let body = rest.strip_prefix("notify;")?;
            let (title, text) = match body.split_once(';') {
                Some((t, b)) => (Some(t.to_owned()), b.to_owned()),
                None => (None, body.to_owned()),
            };
            Some(SemanticEvent::Notification { title, body: text })
        }
        "133" => parse_osc133(rest),
        _ => None,
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    #[derive(Default)]
    struct FakeClock(Arc<AtomicU64>);

    impl Clock for FakeClock {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    fn tracker() -> (BlockTracker, Arc<AtomicU64>) {
        let t = Arc::new(AtomicU64::new(0));
        (BlockTracker::with_clock(64, Box::new(FakeClock(t.clone()))), t)
    }

    #[test]
    fn parses_the_full_osc133_set() {
        assert_eq!(parse_osc133("A"), Some(SemanticEvent::PromptStart));
        assert_eq!(parse_osc133("B"), Some(SemanticEvent::CommandStart));
        assert_eq!(parse_osc133("C"), Some(SemanticEvent::OutputStart { command: None }));
        assert_eq!(parse_osc133("D;0"), Some(SemanticEvent::CommandDone { exit: Some(0) }));
        assert_eq!(parse_osc133("D;127"), Some(SemanticEvent::CommandDone { exit: Some(127) }));
        assert_eq!(parse_osc133("D"), Some(SemanticEvent::CommandDone { exit: None }));
    }

    #[test]
    fn ignores_an_unknown_osc133_kind_rather_than_guessing() {
        assert_eq!(parse_osc133("Z;1"), None);
    }

    #[test]
    fn extracts_the_command_when_the_shell_bothers_to_send_it() {
        assert_eq!(
            parse_osc133("C;cmd=cargo build"),
            Some(SemanticEvent::OutputStart { command: Some("cargo build".into()) })
        );
    }

    #[test]
    fn parses_osc7_and_decodes_percent_escapes() {
        assert_eq!(
            parse_osc7("file://host/Users/me/my%20code"),
            Some(SemanticEvent::Cwd("/Users/me/my code".into()))
        );
    }

    #[test]
    fn a_successful_command_yields_one_succeeded_block() {
        let (mut t, clock) = tracker();
        t.apply(SemanticEvent::PromptStart, 10);
        t.apply(SemanticEvent::CommandStart, 10);
        clock.store(1_000, Ordering::SeqCst);
        t.apply(SemanticEvent::OutputStart { command: Some("true".into()) }, 11);
        clock.store(1_042, Ordering::SeqCst);
        t.apply(SemanticEvent::CommandDone { exit: Some(0) }, 12);

        assert_eq!(t.blocks().len(), 1);
        let b = &t.blocks()[0];
        assert_eq!(b.status, BlockStatus::Succeeded);
        assert_eq!(b.command.as_deref(), Some("true"));
        assert_eq!(b.duration_ms, Some(42));
    }

    #[test]
    fn a_failing_command_records_its_exit_code() {
        let (mut t, clock) = tracker();
        t.apply(SemanticEvent::PromptStart, 0);
        clock.store(5, Ordering::SeqCst);
        t.apply(SemanticEvent::OutputStart { command: Some("false".into()) }, 1);
        clock.store(9, Ordering::SeqCst);
        t.apply(SemanticEvent::CommandDone { exit: Some(1) }, 2);

        let b = &t.blocks()[0];
        assert_eq!(b.status, BlockStatus::Failed(1));
        assert!(b.status.is_failure());
        assert_eq!(b.duration_ms, Some(4));
    }

    #[test]
    fn an_interrupted_block_is_closed_when_the_next_prompt_arrives() {
        let (mut t, _) = tracker();
        t.apply(SemanticEvent::PromptStart, 0);
        t.apply(SemanticEvent::OutputStart { command: Some("sleep 100".into()) }, 1);
        // Ctrl-C: the shell reprompts without ever sending D.
        t.apply(SemanticEvent::PromptStart, 2);

        assert_eq!(t.blocks().len(), 2);
        assert_eq!(t.blocks()[0].status, BlockStatus::Unknown);
        assert_eq!(t.blocks()[0].end_row, Some(2));
    }

    #[test]
    fn cwd_attaches_to_the_block_being_prompted() {
        let (mut t, _) = tracker();
        t.apply(SemanticEvent::Cwd("/tmp".into()), 0);
        t.apply(SemanticEvent::PromptStart, 1);
        assert_eq!(t.blocks()[0].cwd.as_deref(), Some("/tmp"));
        assert_eq!(t.cwd(), Some("/tmp"));
    }

    #[test]
    fn block_history_is_bounded() {
        let mut t = BlockTracker::with_clock(3, Box::new(SystemClock));
        for row in 0..10 {
            t.apply(SemanticEvent::PromptStart, row);
        }
        assert_eq!(t.blocks().len(), 3);
        // The survivors are the most recent three.
        assert_eq!(t.blocks()[0].start_row, 7);
    }

    fn sniff(input: &[u8]) -> Vec<SemanticEvent> {
        let mut s = OscSniffer::new();
        s.feed(input);
        s.drain()
    }

    #[test]
    fn sniffer_finds_osc133_terminated_by_bel() {
        assert_eq!(sniff(b"\x1b]133;A\x07"), vec![SemanticEvent::PromptStart]);
    }

    #[test]
    fn sniffer_finds_osc133_terminated_by_string_terminator() {
        assert_eq!(
            sniff(b"\x1b]133;D;1\x1b\\"),
            vec![SemanticEvent::CommandDone { exit: Some(1) }]
        );
    }

    #[test]
    fn sniffer_survives_a_sequence_split_across_two_reads() {
        let mut s = OscSniffer::new();
        s.feed(b"\x1b]133;D");
        assert!(s.is_empty(), "an incomplete sequence must not fire early");
        s.feed(b";0\x07");
        assert_eq!(s.drain(), vec![SemanticEvent::CommandDone { exit: Some(0) }]);
    }

    #[test]
    fn sniffer_ignores_ordinary_text_and_csi_sequences() {
        assert_eq!(sniff(b"hello \x1b[31mworld\x1b[0m\n"), vec![]);
    }

    #[test]
    fn sniffer_reports_a_bare_bell_but_not_an_osc_terminator() {
        assert_eq!(sniff(b"\x07"), vec![SemanticEvent::Bell]);
        assert_eq!(sniff(b"\x1b]0;title\x07"), vec![SemanticEvent::Title("title".into())]);
    }

    #[test]
    fn sniffer_parses_both_notification_forms() {
        assert_eq!(
            sniff(b"\x1b]9;build finished\x07"),
            vec![SemanticEvent::Notification { title: None, body: "build finished".into() }]
        );
        assert_eq!(
            sniff(b"\x1b]777;notify;Build;done\x07"),
            vec![SemanticEvent::Notification {
                title: Some("Build".into()),
                body: "done".into()
            }]
        );
    }

    #[test]
    fn sniffer_drops_an_oversized_payload_without_growing_forever() {
        let mut s = OscSniffer::new();
        s.feed(b"\x1b]133;");
        s.feed(&vec![b'x'; 256 * 1024]);
        s.feed(b"\x07");
        assert!(s.drain().is_empty());
    }

    #[test]
    fn sniffer_does_not_treat_osc52_as_its_own_business() {
        // The backend's clipboard channel owns this; handling it twice would
        // write the pasteboard twice.
        assert_eq!(sniff(b"\x1b]52;c;aGk=\x07"), vec![]);
    }

    #[test]
    fn navigation_finds_the_neighbouring_blocks() {
        let (mut t, _) = tracker();
        for row in [0u64, 10, 20] {
            t.apply(SemanticEvent::PromptStart, row);
        }
        assert_eq!(t.next_block_after(0).map(|b| b.start_row), Some(10));
        assert_eq!(t.previous_block_before(20).map(|b| b.start_row), Some(10));
        assert!(t.next_block_after(20).is_none());
    }
}
