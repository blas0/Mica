//! Frame scheduling.
//!
//! **This is the most load-bearing file in the project and it contains no
//! Metal.** The claim Mica is sold on — *"when nothing changes, the renderer
//! sends no frames at all; not fewer, none"* — is a scheduling property, not a
//! rendering one, so it is decided here and can be tested exhaustively without
//! a GPU.
//!
//! The rule: **a frame happens because something changed.** There is no
//! `CVDisplayLink`, no timer, no `setNeedsDisplay` on a tick. Every reason to
//! draw is an explicit [`Reason`], and if no reason arrives, `poll` returns
//! [`Decision::Idle`] forever and the command queue stays empty.
//!
//! The one apparent exception is animation — a blinking caret, a theme
//! cross-fade — and it is not an exception. An animation *is* a change, so it
//! registers as one and withdraws when it finishes. That is why
//! [`FrameScheduler::animations`] is a count that must go back to zero rather
//! than a boolean that someone forgets to clear.

use std::time::Duration;

/// Why a frame is wanted. Kept as an enum rather than a bool so a stuck
/// redraw loop can be diagnosed by asking what keeps asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reason {
    /// Terminal rows changed — the overwhelmingly common case.
    Damage,
    /// The window resized, or the backing scale changed.
    Resize,
    /// The caret moved, or its blink phase flipped.
    Cursor,
    /// An overlay opened, closed, or changed: the palette, find, the HUD.
    Overlay,
    /// Selection changed.
    Selection,
    /// Theme cross-fade, or any other running animation.
    Animation,
    /// The window became key, or lost focus.
    Focus,
    /// The atlas grew and its textures must be re-uploaded.
    Atlas,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Draw now.
    Draw,
    /// Nothing to do. **Submit no command buffer.**
    Idle,
    /// Something changed, but a frame is already in flight and the drawable
    /// pool is exhausted. Try again when it completes.
    Throttled,
}

/// Counters for the idle-frame test and the HUD.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameStats {
    /// Command buffers actually submitted. The number the idle test asserts on.
    pub submitted: u64,
    /// Times `poll` was asked and answered [`Decision::Idle`].
    pub idle_polls: u64,
    pub throttled: u64,
    pub dropped: u64,
}

/// Decides whether to draw.
#[derive(Debug)]
pub struct FrameScheduler {
    dirty: Option<Reason>,
    in_flight: u32,
    max_in_flight: u32,
    /// Number of running animations. While non-zero, every completed frame
    /// schedules the next one — and when it returns to zero, the terminal goes
    /// completely silent again.
    animations: u32,
    /// `0` follows the display; anything else is an explicit cap and implies a
    /// minimum interval between frames.
    frame_cap: u16,
    stats: FrameStats,
}

impl Default for FrameScheduler {
    fn default() -> FrameScheduler {
        FrameScheduler::new(2)
    }
}

impl FrameScheduler {
    /// `max_in_flight` should match `CAMetalLayer.maximumDrawableCount`.
    ///
    /// Two, not three: triple buffering trades latency for throughput, and a
    /// terminal is a latency instrument. The extra frame of smoothness is not
    /// worth 8 ms between a keystroke and its glyph.
    pub fn new(max_in_flight: u32) -> FrameScheduler {
        FrameScheduler {
            dirty: None,
            in_flight: 0,
            max_in_flight: max_in_flight.max(1),
            animations: 0,
            frame_cap: 0,
            stats: FrameStats::default(),
        }
    }

    pub fn set_frame_cap(&mut self, cap: u16) {
        self.frame_cap = cap;
    }

    /// The minimum interval between frames, if a cap is set.
    pub fn min_interval(&self) -> Option<Duration> {
        (self.frame_cap > 0)
            .then(|| Duration::from_secs_f64(1.0 / self.frame_cap as f64))
    }

