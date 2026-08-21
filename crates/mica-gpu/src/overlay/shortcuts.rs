//! Drawing the keyboard shortcut panel.
//!
//! Only the drawing. The panel's model — what a chord is, which action holds
//! it, what happens when two collide — lives in the window layer, because
//! chords are an input concept and this crate must not learn what a modifier
//! key is. What crosses the boundary is a list of strings.
//!
//! That split is also what makes the panel testable: the interesting part is
//! the rebinding, and it is tested without a GPU.

use mica_atlas::atlas::Atlas;
use mica_atlas::fontset::Style;
use mica_core::material::{Material, Role};

use crate::grid::InstanceBuffers;

use super::{fill, layout_text, panel, text_width, OverlayMetrics};

/// One row: what the action is, and what it is bound to.
#[derive(Debug, Clone, Copy)]
pub struct ShortcutRow<'a> {
    pub label: &'a str,
    /// Already in display form (`⇧⌘P`), or empty for an unbound action.
    pub chord: &'a str,
}

/// How many rows are visible at once.
const VISIBLE_ROWS: usize = 10;

/// Everything the panel needs to draw itself.
#[derive(Debug, Clone, Copy)]
pub struct ShortcutView<'a> {
    pub rows: &'a [ShortcutRow<'a>],
    pub selected: usize,
    /// True while waiting for the user to press the combination.
    pub capturing: bool,
    /// A line under the list — a conflict that was resolved, or a rejected
    /// chord. Transient, and worth the space: silently refusing a keystroke is
    /// how a configuration panel becomes a thing people distrust.
    pub notice: Option<&'a str>,
}


/// What the panel says it can do, and how loudly.
///
/// Named keys, not drawn ones. `⌫` and `⎋` are the correct symbols and they
/// are what the keycaps say, but at footer size in a monospace face they read
/// as a circled cross and a circular arrow — neither obviously the key you are
/// meant to press. The arrows survive that test; those two do not.
fn footer_hint(notice: Option<&str>, capturing: bool) -> (&str, Role) {
    match (notice, capturing) {
        (Some(notice), _) => (notice, Role::Warning),
        (None, true) => ("esc cancel", Role::Dim),
        (None, false) => ("↑↓ move · space rebind · backspace unbind · esc close", Role::Dim),
    }
}

