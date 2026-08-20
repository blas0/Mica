//! The command palette — `⌘⇧P`.
//!
//! Every setting, theme, and action behind one fuzzy field. Action ids are flat
//! `namespace.verb` strings with a human description each, which is what lets
//! the same list be searched, displayed, and dispatched without a lookup table
//! per use.
//!
//! Ranked by [`crate::search::fuzzy_match`], which is a real search rather than
//! a greedy scan — see that module for why the difference is visible to the
//! user on the very first query anyone types.

use mica_atlas::atlas::Atlas;
use mica_atlas::fontset::Style;
use mica_core::material::{Material, Role};

use crate::grid::InstanceBuffers;
use crate::search::{rank, FuzzyMatch};

use super::{fill, layout_text, panel, text_width, OverlayMetrics, TextField};

/// One thing the palette can do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    /// `namespace.verb`, matched against and dispatched on.
    pub id: String,
    /// What a person reads.
    pub label: String,
    /// The key equivalent, shown right-aligned. Empty when there is none.
    pub shortcut: String,
}

impl Action {
    fn new(id: &str, label: &str, shortcut: &str) -> Action {
        Action { id: id.into(), label: label.into(), shortcut: shortcut.into() }
    }
}

/// The action set. Themes are appended at construction so a user theme appears
/// without anything here changing.
pub fn default_actions(theme_ids: &[String]) -> Vec<Action> {
    let mut actions = vec![
        Action::new("session.next_tab", "Next Tab", "⌃⇥"),
        Action::new("session.previous_tab", "Previous Tab", "⌃⇧⇥"),
        Action::new("session.scroll_bottom", "Scroll to Bottom", "⌘↓"),
        Action::new("session.clear_selection", "Clear Selection", "⎋"),
        Action::new("blocks.next", "Next Command Block", "⌘↓"),
        Action::new("blocks.previous", "Previous Command Block", "⌘↑"),
        Action::new("blocks.fold", "Fold Command Block", ""),
        Action::new("settings.open", "Open Settings", "⌘,"),
        Action::new("settings.fx.cursor", "Caret Motion · Next Style", ""),
        Action::new("settings.fx.decay", "Toggle Caret Decay", ""),
        Action::new("settings.fx.blink", "Toggle Caret Blink", ""),
        Action::new("settings.fx.reduce", "Toggle Reduce Motion", ""),
        Action::new("settings.fx.blocks", "Toggle Block Gutter", ""),
        Action::new("settings.fx.depth", "Toggle Ambient Light", ""),
    ];
    for id in theme_ids {
        actions.push(Action::new(
            &format!("theme.{id}"),
            &format!("Theme · {id}"),
            "",
        ));
    }
    actions
}

/// How many rows are shown at once. More than this and the palette stops being
/// a thing you scan and starts being a thing you scroll.
const VISIBLE_ROWS: usize = 8;

#[derive(Debug, Clone)]
pub struct Palette {
    open: bool,
    query: TextField,
    actions: Vec<Action>,
    /// Indices into `actions`, best first.
    ranked: Vec<(usize, FuzzyMatch)>,
    selected: usize,
    /// First visible row, so a long result list scrolls rather than clipping.
    scroll: usize,
}

