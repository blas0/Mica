//! Find in scrollback — `⌘F`.
//!
//! Matches are highlighted **on the GPU, by drawing quads under the text**.
//! They are never written into cells. That distinction is worth stating because
//! the alternative is tempting and wrong: mutating cells to mark a match makes
//! the highlight part of terminal state, which means it survives a reflow it
//! should not, corrupts the scrollback the user copies, and dirties every row it
//! touches — turning a search into a full repaint.
//!
//! Because highlights are quads, closing find costs exactly one frame and
//! leaves no trace.

use mica_atlas::atlas::Atlas;
use mica_atlas::fontset::Style;
use mica_core::material::{Material, Role};

use crate::grid::InstanceBuffers;
use crate::search::{Match, Search};

use super::{fill, layout_text, panel, text_width, OverlayMetrics, TextField};

#[derive(Debug, Default, Clone)]
pub struct Find {
    open: bool,
    query: TextField,
    search: Search,
    /// Set when the query changed and the caller has not re-run the search yet.
    /// The search reads every line of scrollback, so it must not happen inside
    /// the render path.
    dirty: bool,
}

impl Find {
    pub fn new() -> Find {
        Find::default()
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn query(&self) -> &str {
        self.query.text()
    }

    pub fn search(&self) -> &Search {
        &self.search
    }

    pub fn needs_search(&self) -> bool {
        self.dirty
    }

    pub fn open(&mut self) {
        self.open = true;
        // The query survives reopening, unlike the palette's. Searching for the
        // same thing again is the common case; running the same action again is
        // not.
        self.dirty = true;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.search.clear();
        self.dirty = false;
    }

    pub fn type_char(&mut self, ch: char) -> bool {
        if !self.open || ch.is_control() {
            return false;
        }
        self.query.insert(ch);
        self.dirty = true;
        true
    }

    pub fn backspace(&mut self) -> bool {
        if !self.open || !self.query.backspace() {
            return false;
        }
        self.dirty = true;
        true
    }

    /// Re-runs the search over `lines`, which yields `(absolute_line, text)`.
    ///
    /// The caller supplies the corpus so this module never has to know about a
    /// terminal, and so the expensive read happens where the caller can see it.
    pub fn run<'a, I>(&mut self, lines: I, focus_line: i32)
    where
        I: IntoIterator<Item = (i32, &'a str)>,
    {
        self.search.run(self.query.text(), lines);
        self.search.focus_near(focus_line);
        self.dirty = false;
    }

    pub fn next(&mut self) -> Option<Match> {
        self.search.next()
    }

    pub fn previous(&mut self) -> Option<Match> {
        self.search.previous()
    }

    pub fn current(&self) -> Option<Match> {
        self.search.current()
    }

    /// The `3 / 17` readout, or `no results` when there are none.
    pub fn status(&self) -> String {
        if self.query.is_empty() {
            return String::new();
        }
        match self.search.position() {
            Some(position) => format!("{position} / {}", self.search.len()),
            None => "no results".to_owned(),
        }
    }

    /// Draws the find bar and the match highlights.
    ///
    /// `first_visible_line` maps absolute line indices onto viewport rows;
    /// matches outside the viewport are skipped rather than clamped, so a
    /// match one line above the fold does not paint a stripe at the top.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &self,
        atlas: &mut Atlas,
        material: &Material,
        metrics: OverlayMetrics,
        first_visible_line: i32,
        visible_rows: u16,
        grid_origin: (f32, f32),
        out: &mut InstanceBuffers,
    ) {
        if !self.open {
            return;
        }

        // --- highlights -----------------------------------------------------
        let current = self.search.current();
        for m in self.search.matches() {
            let row = m.line - first_visible_line;
            if row < 0 || row >= visible_rows as i32 {
                continue;
            }
            let x = grid_origin.0 + m.start as f32 * metrics.cell_width;
            let y = grid_origin.1 + row as f32 * metrics.cell_height;
            let width = (m.end - m.start) as f32 * metrics.cell_width;

            // The current match is the accent; the rest are dimmer, so the eye
            // can find where it is without reading the counter.
            let is_current = current == Some(*m);
            out.quads.push(fill(
                (x, y),
                (width, metrics.cell_height),
                material.role(if is_current { Role::Accent } else { Role::Info }),
                if is_current { 0.55 } else { 0.28 },
            ));
        }

        // --- the bar --------------------------------------------------------
        let padding = metrics.padding();
        let row = metrics.row_height();
        let width = (metrics.viewport.0 * 0.4).clamp(260.0, 520.0);
        let height = row + padding / 2.0;
        // Top right, out of the way of a prompt at the bottom left.
        let origin = ((metrics.viewport.0 - width - padding).max(0.0).round(), padding.round());

        out.quads.push(panel(origin, (width, height), material, 1.0));

        let text_x = origin.0 + padding;
        let text_y = origin.1 + padding / 4.0;

        layout_text(
            atlas,
            "⌕ ",
            (text_x, text_y),
            material.role(Role::Accent),
            1.0,
            metrics,
            Style::REGULAR,
            &mut out.ui_text,
        );
        let prompt_width = text_width("⌕ ", metrics);

        layout_text(
            atlas,
            self.query.text(),
            (text_x + prompt_width, text_y),
            material.role(Role::Foreground),
            1.0,
            metrics,
            Style::REGULAR,
            &mut out.ui_text,
        );

        let caret_x = text_x + prompt_width + self.query.caret() as f32 * metrics.cell_width;
        out.quads.push(fill(
            (caret_x, text_y),
            (2.0, metrics.cell_height),
            material.role(Role::Accent),
            0.9,
        ));

        let status = self.status();
        if !status.is_empty() {
            let status_x = origin.0 + width - padding - text_width(&status, metrics);
            // "no results" reads as an error, a count reads as information.
            let role = if self.search.is_empty() { Role::Error } else { Role::Dim };
            layout_text(
                atlas,
                &status,
                (status_x, text_y),
                material.role(role),
                1.0,
                metrics,
                Style::REGULAR,
                &mut out.ui_text,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mica_atlas::fontset::FontSet;
    use mica_core::material::builtin;

    const CORPUS: &[(i32, &str)] = &[
        (-1, "error: first"),
        (0, "ok"),
        (1, "error: second"),
        (2, "error: third"),
    ];

    fn find_with(query: &str) -> Find {
        let mut find = Find::new();
        find.open();
        for ch in query.chars() {
            find.type_char(ch);
        }
        find.run(CORPUS.iter().map(|(l, t)| (*l, *t)), 0);
        find
    }

    fn setup() -> (Atlas, Material, OverlayMetrics) {
        let atlas = Atlas::new(FontSet::resolve("Menlo", 13.0, 2.0));
        let material = Material::from_theme(&builtin("slate").unwrap()).unwrap();
        let metrics = OverlayMetrics::from_atlas(&atlas, (800.0, 600.0));
        (atlas, material, metrics)
    }

    #[test]
    fn a_new_find_is_closed_and_empty() {
        let find = Find::new();
        assert!(!find.is_open());
        assert_eq!(find.query(), "");
        assert_eq!(find.status(), "");
    }

    #[test]
    fn typing_marks_the_search_dirty_without_running_it() {
        // The search reads every line of scrollback; doing it inside render
        // would put a full history walk in the frame path.
        let mut find = Find::new();
        find.open();
        find.type_char('e');
        assert!(find.needs_search());
        find.run(CORPUS.iter().map(|(l, t)| (*l, *t)), 0);
        assert!(!find.needs_search());
    }

    #[test]
    fn matches_are_found_and_counted() {
        let find = find_with("error");
        assert_eq!(find.search().len(), 3);
        // Lines -1 and 1 are equidistant from the focus line; the tie goes to
        // the earlier match, so the readout starts at the first of the three.
        assert_eq!(find.status(), "1 / 3");
        assert_eq!(find.current().unwrap().line, -1);
    }

    #[test]
    fn a_query_with_no_matches_says_so() {
        let find = find_with("zzz");
        assert_eq!(find.status(), "no results");
        assert!(find.current().is_none());
    }

    #[test]
    fn next_and_previous_move_through_the_matches() {
        let mut find = find_with("error");
        let first = find.current().unwrap().line;
        let second = find.next().unwrap().line;
        assert_ne!(first, second);
        assert_eq!(find.previous().unwrap().line, first);
    }

    #[test]
    fn the_query_survives_closing_and_reopening() {
        // Searching for the same thing again is the common case.
        let mut find = find_with("error");
        find.close();
        assert!(find.search().is_empty(), "closing must drop the highlights");
        find.open();
        assert_eq!(find.query(), "error");
        assert!(find.needs_search());
    }

    #[test]
    fn a_closed_find_draws_nothing() {
        let (mut atlas, material, metrics) = setup();
        let mut out = InstanceBuffers::default();
        Find::new().render(&mut atlas, &material, metrics, 0, 24, (0.0, 0.0), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn highlights_are_quads_and_never_touch_the_cell_pipeline() {
        // The distinction the module exists to preserve: a highlight is drawn
        // over the text, not written into it.
        let (mut atlas, material, metrics) = setup();
        let mut out = InstanceBuffers::default();
        find_with("error").render(&mut atlas, &material, metrics, -1, 24, (0.0, 0.0), &mut out);

        assert!(out.quads.len() >= 4, "expected three highlights plus the bar");
        assert!(out.glyphs.is_empty(), "find wrote into the cell pipeline");
        assert!(out.backgrounds.is_empty(), "find mutated cell backgrounds");
        assert!(!out.ui_text.is_empty(), "the find bar drew no text");
    }

    #[test]
    fn matches_outside_the_viewport_are_skipped_not_clamped() {
        // A match one line above the fold must not paint a stripe at the top.
        let (mut atlas, material, metrics) = setup();
        let mut with_history = InstanceBuffers::default();
        find_with("error").render(
            &mut atlas,
            &material,
            metrics,
            -1,
            24,
            (0.0, 0.0),
            &mut with_history,
        );

        let mut without_history = InstanceBuffers::default();
        find_with("error").render(
            &mut atlas,
            &material,
            metrics,
            1, // line -1 is now scrolled off the top
            24,
            (0.0, 0.0),
            &mut without_history,
        );
        assert!(
            without_history.quads.len() < with_history.quads.len(),
            "an off-screen match was still drawn"
        );
    }

    #[test]
    fn the_current_match_is_drawn_differently_from_the_others() {
        let (mut atlas, material, metrics) = setup();
        let mut out = InstanceBuffers::default();
        find_with("error").render(&mut atlas, &material, metrics, -1, 24, (0.0, 0.0), &mut out);

        // Three highlights, and they must not all be the same colour.
        let highlights: Vec<_> = out.quads.iter().take(3).map(|q| q.fill).collect();
        let distinct: std::collections::HashSet<_> = highlights.iter().collect();
        assert!(distinct.len() > 1, "the current match is indistinguishable from the rest");
    }

    #[test]
    fn the_bar_stays_inside_a_narrow_window() {
        let (mut atlas, material, _) = setup();
        let metrics = OverlayMetrics::from_atlas(&atlas, (300.0, 200.0));
        let mut out = InstanceBuffers::default();
        find_with("e").render(&mut atlas, &material, metrics, 0, 12, (0.0, 0.0), &mut out);

        let bar = out.quads.iter().find(|q| q.border_width > 0.0).expect("no panel drawn");
        assert!(bar.origin[0] >= 0.0, "the find bar starts off the left edge");
    }
}