pub fn render(
    view: ShortcutView<'_>,
    atlas: &mut Atlas,
    material: &Material,
    metrics: OverlayMetrics,
    out: &mut InstanceBuffers,
) {
    let padding = metrics.padding();
    let row_height = metrics.row_height();

    let width = (metrics.viewport.0 * 0.62).clamp(420.0, 760.0);
    let header = row_height;
    let footer = row_height;
    let visible = view.rows.len().min(VISIBLE_ROWS);
    let height = header + visible as f32 * row_height + footer + padding;

    let origin = (
        ((metrics.viewport.0 - width) / 2.0).round(),
        ((metrics.viewport.1 - height) / 3.0).max(padding).round(),
    );
    out.quads.push(panel(origin, (width, height), material, 1.0));

    let text_x = origin.0 + padding;
    let mut y = origin.1 + padding / 2.0;

    layout_text(
        atlas,
        "Keyboard Shortcuts",
        (text_x, y),
        material.role(Role::Foreground),
        1.0,
        metrics,
        Style::new(true, false),
        &mut out.ui_text,
    );
    y += header;

    // Scroll the window of rows so the selection is always inside it.
    let first = view.selected.saturating_sub(VISIBLE_ROWS - 1).min(
        view.rows.len().saturating_sub(visible),
    );

    for (offset, row) in view.rows.iter().skip(first).take(visible).enumerate() {
        let index = first + offset;
        let selected = index == view.selected;
        let row_y = y + offset as f32 * row_height;

        if selected {
            out.quads.push(fill(
                (origin.0 + padding / 2.0, row_y - padding / 4.0),
                (width - padding, row_height),
                material.role(Role::Accent),
                if view.capturing { 0.42 } else { 0.22 },
            ));
        }

        layout_text(
            atlas,
            row.label,
            (text_x, row_y),
            material.role(Role::Foreground),
            if selected { 1.0 } else { 0.82 },
            metrics,
            Style::REGULAR,
            &mut out.ui_text,
        );

        // The chord is right-aligned, because the eye scans a column of
        // shortcuts down its right edge, not from wherever each label ended.
        let shown = if selected && view.capturing {
            "press a combination…"
        } else if row.chord.is_empty() {
            "—"
        } else {
            row.chord
        };
        let chord_x = origin.0 + width - padding - text_width(shown, metrics);
        layout_text(
            atlas,
            shown,
            (chord_x, row_y),
            material.role(if selected && view.capturing {
                Role::Warning
            } else if row.chord.is_empty() {
                Role::Dim
            } else {
                Role::Accent
            }),
            1.0,
            metrics,
            Style::REGULAR,
            &mut out.ui_text,
        );
    }

    // --- the footer ---------------------------------------------------------
    let footer_y = y + visible as f32 * row_height + padding / 4.0;
    let (hint, role) = footer_hint(view.notice, view.capturing);
    layout_text(
        atlas,
        hint,
        (text_x, footer_y),
        material.role(role),
        1.0,
        metrics,
        Style::REGULAR,
        &mut out.ui_text,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use mica_atlas::fontset::FontSet;
    use mica_core::material::builtin;

    fn setup() -> (Atlas, Material, OverlayMetrics) {
        let atlas = Atlas::new(FontSet::resolve("Menlo", 13.0, 1.0));
        let material = Material::from_theme(&builtin("slate").unwrap()).unwrap();
        let metrics = OverlayMetrics::from_atlas(&atlas, (1200.0, 700.0));
        (atlas, material, metrics)
    }

    fn rows() -> Vec<ShortcutRow<'static>> {
        vec![
            ShortcutRow { label: "Command Palette", chord: "⇧⌘P" },
            ShortcutRow { label: "Find in Scrollback", chord: "⌘F" },
            ShortcutRow { label: "Next Match", chord: "" },
        ]
    }

    #[test]
    fn the_panel_draws_a_background_and_some_text() {
        let (mut atlas, material, metrics) = setup();
        let mut out = InstanceBuffers::default();
        let rows = rows();
        render(
            ShortcutView { rows: &rows, selected: 0, capturing: false, notice: None },
            &mut atlas,
            &material,
            metrics,
            &mut out,
        );
        assert!(!out.quads.is_empty(), "the panel drew no background");
        assert!(!out.ui_text.is_empty(), "the panel drew no text");
    }

    #[test]
    fn the_selected_row_is_highlighted_and_only_that_row() {
        let (mut atlas, material, metrics) = setup();
        let rows = rows();
        let mut count = |selected| {
            let mut out = InstanceBuffers::default();
            render(
                ShortcutView { rows: &rows, selected, capturing: false, notice: None },
                &mut atlas,
                &material,
                metrics,
                &mut out,
            );
            out.quads.len()
        };
        // One panel plus one highlight, whichever row is selected.
        assert_eq!(count(0), count(1));
        assert_eq!(count(0), 2);
    }

    #[test]
    fn capturing_says_so_instead_of_showing_the_old_chord() {
        // Otherwise the panel looks identical whether or not it is waiting for
        // a keystroke, and the user cannot tell that it has the keyboard.
        let (mut atlas, material, metrics) = setup();
        let rows = rows();
        let mut text_len = |capturing| {
            let mut out = InstanceBuffers::default();
            render(
                ShortcutView { rows: &rows, selected: 0, capturing, notice: None },
                &mut atlas,
                &material,
                metrics,
                &mut out,
            );
            out.ui_text.len()
        };
        assert_ne!(text_len(false), text_len(true), "capturing looks the same as not capturing");
    }

    #[test]
    fn a_notice_replaces_the_hint_rather_than_being_dropped() {
        let (mut atlas, material, metrics) = setup();
        let rows = rows();
        let mut with = InstanceBuffers::default();
        render(
            ShortcutView {
                rows: &rows,
                selected: 0,
                capturing: false,
                notice: Some("⌘F taken from Find in Scrollback"),
            },
            &mut atlas,
            &material,
            metrics,
            &mut with,
        );
        let mut without = InstanceBuffers::default();
        render(
            ShortcutView { rows: &rows, selected: 0, capturing: false, notice: None },
            &mut atlas,
            &material,
            metrics,
            &mut without,
        );
        assert_ne!(with.ui_text.len(), without.ui_text.len());
    }

    #[test]
    fn a_long_list_scrolls_rather_than_drawing_past_the_panel() {
        let (mut atlas, material, metrics) = setup();
        let many: Vec<ShortcutRow> =
            (0..40).map(|_| ShortcutRow { label: "Action", chord: "⌘K" }).collect();
        let mut out = InstanceBuffers::default();
        render(
            ShortcutView { rows: &many, selected: 39, capturing: false, notice: None },
            &mut atlas,
            &material,
            metrics,
            &mut out,
        );
        // Ten rows of label and chord, plus the header and the footer.
        assert!(
            out.ui_text.len() < 40 * 20,
            "the panel drew every row instead of a window of them"
        );
        assert_eq!(out.quads.len(), 2, "the selected row is off-screen, so nothing highlighted");
    }

    #[test]
    fn an_empty_list_does_not_panic() {
        let (mut atlas, material, metrics) = setup();
        let mut out = InstanceBuffers::default();
        render(
            ShortcutView { rows: &[], selected: 0, capturing: false, notice: None },
            &mut atlas,
            &material,
            metrics,
            &mut out,
        );
        assert!(!out.quads.is_empty());
    }

    #[test]
    fn the_footer_names_its_keys_in_words_rather_than_drawing_them() {
        // Reported from a screenshot: `⌫ unbind · ⎋ close` read as a circled
        // cross and a circular arrow, and neither said what to press.
        let (hint, _) = footer_hint(None, false);
        for word in ["move", "rebind", "unbind", "close"] {
            assert!(hint.contains(word), "`{word}` is missing from `{hint}`");
        }
        for drawn in ['\u{232b}', '\u{238b}'] {
            assert!(!hint.contains(drawn), "`{drawn}` is back in `{hint}`");
        }
        assert!(footer_hint(None, true).0.starts_with("esc"), "the capture hint still draws its key");

        // A notice still wins the footer, and is not a hint.
        assert_eq!(footer_hint(Some("taken"), false).0, "taken");
    }
}