    /// Registers a reason to draw. Idempotent within a frame: ten damaged rows
    /// are one frame, not ten.
    pub fn request(&mut self, reason: Reason) {
        // The first reason wins, so the counter names what *started* the
        // frame rather than whatever happened to arrive last.
        if self.dirty.is_none() {
            self.dirty = Some(reason);
        }
    }

    pub fn pending_reason(&self) -> Option<Reason> {
        self.dirty
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty.is_some()
    }

    /// Starts an animation, which keeps frames coming until it is stopped.
    pub fn begin_animation(&mut self) {
        self.animations += 1;
        self.request(Reason::Animation);
    }

    /// Ends one animation. When the last one ends, the terminal goes silent.
    pub fn end_animation(&mut self) {
        self.animations = self.animations.saturating_sub(1);
    }

    pub fn animations(&self) -> u32 {
        self.animations
    }

    pub fn in_flight(&self) -> u32 {
        self.in_flight
    }

    pub fn stats(&self) -> FrameStats {
        self.stats
    }

    /// Asks whether to draw. **Call this as often as you like** — polling is
    /// free and, crucially, polling an idle terminal submits nothing.
    pub fn poll(&mut self) -> Decision {
        if self.dirty.is_none() {
            self.stats.idle_polls += 1;
            return Decision::Idle;
        }
        if self.in_flight >= self.max_in_flight {
            self.stats.throttled += 1;
            return Decision::Throttled;
        }
        Decision::Draw
    }

    /// Called immediately before a command buffer is committed.
    pub fn begin_frame(&mut self) {
        debug_assert!(self.dirty.is_some(), "a frame was begun with nothing to draw");
        self.dirty = None;
        self.in_flight += 1;
        self.stats.submitted += 1;
    }

    /// Called from the command buffer's completion handler.
    ///
    /// A running animation re-arms here, which is what makes an animation a
    /// self-sustaining sequence of frames rather than a timer.
    pub fn end_frame(&mut self) {
        self.in_flight = self.in_flight.saturating_sub(1);
        if self.animations > 0 {
            self.request(Reason::Animation);
        }
    }

