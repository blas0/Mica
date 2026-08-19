//! [`TerminalCore`] over `alacritty_terminal`.
//!
//! This is the fallback backend, and it exists for three reasons: libghostty-vt
//! has never been tagged with a version, its header warns that breaking changes
//! are expected, and it makes Zig a hard build dependency. A backend that is
//! only *nearly* working when you need it is not a fallback, so this one stays
//! green in CI from the first commit.
//!
//! Two things are Mica's rather than alacritty's:
//!
//! - **Damage is filtered through [`Mirror`].** `Term::damage` marks the cursor
//!   row damaged on every call, unconditionally. Forwarded verbatim that would
//!   wake the renderer forever and the zero-frame-when-idle claim would be
//!   false. The mirror compares content and only reports rows that actually
//!   changed.
//! - **OSC 133 is sniffed upstream.** alacritty has no notion of semantic
//!   prompts; [`OscSniffer`] sees the bytes before the parser does.

use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event as AlacEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as AlacPoint, Side};
use alacritty_terminal::selection::{Selection as AlacSelection, SelectionType};
use alacritty_terminal::term::cell::{Cell as AlacCell, Flags as AlacFlags};
use alacritty_terminal::term::{
    viewport_to_point, Config as TermConfig, Term, TermDamage, TermMode,
};
use alacritty_terminal::vte::ansi::{
    Color as AlacColor, CursorShape as AlacCursorShape, NamedColor, Processor,
};

use crate::backend::mirror::Mirror;
use crate::backend::{CursorShape, CursorState, Point, RowRef, Selection, TerminalCore};
use crate::cell::{Cell, CellContent, CellFlags, Color, NO_EXTRA};
use crate::semantic::{OscSniffer, SemanticEvent};
use crate::sidetable::{Extras, SideTables};

