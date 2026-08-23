//! The backend contract — the Phase 2 exit test.
//!
//! Every assertion here is written against the [`TerminalCore`] trait and never
//! against a concrete backend, so the same file runs unchanged under
//! `--features alacritty` and under `--features ghostty`. That is the point:
//! swapping the terminal core underneath Mica must be invisible from above, and
//! the only way to keep that true is to state it as a test that both must pass.
//!
//! ```sh
//! cargo test -p mica-core --no-default-features --features alacritty
//! cargo test -p mica-core --no-default-features --features ghostty
//! ```

use mica_core::backend::{Backend, TerminalCore};
use mica_core::cell::{CellFlags, Color};
use mica_core::semantic::SemanticEvent;

/// Builds a terminal and drains the damage from construction, so each test
/// starts from a clean slate and any damage it sees is its own.
fn terminal(cols: u16, rows: u16) -> Backend {
    let mut core = Backend::new(cols, rows, 1_000);
    core.clear_damage();
    core
}

fn row_text(core: &Backend, index: u16) -> Option<String> {
    core.dirty_rows().find(|r| r.index == index).map(|r| {
        r.cells
            .iter()
            .filter(|c| !c.flags.contains(CellFlags::WIDE_SPACER))
            .filter_map(|c| c.content.as_scalar())
            .collect::<String>()
            .trim_end()
            .to_owned()
    })
}

#[test]
fn coloured_text_lands_in_one_dirty_row_with_the_right_cells() {
    // The canonical case from the build plan: a red `hello`, then reset.
    let mut core = terminal(20, 5);
    core.write(b"\x1b[31mhello\x1b[0m");

    let dirty: Vec<_> = core.dirty_rows().map(|r| r.index).collect();
    assert_eq!(dirty, vec![0], "exactly one row changed, and it is the first");

    let row = core.dirty_rows().next().unwrap();
    assert_eq!(row_text(&core, 0).as_deref(), Some("hello"));

    // ANSI red is palette index 1. It is deliberately *not* resolved to pixels
    // here — a cell stores the role, and the theme turns it into a colour at
    // render time, which is what makes a live theme cross-fade possible.
    for cell in &row.cells[..5] {
        assert_eq!(cell.fg, Color::palette(1), "each of the five glyphs is red");
        assert_eq!(cell.bg, Color::DEFAULT, "the background was never set");
    }
    assert_eq!(row.cells[5].fg, Color::DEFAULT, "the reset took effect after `hello`");
}

#[test]
fn sgr_attributes_survive_translation() {
    let mut core = terminal(20, 2);
    core.write(b"\x1b[1mB\x1b[0m\x1b[3mI\x1b[0m\x1b[4mU\x1b[0m\x1b[9mS\x1b[0m");

    let row = core.dirty_rows().next().expect("the attributes changed row 0");
    assert!(row.cells[0].flags.contains(CellFlags::BOLD));
    assert!(row.cells[1].flags.contains(CellFlags::ITALIC));
    assert!(row.cells[2].flags.contains(CellFlags::UNDERLINE_SINGLE));
    assert!(row.cells[3].flags.contains(CellFlags::STRIKETHROUGH));
}

#[test]
fn a_true_colour_sequence_is_stored_verbatim() {
    let mut core = terminal(10, 2);
    core.write(b"\x1b[38;2;10;20;30mx");
    let row = core.dirty_rows().next().unwrap();
    assert_eq!(row.cells[0].fg, Color::rgb(10, 20, 30));
}

#[test]
fn an_idle_terminal_reports_no_damage() {
    // The single most important property in the project, stated at the lowest
    // level it can be stated at. If this fails, every layer above it is
    // rendering on a timer whether it means to or not.
    let mut core = terminal(80, 24);
    core.write(b"hello\r\n");
    core.clear_damage();

    assert!(!core.has_damage());
    assert_eq!(core.dirty_rows().count(), 0);

    // Writing nothing must not change that.
    core.write(b"");
    assert!(!core.has_damage(), "an empty write woke the renderer");
}

#[test]
fn repainting_a_row_with_identical_content_does_not_dirty_it() {
    // Backends over-report: alacritty marks the cursor's row damaged on every
    // `damage()` call regardless of whether anything changed. The contract is
    // that Mica's layer filters that out.
    let mut core = terminal(20, 3);
    core.write(b"steady");
    core.clear_damage();
    core.write(b"\x1b[1;1Hsteady");
    assert!(!core.has_damage(), "an unchanged repaint must not produce a frame");
}

#[test]
fn only_the_row_that_changed_is_reported() {
    let mut core = terminal(20, 6);
    core.write(b"one\r\ntwo\r\nthree\r\n");
    core.clear_damage();

    // Overwrite the middle row only.
    core.write(b"\x1b[2;1Htwo!");
    let dirty: Vec<u16> = core.dirty_rows().map(|r| r.index).collect();
    assert_eq!(dirty, vec![1], "rows 0 and 2 were untouched and must stay clean");
}

