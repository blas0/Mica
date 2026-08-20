//! Turning scroll events into terminal lines.
//!
//! Pure, and separate from the view, because the arithmetic is the whole bug
//! surface and `NSEvent` cannot be constructed in a test.
//!
//! macOS delivers two completely different quantities through the same field:
//!
//! - A **mouse wheel** reports whole detents. `scrollingDeltaY` is already a
//!   line count — usually 1 or 3 per notch.
//! - A **trackpad** reports precise deltas in *points*, continuously, in
//!   fractions as small as a pixel of finger movement.
//!
//! `NSEvent::hasPreciseScrollingDeltas` is what tells them apart, and treating
//! the second as the first is why trackpad scrolling did nothing: a gentle
//! two-finger drag delivers deltas around 1 point, and one line was a third of
//! that after the old divisor, so every event rounded to zero lines and was
//! discarded. Fast flicks worked, slow ones did nothing at all.
//!
//! The fix is not a smaller divisor — it is keeping the remainder. A scroll of
//! a third of a line three times is one line.

/// Accumulates sub-line scrolling so that slow gestures still move.
#[derive(Debug, Clone, Default)]
pub struct ScrollAccumulator {
    /// Lines scrolled but not yet delivered, always in `(-1, 1)`.
    residue: f64,
}

/// How many points of trackpad travel make one line, when the cell height is
/// not known. Only a fallback; the real value comes from the font.
const FALLBACK_CELL_HEIGHT: f32 = 17.0;

impl ScrollAccumulator {
    pub fn new() -> ScrollAccumulator {
        ScrollAccumulator::default()
    }

    /// Converts one scroll event into whole lines, keeping the remainder.
    ///
    /// `delta` is `scrollingDeltaY`, `precise` is `hasPreciseScrollingDeltas`,
    /// and `cell_height` is in points — not device pixels, because scroll
    /// deltas are in points too and mixing the two makes scrolling twice as
    /// fast on a Retina display.
    pub fn lines(&mut self, delta: f64, precise: bool, cell_height: f32) -> i32 {
        if delta == 0.0 || !delta.is_finite() {
            return 0;
        }
        let in_lines = if precise {
            let height = if cell_height.is_finite() && cell_height >= 1.0 {
                cell_height as f64
            } else {
                FALLBACK_CELL_HEIGHT as f64
            };
            delta / height
        } else {
            // Already a line count. A wheel notch should move a line even when
            // the driver reports a fractional detent.
            delta
        };

        self.residue += in_lines;
        // Truncate toward zero: the fraction that is left is exactly what the
        // next event should build on.
        let whole = self.residue.trunc();
        self.residue -= whole;
        whole as i32
    }

    /// Drops any partial line.
    ///
    /// Called when the gesture ends or the viewport is yanked back to the
    /// bottom, so a half-line of leftover finger movement does not surface
    /// minutes later attached to an unrelated gesture.
    pub fn reset(&mut self) {
        self.residue = 0.0;
    }
}

/// What a scroll gesture should actually do.
///
/// A terminal has two entirely different things a wheel can mean, and which one
/// applies is not a preference — it is a property of what is on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScrollTarget {
    /// Move the viewport through scrollback. The normal screen.
    Viewport(i32),
    /// Send arrow keys to the child. The alternate screen has no scrollback to
    /// move through, so the only honest translation of "scroll up" is the
    /// keystroke the program already understands.
    Keys(Vec<u8>),
    /// Do nothing. The program asked for mouse events itself; inventing
    /// keystrokes would make a scroll gesture edit a file in `vim`.
    Nothing,
}