impl Palette {
    pub fn new(actions: Vec<Action>) -> Palette {
        let mut palette = Palette {
            open: false,
            query: TextField::new(),
            actions,
            ranked: Vec::new(),
            selected: 0,
            scroll: 0,
        };
        palette.rerank();
        palette
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn query(&self) -> &str {
        self.query.text()
    }

    pub fn actions(&self) -> &[Action] {
        &self.actions
    }

    /// The visible, ranked results.
    pub fn results(&self) -> Vec<&Action> {
        self.ranked.iter().map(|(i, _)| &self.actions[*i]).collect()
    }

    pub fn selected(&self) -> Option<&Action> {
        self.ranked.get(self.selected).map(|(i, _)| &self.actions[*i])
    }

    pub fn open(&mut self) {
        self.open = true;
        // Opening always starts from a blank query. A palette that remembers
        // the last thing you typed makes the first keystroke unpredictable.
        self.query.clear();
        self.rerank();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
    }

    pub fn set_theme_ids(&mut self, theme_ids: &[String]) {
        self.actions = default_actions(theme_ids);
        self.rerank();
    }

    fn rerank(&mut self) {
        let query = self.query.text().to_owned();
        let ranked = rank(&query, &self.actions, |a| a.id.as_str());
        // `rank` borrows the actions; convert to indices so the palette can be
        // mutated afterwards.
        self.ranked = ranked
            .into_iter()
            .map(|(action, m)| {
                let index = self
                    .actions
                    .iter()
                    .position(|a| std::ptr::eq(a, action))
                    .unwrap_or(0);
                (index, m)
            })
            .collect();
        self.selected = 0;
        self.scroll = 0;
    }

    /// Feeds a character. Returns whether anything changed.
    pub fn type_char(&mut self, ch: char) -> bool {
        if !self.open || ch.is_control() {
            return false;
        }
        self.query.insert(ch);
        self.rerank();
        true
    }

    pub fn backspace(&mut self) -> bool {
        if !self.open || !self.query.backspace() {
            return false;
        }
        self.rerank();
        true
    }

    pub fn select_next(&mut self) -> bool {
        if self.ranked.is_empty() {
            return false;
        }
        // Wrapping, like every palette people already use.
        self.selected = (self.selected + 1) % self.ranked.len();
        self.follow_selection();
        true
    }

    pub fn select_previous(&mut self) -> bool {
        if self.ranked.is_empty() {
            return false;
        }
        self.selected =
            if self.selected == 0 { self.ranked.len() - 1 } else { self.selected - 1 };
        self.follow_selection();
        true
    }

    fn follow_selection(&mut self) {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + VISIBLE_ROWS {
            self.scroll = self.selected + 1 - VISIBLE_ROWS;
        }
    }

    /// Takes the selected action's id and closes the palette.
    pub fn accept(&mut self) -> Option<String> {
        let id = self.selected()?.id.clone();
        self.close();
        Some(id)
    }

    /// Emits the palette's quads and text.
    pub fn render(
        &self,
        atlas: &mut Atlas,
        material: &Material,
        metrics: OverlayMetrics,
        out: &mut InstanceBuffers,
    ) {
        if !self.open {
            return;
        }
        let padding = metrics.padding();
        let row = metrics.row_height();
        let width = (metrics.viewport.0 * 0.6).clamp(320.0, 720.0);
        let rows = self.ranked.len().min(VISIBLE_ROWS);
        let height = row * (rows as f32 + 1.0) + padding;
        let origin = (
            ((metrics.viewport.0 - width) / 2.0).round(),
            (metrics.viewport.1 * 0.12).round(),
        );

        out.quads.push(panel(origin, (width, height), material, 1.0));

        // --- query line -----------------------------------------------------
        let text_x = origin.0 + padding;
        let query_y = origin.1 + (padding / 2.0);
        layout_text(
            atlas,
            "❯ ",
            (text_x, query_y),
            material.role(Role::Accent),
            1.0,
            metrics,
            Style::REGULAR,
            &mut out.ui_text,
        );
        let prompt_width = text_width("❯ ", metrics);
        layout_text(
            atlas,
            self.query.text(),
            (text_x + prompt_width, query_y),
            material.role(Role::Foreground),
            1.0,
            metrics,
            Style::REGULAR,
            &mut out.ui_text,
        );
        // The caret. Non-blinking on purpose: a blinking caret in an overlay
        // would be an animation, and an animation keeps the renderer awake for
        // as long as the palette is open.
        let caret_x = text_x + prompt_width + self.query.caret() as f32 * metrics.cell_width;
        out.quads.push(fill(
            (caret_x, query_y),
            (2.0, metrics.cell_height),
            material.role(Role::Accent),
            0.9,
        ));

        // --- results --------------------------------------------------------
        for (offset, (index, matched)) in
            self.ranked.iter().skip(self.scroll).take(VISIBLE_ROWS).enumerate()
        {
            let action = &self.actions[*index];
            let absolute = self.scroll + offset;
            let y = origin.1 + row * (offset as f32 + 1.0) + (padding / 2.0);

            if absolute == self.selected {
                out.quads.push(fill(
                    (origin.0 + padding / 2.0, y - row * 0.15),
                    (width - padding, row),
                    material.role(Role::Accent),
                    0.22,
                ));
            }

            layout_text(
                atlas,
                &action.label,
                (text_x, y),
                material.role(Role::Foreground),
                1.0,
                metrics,
                Style::REGULAR,
                &mut out.ui_text,
            );

            // The id, dimmed, so the thing being fuzzy-matched is visible —
            // otherwise a query that matches on the id looks like it matched
            // nothing in particular.
            let id_x = text_x + text_width(&action.label, metrics) + metrics.cell_width * 2.0;
            layout_text(
                atlas,
                &action.id,
                (id_x, y),
                material.role(Role::Dim),
                1.0,
                metrics,
                Style::REGULAR,
                &mut out.ui_text,
            );

            // Highlight the characters the query actually matched.
            for position in &matched.positions {
                let x = id_x + *position as f32 * metrics.cell_width;
                out.quads.push(fill(
                    (x, y + metrics.cell_height * 0.92),
                    (metrics.cell_width, 1.5),
                    material.role(Role::Accent),
                    0.8,
                ));
            }

            if !action.shortcut.is_empty() {
                let shortcut_x =
                    origin.0 + width - padding - text_width(&action.shortcut, metrics);
                layout_text(
                    atlas,
                    &action.shortcut,
                    (shortcut_x, y),
                    material.role(Role::Dim),
                    1.0,
                    metrics,
                    Style::REGULAR,
                    &mut out.ui_text,
                );
            }
        }
    }
}

impl Default for Palette {
    fn default() -> Palette {
        Palette::new(default_actions(&[]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette() -> Palette {
        Palette::new(default_actions(&[
            "slate".to_owned(),
            "quartz".to_owned(),
            "basalt".to_owned(),
        ]))
    }

    #[test]
    fn a_palette_starts_closed() {
        assert!(!palette().is_open());
    }

    #[test]
    fn opening_shows_every_action() {
        let mut p = palette();
        p.open();
        assert!(p.is_open());
        assert_eq!(p.results().len(), p.actions().len());
        assert_eq!(p.query(), "");
    }

    #[test]
    fn themes_appear_as_actions_without_a_special_case() {
        let p = palette();
        let ids: Vec<&str> = p.actions().iter().map(|a| a.id.as_str()).collect();
        assert!(ids.contains(&"theme.quartz"));
        assert!(ids.contains(&"theme.basalt"));
    }

    #[test]
    fn typing_narrows_the_list_and_ranks_the_obvious_answer_first() {
        let mut p = palette();
        p.open();
        for ch in "snt".chars() {
            assert!(p.type_char(ch));
        }
        assert_eq!(p.selected().unwrap().id, "session.next_tab");
        assert!(p.results().len() < p.actions().len());
    }

    #[test]
    fn a_theme_can_be_reached_by_its_name() {
        let mut p = palette();
        p.open();
        for ch in "quartz".chars() {
            p.type_char(ch);
        }
        assert_eq!(p.selected().unwrap().id, "theme.quartz");
    }

    #[test]
    fn backspace_widens_the_list_again() {
        let mut p = palette();
        p.open();
        for ch in "snt".chars() {
            p.type_char(ch);
        }
        let narrow = p.results().len();
        assert!(p.backspace());
        assert!(p.results().len() > narrow);
    }

    #[test]
    fn a_query_matching_nothing_leaves_no_selection() {
        let mut p = palette();
        p.open();
        for ch in "zzzzz".chars() {
            p.type_char(ch);
        }
        assert!(p.results().is_empty());
        assert!(p.selected().is_none());
        assert_eq!(p.accept(), None, "accepting nothing must not dispatch");
    }

    #[test]
    fn selection_wraps_in_both_directions() {
        let mut p = palette();
        p.open();
        let first = p.selected().unwrap().id.clone();
        assert!(p.select_previous());
        let last = p.selected().unwrap().id.clone();
        assert_ne!(first, last);
        assert!(p.select_next());
        assert_eq!(p.selected().unwrap().id, first);
    }

    #[test]
    fn typing_resets_the_selection_to_the_best_match() {
        // Otherwise a selection five rows down survives a query change and
        // Enter runs something the user never looked at.
        let mut p = palette();
        p.open();
        p.select_next();
        p.select_next();
        p.type_char('s');
        assert_eq!(p.selected().map(|a| a.id.as_str()), p.results().first().map(|a| a.id.as_str()));
    }

    #[test]
    fn accepting_returns_the_id_and_closes() {
        let mut p = palette();
        p.open();
        for ch in "snt".chars() {
            p.type_char(ch);
        }
        assert_eq!(p.accept().as_deref(), Some("session.next_tab"));
        assert!(!p.is_open());
    }

    #[test]
    fn closing_forgets_the_query() {
        // A palette that remembers the last thing you typed makes the first
        // keystroke of the next invocation unpredictable.
        let mut p = palette();
        p.open();
        p.type_char('s');
        p.close();
        p.open();
        assert_eq!(p.query(), "");
        assert_eq!(p.results().len(), p.actions().len());
    }

    #[test]
    fn a_closed_palette_ignores_input() {
        let mut p = palette();
        assert!(!p.type_char('s'));
        assert!(!p.backspace());
        assert_eq!(p.query(), "");
    }

    #[test]
    fn control_characters_are_not_typed_into_the_query() {
        let mut p = palette();
        p.open();
        assert!(!p.type_char('\u{1b}'));
        assert!(!p.type_char('\r'));
        assert_eq!(p.query(), "");
    }

    #[test]
    fn a_long_result_list_scrolls_to_follow_the_selection() {
        let mut p = palette();
        p.open();
        assert!(p.actions().len() > VISIBLE_ROWS, "this test needs a list longer than a page");
        for _ in 0..VISIBLE_ROWS {
            p.select_next();
        }
        // The selection has to still be on screen, which means the window moved.
        assert!(p.scroll > 0, "the result list did not scroll to follow the selection");
    }

    #[test]
    fn a_closed_palette_draws_nothing() {
        use mica_atlas::fontset::FontSet;
        use mica_core::material::builtin;

        let mut atlas = Atlas::new(FontSet::resolve("Menlo", 13.0, 2.0));
        let material = Material::from_theme(&builtin("slate").unwrap()).unwrap();
        let metrics = OverlayMetrics::from_atlas(&atlas, (800.0, 600.0));
        let mut out = InstanceBuffers::default();

        palette().render(&mut atlas, &material, metrics, &mut out);
        assert!(out.is_empty(), "a closed palette produced {} instances", out.total());
    }

    #[test]
    fn an_open_palette_draws_a_panel_and_its_rows() {
        use mica_atlas::fontset::FontSet;
        use mica_core::material::builtin;

        let mut atlas = Atlas::new(FontSet::resolve("Menlo", 13.0, 2.0));
        let material = Material::from_theme(&builtin("slate").unwrap()).unwrap();
        let metrics = OverlayMetrics::from_atlas(&atlas, (800.0, 600.0));
        let mut out = InstanceBuffers::default();

        let mut p = palette();
        p.open();
        p.render(&mut atlas, &material, metrics, &mut out);

        assert!(!out.quads.is_empty(), "no panel was drawn");
        assert!(!out.ui_text.is_empty(), "no text was drawn");
        // Chrome must go through ui_text, never through the cell pipeline —
        // the grid and the overlay have different coordinate systems.
        assert!(out.glyphs.is_empty(), "overlay text leaked into the cell pipeline");
        assert!(out.backgrounds.is_empty());
    }

    #[test]
    fn the_palette_stays_inside_the_viewport() {
        use mica_atlas::fontset::FontSet;
        use mica_core::material::builtin;

        let mut atlas = Atlas::new(FontSet::resolve("Menlo", 13.0, 2.0));
        let material = Material::from_theme(&builtin("slate").unwrap()).unwrap();
        let mut out = InstanceBuffers::default();

        // A narrow window is where a fixed-width panel would escape.
        let metrics = OverlayMetrics::from_atlas(&atlas, (400.0, 300.0));
        let mut p = palette();
        p.open();
        p.render(&mut atlas, &material, metrics, &mut out);

        let panel = out.quads[0];
        assert!(panel.origin[0] >= 0.0, "the panel starts off the left edge");
        assert!(
            panel.origin[0] + panel.size[0] <= metrics.viewport.0 + 1.0,
            "the panel runs off the right edge"
        );
    }
}