#[test]
fn damage_all_is_the_only_way_to_get_everything() {
    // There is no `full_grid()` on the trait, on purpose. This is the escape
    // hatch, and it is explicit at the call site.
    let mut core = terminal(20, 7);
    core.write(b"x");
    core.clear_damage();
    core.damage_all();
    assert_eq!(core.dirty_rows().count(), 7);
}

#[test]
fn the_cursor_follows_the_text() {
    let mut core = terminal(20, 5);
    core.write(b"abc");
    let cursor = core.cursor();
    assert_eq!((cursor.line, cursor.column), (0, 3));
    assert!(cursor.visible);

    core.write(b"\r\n");
    assert_eq!(core.cursor().line, 1);
}

#[test]
fn hiding_the_cursor_is_honoured() {
    let mut core = terminal(20, 5);
    assert!(core.cursor().visible);
    core.write(b"\x1b[?25l");
    assert!(!core.cursor().visible);
    core.write(b"\x1b[?25h");
    assert!(core.cursor().visible);
}

#[test]
fn decscusr_selects_the_cursor_shape() {
    use mica_core::backend::CursorShape;
    let mut core = terminal(20, 5);
    core.write(b"\x1b[4 q"); // steady underline
    assert_eq!(core.cursor().shape, CursorShape::Underline);
    core.write(b"\x1b[6 q"); // steady bar
    assert_eq!(core.cursor().shape, CursorShape::Bar);
    core.write(b"\x1b[2 q"); // steady block
    assert_eq!(core.cursor().shape, CursorShape::Block);
}

#[test]
fn resizing_reflows_and_invalidates_the_surface() {
    let mut core = terminal(20, 5);
    core.write(b"hello");
    core.clear_damage();

    core.resize(40, 10);
    assert_eq!(core.dimensions(), (40, 10));
    assert_eq!(core.dirty_rows().count(), 10, "a resize invalidates every row");
}

#[test]
fn a_wide_character_occupies_one_cell_and_a_spacer() {
    let mut core = terminal(10, 2);
    core.write("\u{4E16}\u{754C}".as_bytes()); // two CJK ideographs

    let row = core.dirty_rows().next().unwrap();
    assert_eq!(row.cells[0].width, 2);
    assert_eq!(row.cells[0].content.as_scalar(), Some('\u{4E16}'));
    assert!(row.cells[1].flags.contains(CellFlags::WIDE_SPACER));
    assert_eq!(row.cells[2].content.as_scalar(), Some('\u{754C}'));
}

#[test]
fn a_grapheme_cluster_occupies_exactly_one_cell() {
    // A family emoji is five scalars joined by ZWJ. The claim is that it is one
    // cell drawn as one colour glyph — not a fallback box, and not two halves.
    let mut core = terminal(10, 2);
    core.write("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}".as_bytes());

    let row = core.dirty_rows().next().unwrap();
    let cell = row.cells[0];
    let id = cell.content.as_cluster().expect("a ZWJ sequence must intern as a cluster");
    let text = core.side_tables().graphemes.get(id).expect("the cluster must be retrievable");
    assert!(text.starts_with('\u{1F468}'));
    assert!(text.contains('\u{200D}'), "the joiners are preserved: {text:?}");
}

#[test]
fn a_plain_build_log_allocates_no_side_tables() {
    // The 20-byte-cell claim only means anything if the side tables stay empty
    // for ordinary text.
    let mut core = terminal(80, 24);
    for _ in 0..50 {
        core.write(b"   Compiling mica-core v0.1.0\r\n");
    }
    assert_eq!(
        core.side_tables().allocated_bytes(),
        0,
        "plain ASCII output must not allocate a single byte of side table"
    );
}

#[test]
fn a_device_attributes_query_is_answered() {
    let mut core = terminal(20, 5);
    core.write(b"\x1b[c");
    let reply = core.take_replies();
    assert!(!reply.is_empty(), "DA1 must be answered, not ignored");
    assert!(reply.starts_with(b"\x1b[?"), "unexpected DA1 reply: {reply:?}");
    assert!(core.take_replies().is_empty(), "replies are drained, not repeated");
}

#[test]
fn osc133_markers_are_reported_as_semantic_events() {
    let mut core = terminal(40, 10);
    core.write(b"\x1b]133;A\x07$ \x1b]133;B\x07false\r\n\x1b]133;C\x07");
    core.write(b"\x1b]133;D;1\x07");

    let events = core.drain_semantic_events();
    assert!(events.contains(&SemanticEvent::PromptStart));
    assert!(events.contains(&SemanticEvent::CommandStart));
    assert!(events.contains(&SemanticEvent::OutputStart { command: None }));
    assert!(events.contains(&SemanticEvent::CommandDone { exit: Some(1) }));
    assert!(core.drain_semantic_events().is_empty(), "events are drained, not repeated");
}

#[test]
fn osc7_is_reported_as_a_working_directory_change() {
    let mut core = terminal(40, 10);
    core.write(b"\x1b]7;file://host/Users/me/code\x07");
    assert!(core
        .drain_semantic_events()
        .contains(&SemanticEvent::Cwd("/Users/me/code".into())));
}