/// Arrow keys, in the spelling the terminal's cursor-key mode requires.
///
/// This is xterm's `alternateScroll` (private mode 1007), on by default there
/// and in every terminal anyone actually uses. Without it, scrolling inside
/// `less`, `vim`, `htop` — or a full-screen CLI like `claude` — does nothing at
/// all, which is exactly the symptom that sent me looking.
///
/// Capped because a trackpad flick can produce a hundred lines in one event,
/// and a hundred arrow keys arriving at a program that redraws on each one is a
/// visible stall. Three screens of movement per event is already more than a
/// hand can ask for.
pub fn alternate_scroll(lines: i32, application_cursor: bool) -> ScrollTarget {
    const MAX_KEYS: i32 = 96;
    if lines == 0 {
        return ScrollTarget::Nothing;
    }
    let up = lines > 0;
    let count = lines.unsigned_abs().min(MAX_KEYS as u32) as usize;
    // DECCKM: `ESC O A` when the application asked for it, `ESC [ A` otherwise.
    // Sending the wrong one is not cosmetic — `less` ignores the form it did
    // not ask for, so the scroll silently does nothing.
    let key: &[u8] = match (application_cursor, up) {
        (true, true) => b"\x1bOA",
        (true, false) => b"\x1bOB",
        (false, true) => b"\x1b[A",
        (false, false) => b"\x1b[B",
    };
    let mut out = Vec::with_capacity(key.len() * count);
    for _ in 0..count {
        out.extend_from_slice(key);
    }
    ScrollTarget::Keys(out)
}