/// Grid dimensions in the shape alacritty wants.
struct Size {
    columns: usize,
    screen_lines: usize,
    total_lines: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.total_lines
    }
    fn screen_lines(&self) -> usize {
        self.screen_lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

/// Everything `Term` pushes out of band. `EventListener::send_event` takes
/// `&self`, so this has to be shared and interior-mutable.
#[derive(Default)]
struct Sink {
    replies: Vec<u8>,
    events: Vec<SemanticEvent>,
    title: Option<String>,
}

#[derive(Clone, Default)]
struct SinkHandle(Arc<Mutex<Sink>>);

impl EventListener for SinkHandle {
    fn send_event(&self, event: AlacEvent) {
        let Ok(mut sink) = self.0.lock() else { return };
        match event {
            AlacEvent::PtyWrite(text) => sink.replies.extend_from_slice(text.as_bytes()),
            AlacEvent::Title(title) => {
                sink.title = Some(title.clone());
                sink.events.push(SemanticEvent::Title(title));
            }
            AlacEvent::ResetTitle => {
                sink.title = Some(String::new());
                sink.events.push(SemanticEvent::Title(String::new()));
            }
            AlacEvent::ClipboardStore(_, text) => {
                sink.events.push(SemanticEvent::ClipboardWrite(text))
            }
            AlacEvent::Bell => sink.events.push(SemanticEvent::Bell),
            _ => {}
        }
    }
}

pub struct AlacrittyCore {
    term: Term<SinkHandle>,
    parser: Processor,
    sink: SinkHandle,
    sniffer: OscSniffer,
    mirror: Mirror,
    tables: SideTables,
    scrollback: u32,
}

impl AlacrittyCore {
    /// Pulls alacritty's damage into the mirror, translating only the rows it
    /// names. This is the only place alacritty's cell layout is read.
    fn sync_damage(&mut self) {
        let cols = self.mirror.dimensions().0 as usize;
        let display_offset = self.term.grid().display_offset();

        let lines: Vec<usize> = match self.term.damage() {
            TermDamage::Full => (0..self.mirror.dimensions().1 as usize).collect(),
            TermDamage::Partial(iter) => iter.map(|d| d.line).collect(),
        };
        self.term.reset_damage();

        let mut scratch: Vec<Cell> = vec![Cell::EMPTY; cols];
        for viewport_line in lines {
            if viewport_line >= self.mirror.dimensions().1 as usize {
                continue;
            }
            let point = viewport_to_point(display_offset, AlacPoint::new(viewport_line, Column(0)));
            let row = &self.term.grid()[point.line];

            let mut wrapped = false;
            for col in 0..cols {
                let src = &row[Column(col)];
                if src.flags.contains(AlacFlags::WRAPLINE) {
                    wrapped = true;
                }
                scratch[col] = translate_cell(src, &mut self.tables);
            }
            self.mirror.put_row(viewport_line as u16, &scratch, wrapped);
        }
    }

    fn drain_sink(&mut self) -> Vec<SemanticEvent> {
        let Ok(mut sink) = self.sink.0.lock() else { return Vec::new() };
        std::mem::take(&mut sink.events)
    }
}

fn translate_color(color: AlacColor) -> Color {
    match color {
        // The theme resolves these at render time; baking them in here would
        // make a live theme cross-fade impossible.
        AlacColor::Named(NamedColor::Foreground | NamedColor::Background) => Color::DEFAULT,
        AlacColor::Named(named) => {
            let index = named as usize;
            if index < 256 {
                Color::palette(index as u8)
            } else {
                Color::DEFAULT
            }
        }
        AlacColor::Indexed(i) => Color::palette(i),
        AlacColor::Spec(rgb) => Color::rgb(rgb.r, rgb.g, rgb.b),
    }
}

fn translate_flags(flags: AlacFlags) -> CellFlags {
    let mut out = CellFlags::EMPTY;
    out.set(CellFlags::BOLD, flags.contains(AlacFlags::BOLD));
    out.set(CellFlags::DIM, flags.contains(AlacFlags::DIM));
    out.set(CellFlags::ITALIC, flags.contains(AlacFlags::ITALIC));
    out.set(CellFlags::INVERSE, flags.contains(AlacFlags::INVERSE));
    out.set(CellFlags::HIDDEN, flags.contains(AlacFlags::HIDDEN));
    out.set(CellFlags::STRIKETHROUGH, flags.contains(AlacFlags::STRIKEOUT));
    out.set(
        CellFlags::WIDE_SPACER,
        flags.contains(AlacFlags::WIDE_CHAR_SPACER)
            || flags.contains(AlacFlags::LEADING_WIDE_CHAR_SPACER),
    );

    // Underline style is a small enum packed into bits 8..11, not a set of
    // independent flags, so it is assigned rather than or-ed.
    let underline = if flags.contains(AlacFlags::UNDERCURL) {
        CellFlags::UNDERLINE_CURLY
    } else if flags.contains(AlacFlags::DOUBLE_UNDERLINE) {
        CellFlags::UNDERLINE_DOUBLE
    } else if flags.contains(AlacFlags::DOTTED_UNDERLINE) {
        CellFlags::UNDERLINE_DOTTED
    } else if flags.contains(AlacFlags::DASHED_UNDERLINE) {
        CellFlags::UNDERLINE_DASHED
    } else if flags.contains(AlacFlags::UNDERLINE) {
        CellFlags::UNDERLINE_SINGLE
    } else {
        CellFlags::EMPTY
    };
    out.union(underline)
}

fn translate_cell(src: &AlacCell, tables: &mut SideTables) -> Cell {
    // A grapheme cluster is the base character plus its zero-width
    // continuation. It stays in one cell; only the *storage* moves to a side
    // table, and only when there is something to store.
    let content = match src.zerowidth() {
        Some(zw) if !zw.is_empty() => {
            let mut cluster = String::with_capacity(4 + zw.len() * 4);
            cluster.push(src.c);
            cluster.extend(zw.iter().copied());
            CellContent::cluster(tables.graphemes.intern(&cluster))
        }
        _ => CellContent::scalar(src.c),
    };

    let extras = Extras {
        underline_color: src.underline_color().map(translate_color),
        hyperlink: src.hyperlink().map(|h| tables.hyperlinks.intern(h.uri())),
    };
    let extra = if extras.is_empty() { NO_EXTRA } else { tables.extras.intern(extras) };

    Cell::build(
        content,
        translate_color(src.fg),
        translate_color(src.bg),
        extra,
        translate_flags(src.flags),
        if src.flags.contains(AlacFlags::WIDE_CHAR) { 2 } else { 1 },
    )
}

impl TerminalCore for AlacrittyCore {
    fn new(cols: u16, rows: u16, scrollback: u32) -> AlacrittyCore {
        let sink = SinkHandle::default();
        let size = Size {
            columns: cols.max(1) as usize,
            screen_lines: rows.max(1) as usize,
            total_lines: rows.max(1) as usize,
        };
        let config = TermConfig { scrolling_history: scrollback as usize, ..TermConfig::default() };
        let term = Term::new(config, &size, sink.clone());

        AlacrittyCore {
            term,
            parser: Processor::new(),
            sink,
            sniffer: OscSniffer::new(),
            mirror: Mirror::new(cols, rows),
            tables: SideTables::new(),
            scrollback,
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        // Sniff before parsing: the sniffer must see the raw stream, and the
        // parser is free to swallow whatever it does not recognise.
        self.sniffer.feed(bytes);
        self.parser.advance(&mut self.term, bytes);
        self.sync_damage();
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if self.mirror.dimensions() == (cols, rows) {
            return;
        }
        self.term.resize(Size {
            columns: cols as usize,
            screen_lines: rows as usize,
            total_lines: rows as usize,
        });
        self.mirror.resize(cols, rows);
        self.sync_damage();
        // A resize invalidates the surface by definition; reflow may have moved
        // every row even where the content is unchanged.
        self.mirror.damage_all();
    }

    fn scroll_viewport(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        self.term.scroll_display(Scroll::Delta(delta));
        self.sync_damage();
        // Scrolling shifts every visible row: content-equality would wrongly
        // report "unchanged" for a screen of identical blank lines.
        self.mirror.damage_all();
    }

    fn dimensions(&self) -> (u16, u16) {
        self.mirror.dimensions()
    }

    fn dirty_rows(&self) -> impl Iterator<Item = RowRef<'_>> {
        self.mirror.dirty_rows()
    }

    fn has_damage(&self) -> bool {
        self.mirror.has_damage()
    }

    fn damage_all(&mut self) {
        self.mirror.damage_all();
    }

    fn clear_damage(&mut self) {
        self.mirror.clear_damage();
    }

    fn cursor(&self) -> CursorState {
        let point = self.term.grid().cursor.point;
        let offset = self.term.grid().display_offset() as i32;
        let style = self.term.cursor_style();
        let visible = self.term.mode().contains(TermMode::SHOW_CURSOR)
            && style.shape != AlacCursorShape::Hidden;

        let line = point.line.0 + offset;
        CursorState {
            line: line.clamp(0, i32::from(u16::MAX)) as u16,
            column: point.column.0 as u16,
            shape: match style.shape {
                AlacCursorShape::Underline => CursorShape::Underline,
                AlacCursorShape::Beam => CursorShape::Bar,
                _ => CursorShape::Block,
            },
            visible: visible && line >= 0,
            blinking: style.blinking,
        }
    }

    fn selection(&self) -> Option<Selection> {
        let range = self.term.selection.as_ref()?.to_range(&self.term)?;
        let offset = self.term.grid().display_offset() as i32;
        Some(Selection {
            start: Point::new(range.start.line.0 + offset, range.start.column.0 as u16),
            end: Point::new(range.end.line.0 + offset, range.end.column.0 as u16),
            rectangular: range.is_block,
        })
    }

    fn set_selection(&mut self, selection: Option<Selection>) {
        self.term.selection = selection.map(|s| {
            let offset = self.term.grid().display_offset();
            let ty = if s.rectangular { SelectionType::Block } else { SelectionType::Simple };
            let start = viewport_to_point(
                offset,
                AlacPoint::new(s.start.line.max(0) as usize, Column(s.start.column as usize)),
            );
            let end = viewport_to_point(
                offset,
                AlacPoint::new(s.end.line.max(0) as usize, Column(s.end.column as usize)),
            );
            let mut sel = AlacSelection::new(ty, start, Side::Left);
            sel.update(end, Side::Right);
            sel
        });
        // Selection is drawn from cell state, so the affected rows must repaint.
        self.mirror.damage_all();
    }

    fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string()
    }

    fn drain_semantic_events(&mut self) -> Vec<SemanticEvent> {
        let mut events = self.sniffer.drain();
        events.extend(self.drain_sink());
        events
    }

    fn take_replies(&mut self) -> Vec<u8> {
        let Ok(mut sink) = self.sink.0.lock() else { return Vec::new() };
        std::mem::take(&mut sink.replies)
    }

    fn side_tables(&self) -> &SideTables {
        &self.tables
    }

    fn history_len(&self) -> usize {
        self.term.grid().total_lines()
    }

    fn take_title(&mut self) -> Option<String> {
        let mut sink = self.sink.0.lock().ok()?;
        sink.title.take()
    }

    fn line_bounds(&self) -> (i32, i32) {
        let grid = self.term.grid();
        // `topmost_line` is negative by the amount of history retained.
        (grid.topmost_line().0, grid.bottommost_line().0)
    }

    fn line_text(&self, line: i32) -> Option<String> {
        let (top, bottom) = self.line_bounds();
        if line < top || line > bottom {
            return None;
        }
        let cols = self.mirror.dimensions().0 as usize;
        let start = AlacPoint::new(Line(line), Column(0));
        let end = AlacPoint::new(Line(line), Column(cols.saturating_sub(1)));
        Some(self.term.bounds_to_string(start, end))
    }
}

impl std::fmt::Debug for AlacrittyCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlacrittyCore")
            .field("dimensions", &self.mirror.dimensions())
            .field("scrollback", &self.scrollback)
            .finish_non_exhaustive()
    }
}