    /// Called when a drawable could not be obtained.
    ///
    /// The reason is put back: a frame that could not be drawn is still a
    /// frame that is owed, and dropping it silently is how a window ends up
    /// showing stale content until the next keystroke.
    pub fn drop_frame(&mut self, reason: Reason) {
        self.stats.dropped += 1;
        self.request(reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_scheduler_is_idle() {
        let mut s = FrameScheduler::default();
        assert_eq!(s.poll(), Decision::Idle);
        assert!(!s.is_dirty());
    }

    #[test]
    fn an_idle_terminal_submits_nothing_no_matter_how_often_it_is_polled() {
        // The single most important test in the project. Ten seconds at 120 Hz
        // is 1200 polls; every one of them must decline to draw.
        let mut s = FrameScheduler::default();
        for _ in 0..1200 {
            assert_eq!(s.poll(), Decision::Idle);
        }
        assert_eq!(s.stats().submitted, 0, "an idle window submitted a command buffer");
        assert_eq!(s.stats().idle_polls, 1200);
    }

    #[test]
    fn damage_produces_exactly_one_frame() {
        let mut s = FrameScheduler::default();
        s.request(Reason::Damage);
        assert_eq!(s.poll(), Decision::Draw);
        s.begin_frame();
        s.end_frame();

        assert_eq!(s.stats().submitted, 1);
        assert_eq!(s.poll(), Decision::Idle, "one change must not produce two frames");
    }

    #[test]
    fn many_damaged_rows_still_produce_one_frame() {
        let mut s = FrameScheduler::default();
        for _ in 0..500 {
            s.request(Reason::Damage);
        }
        assert_eq!(s.poll(), Decision::Draw);
        s.begin_frame();
        s.end_frame();
        assert_eq!(s.stats().submitted, 1);
    }

    #[test]
    fn the_first_reason_is_the_one_reported() {
        // So that "why is this window redrawing?" has an answer that points at
        // the cause rather than at whatever arrived most recently.
        let mut s = FrameScheduler::default();
        s.request(Reason::Damage);
        s.request(Reason::Cursor);
        assert_eq!(s.pending_reason(), Some(Reason::Damage));
    }

    #[test]
    fn frames_are_throttled_rather_than_queued_without_bound() {
        let mut s = FrameScheduler::new(2);
        s.request(Reason::Damage);
        assert_eq!(s.poll(), Decision::Draw);
        s.begin_frame();

        s.request(Reason::Damage);
        assert_eq!(s.poll(), Decision::Draw);
        s.begin_frame();

        s.request(Reason::Damage);
        assert_eq!(s.poll(), Decision::Throttled, "a third frame must wait for a drawable");
        assert_eq!(s.stats().submitted, 2);

        s.end_frame();
        assert_eq!(s.poll(), Decision::Draw, "a completed frame must free the slot");
    }

    #[test]
    fn an_animation_sustains_itself_and_then_stops_completely() {
        let mut s = FrameScheduler::default();
        s.begin_animation();

        // Ten frames of animation, each re-armed by the previous completion.
        for _ in 0..10 {
            assert_eq!(s.poll(), Decision::Draw);
            s.begin_frame();
            s.end_frame();
        }
        assert_eq!(s.stats().submitted, 10);

        s.end_animation();
        // The frame in flight at the moment the animation ended may still
        // re-arm once; after that the terminal must go silent.
        while s.poll() == Decision::Draw {
            s.begin_frame();
            s.end_frame();
        }
        let after = s.stats().submitted;
        for _ in 0..1000 {
            assert_eq!(s.poll(), Decision::Idle);
        }
        assert_eq!(s.stats().submitted, after, "the animation kept drawing after it ended");
    }

    #[test]
    fn overlapping_animations_each_have_to_end() {
        // A boolean would let whichever finished first switch the other off.
        let mut s = FrameScheduler::default();
        s.begin_animation();
        s.begin_animation();
        assert_eq!(s.animations(), 2);

        s.end_animation();
        assert_eq!(s.animations(), 1);
        s.begin_frame();
        s.end_frame();
        assert_eq!(s.poll(), Decision::Draw, "the second animation still wants frames");

        s.end_animation();
        s.begin_frame();
        s.end_frame();
        while s.poll() == Decision::Draw {
            s.begin_frame();
            s.end_frame();
        }
        assert_eq!(s.poll(), Decision::Idle);
    }

    #[test]
    fn ending_more_animations_than_were_started_does_not_underflow() {
        let mut s = FrameScheduler::default();
        s.end_animation();
        s.end_animation();
        assert_eq!(s.animations(), 0);
    }

    #[test]
    fn a_dropped_frame_is_owed_rather_than_lost() {
        let mut s = FrameScheduler::default();
        s.request(Reason::Damage);
        assert_eq!(s.poll(), Decision::Draw);
        s.begin_frame();
        // No drawable was available; the frame never reached the GPU.
        s.drop_frame(Reason::Damage);
        s.end_frame();

        assert_eq!(s.poll(), Decision::Draw, "the dropped frame was never redrawn");
        assert_eq!(s.stats().dropped, 1);
    }

    #[test]
    fn a_frame_cap_implies_a_minimum_interval_and_no_cap_implies_none() {
        let mut s = FrameScheduler::default();
        assert_eq!(s.min_interval(), None, "0 must mean follow the display");
        s.set_frame_cap(30);
        assert_eq!(s.min_interval(), Some(Duration::from_secs_f64(1.0 / 30.0)));
    }

    #[test]
    fn every_reason_can_wake_the_renderer() {
        for reason in [
            Reason::Damage,
            Reason::Resize,
            Reason::Cursor,
            Reason::Overlay,
            Reason::Selection,
            Reason::Animation,
            Reason::Focus,
            Reason::Atlas,
        ] {
            let mut s = FrameScheduler::default();
            s.request(reason);
            assert_eq!(s.poll(), Decision::Draw, "{reason:?} did not schedule a frame");
        }
    }
}