/// Routes one gesture, given what the terminal is currently doing.
pub fn route(
    lines: i32,
    modes: mica_core::backend::TerminalModes,
) -> ScrollTarget {
    if !modes.alt_screen {
        return ScrollTarget::Viewport(lines);
    }
    if modes.mouse_reporting {
        return ScrollTarget::Nothing;
    }
    alternate_scroll(lines, modes.application_cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: f32 = 17.0;

    #[test]
    fn a_mouse_wheel_notch_scrolls_its_lines_directly() {
        let mut a = ScrollAccumulator::new();
        assert_eq!(a.lines(3.0, false, CELL), 3);
        assert_eq!(a.lines(-1.0, false, CELL), -1);
    }

    #[test]
    fn a_slow_trackpad_drag_eventually_scrolls() {
        // The reported bug, as a test. Each event is well under one line; the
        // old code rounded every one of them to zero and scrolling was dead.
        let mut a = ScrollAccumulator::new();
        let mut moved = 0;
        for _ in 0..20 {
            moved += a.lines(1.0, true, CELL);
        }
        assert!(
            moved > 0,
            "twenty points of finger travel scrolled {moved} lines — a slow drag does nothing"
        );
    }

    #[test]
    fn no_single_sub_line_event_is_thrown_away() {
        // Seventeen points is one cell, so seventeen one-point events are one
        // line — not zero, and not seventeen.
        let mut a = ScrollAccumulator::new();
        let total: i32 = (0..17).map(|_| a.lines(1.0, true, CELL)).sum();
        assert_eq!(total, 1);
    }

    #[test]
    fn the_remainder_carries_across_events_rather_than_accumulating_error() {
        let mut a = ScrollAccumulator::new();
        let total: i32 = (0..170).map(|_| a.lines(1.0, true, CELL)).sum();
        assert_eq!(total, 10, "170 points of travel is ten 17-point lines");
    }

    #[test]
    fn direction_is_preserved_in_both_modes() {
        let mut a = ScrollAccumulator::new();
        assert!(a.lines(100.0, true, CELL) > 0);
        a.reset();
        assert!(a.lines(-100.0, true, CELL) < 0);
        a.reset();
        assert!(a.lines(5.0, false, CELL) > 0);
        assert!(a.lines(-5.0, false, CELL) < 0);
    }

    #[test]
    fn reversing_direction_does_not_leave_a_stuck_fraction() {
        // Half a line down then half a line up is no movement, not one line in
        // whichever direction happened to cross the boundary first.
        let mut a = ScrollAccumulator::new();
        let down = a.lines(8.0, true, CELL);
        let up = a.lines(-8.0, true, CELL);
        assert_eq!(down + up, 0, "a there-and-back gesture moved {}", down + up);
    }

    #[test]
    fn a_retina_cell_height_is_not_used_as_if_it_were_points() {
        // Scroll deltas are in points. Handing this function a cell height in
        // device pixels would halve the distance per line and make scrolling
        // twice as fast on every Retina display — the bug is easy to write and
        // impossible to see without two machines.
        let mut points = ScrollAccumulator::new();
        let mut pixels = ScrollAccumulator::new();
        let a = points.lines(170.0, true, 17.0);
        let b = pixels.lines(170.0, true, 34.0);
        assert_eq!(a, 10);
        assert_eq!(b, 5);
        assert_ne!(a, b, "the two are meant to differ — this test documents which is right");
    }

    #[test]
    fn nonsense_input_scrolls_nothing() {
        let mut a = ScrollAccumulator::new();
        assert_eq!(a.lines(f64::NAN, true, CELL), 0);
        assert_eq!(a.lines(f64::INFINITY, true, CELL), 0);
        assert_eq!(a.lines(0.0, true, CELL), 0);
        // A zero cell height must not divide by zero.
        assert_eq!(a.lines(170.0, true, 0.0), 10);
    }

    #[test]
    fn reset_drops_the_partial_line() {
        let mut a = ScrollAccumulator::new();
        a.lines(8.0, true, CELL);
        a.reset();
        assert_eq!(a.lines(8.0, true, CELL), 0, "a stale fraction survived the reset");
    }
}

#[cfg(test)]
mod alternate_screen_tests {
    use super::*;
    use mica_core::backend::TerminalModes;

    fn modes(alt: bool, app: bool, mouse: bool) -> TerminalModes {
        TerminalModes { alt_screen: alt, application_cursor: app, mouse_reporting: mouse }
    }

    #[test]
    fn the_normal_screen_scrolls_the_viewport() {
        assert_eq!(route(3, modes(false, false, false)), ScrollTarget::Viewport(3));
        // Even with application cursor keys on: DECCKM says nothing about
        // whether there is scrollback.
        assert_eq!(route(-2, modes(false, true, false)), ScrollTarget::Viewport(-2));
    }

    #[test]
    fn the_alternate_screen_sends_arrow_keys() {
        assert_eq!(
            route(2, modes(true, false, false)),
            ScrollTarget::Keys(b"\x1b[A\x1b[A".to_vec()),
            "scrolling up in a full-screen program must send Up, not move a \
             viewport that does not exist"
        );
        assert_eq!(route(-1, modes(true, false, false)), ScrollTarget::Keys(b"\x1b[B".to_vec()));
    }

    #[test]
    fn application_cursor_mode_changes_the_spelling() {
        // `less` reads `ESC O A` and ignores `ESC [ A`. Getting this wrong is
        // indistinguishable from not implementing the feature.
        assert_eq!(route(1, modes(true, true, false)), ScrollTarget::Keys(b"\x1bOA".to_vec()));
        assert_eq!(route(-1, modes(true, true, false)), ScrollTarget::Keys(b"\x1bOB".to_vec()));
    }

    #[test]
    fn a_program_tracking_the_mouse_is_left_alone() {
        assert_eq!(
            route(5, modes(true, false, true)),
            ScrollTarget::Nothing,
            "inventing arrow keys under a program that reads the mouse itself \
             would make a scroll gesture edit the file"
        );
    }

    #[test]
    fn a_flick_cannot_send_an_unbounded_burst_of_keys() {
        let ScrollTarget::Keys(bytes) = route(10_000, modes(true, false, false)) else {
            panic!("the alternate screen should have produced keys");
        };
        assert_eq!(bytes.len(), 96 * 3, "a single flick queued {} bytes", bytes.len());
    }

    #[test]
    fn a_gesture_that_rounded_to_nothing_sends_nothing() {
        assert_eq!(route(0, modes(true, false, false)), ScrollTarget::Nothing);
    }
}