#[test]
fn clipboard_bell_and_notifications_leave_the_backend_as_events() {
    let mut core = terminal(40, 10);
    core.write(b"\x1b]52;c;Y29weSBtZQ==\x07");
    core.write(b"\x07");
    core.write(b"\x1b]777;notify;Build;finished\x07");
    let events = core.drain_semantic_events();
    assert!(events.contains(&SemanticEvent::ClipboardWrite("copy me".into())));
    assert!(events.contains(&SemanticEvent::Bell));
    assert!(events.contains(&SemanticEvent::Notification {
        title: Some("Build".into()),
        body: "finished".into(),
    }));
}

#[test]
fn osc8_hyperlinks_can_be_resolved_at_a_visible_cell() {
    use mica_core::backend::Point;
    let mut core = terminal(40, 10);
    core.write(b"\x1b]8;;https://example.com/docs\x07link\x1b]8;;\x07");
    assert_eq!(
        core.hyperlink_at(Point::new(0, 0)).as_deref(),
        Some("https://example.com/docs")
    );
    assert_eq!(core.hyperlink_at(Point::new(0, 4)), None);
}

#[test]
fn visible_text_is_available_for_on_demand_accessibility() {
    let mut core = terminal(20, 3);
    core.write(b"accessible");
    assert!(core.visible_text().contains("accessible"));
}

#[test]
fn a_title_change_is_reported_once() {
    let mut core = terminal(40, 10);
    core.write(b"\x1b]0;mica\x07");
    assert_eq!(core.take_title().as_deref(), Some("mica"));
    assert_eq!(core.take_title(), None, "the title is taken, not polled");
}

#[test]
fn scrolling_into_history_and_back_lands_where_it_started() {
    let mut core = terminal(20, 5);
    for i in 0..40 {
        core.write(format!("line {i}\r\n").as_bytes());
    }
    let bottom = row_text_all(&mut core);

    core.scroll_viewport(10);
    assert_ne!(row_text_all(&mut core), bottom, "scrolling up must show history");

    core.scroll_viewport(-10);
    assert_eq!(row_text_all(&mut core), bottom, "scrolling back must restore the view");
}

/// Reads the whole visible grid.
///
/// Note the `damage_all` — there is no `full_grid()` on the trait, so even a
/// test that wants everything has to say so out loud. That is the design
/// working as intended rather than an inconvenience to route around.
fn row_text_all(core: &mut Backend) -> Vec<String> {
    core.damage_all();
    let mut rows: Vec<(u16, String)> = core
        .dirty_rows()
        .map(|r| {
            (
                r.index,
                r.cells.iter().filter_map(|c| c.content.as_scalar()).collect::<String>(),
            )
        })
        .collect();
    rows.sort_by_key(|(i, _)| *i);
    rows.into_iter().map(|(_, t)| t).collect()
}

#[test]
fn selected_text_can_be_read_back() {
    use mica_core::backend::{Point, Selection, SelectionKind};
    let mut core = terminal(20, 5);
    core.write(b"copy me");
    core.set_selection(Some(Selection {
        start: Point::new(0, 0),
        end: Point::new(0, 6),
        kind: SelectionKind::Simple,
    }));
    assert_eq!(core.selection_text().as_deref(), Some("copy me"));
    core.set_selection(None);
    assert!(core.selection().is_none());
}

#[test]
fn selection_can_span_the_complete_retained_buffer() {
    use mica_core::backend::{Point, Selection, SelectionKind};
    let mut core = terminal(8, 2);
    core.write(b"first\r\nsecond\r\nthird");
    core.set_selection(Some(Selection {
        start: Point::new(i32::MIN, 0),
        end: Point::new(i32::MAX, u16::MAX),
        kind: SelectionKind::Simple,
    }));
    let text = core.selection_text().unwrap();
    assert!(text.contains("first"), "oldest line was omitted: {text:?}");
    assert!(text.contains("third"), "newest line was omitted: {text:?}");
}

#[test]
fn bracketed_paste_mode_tracks_what_the_child_requested() {
    let mut core = terminal(20, 5);
    assert!(!core.modes().bracketed_paste);

    core.write(b"\x1b[?2004h");
    assert!(core.modes().bracketed_paste);

    core.write(b"\x1b[?2004l");
    assert!(!core.modes().bracketed_paste);
}

#[test]
fn mouse_and_focus_modes_track_the_child_protocol() {
    use mica_core::backend::{MouseEncoding, MouseTracking};
    let mut core = terminal(20, 5);
    assert_eq!(core.modes().mouse_tracking, MouseTracking::Off);
    assert!(!core.modes().focus_reporting);

    core.write(b"\x1b[?1002h\x1b[?1006h\x1b[?1004h");
    let modes = core.modes();
    assert!(modes.mouse_reporting);
    assert_eq!(modes.mouse_tracking, MouseTracking::Drag);
    assert_eq!(modes.mouse_encoding, MouseEncoding::Sgr);
    assert!(modes.focus_reporting);

    core.write(b"\x1b[?1002l\x1b[?1006l\x1b[?1004l");
    assert_eq!(core.modes().mouse_tracking, MouseTracking::Off);
    assert!(!core.modes().focus_reporting);
}
