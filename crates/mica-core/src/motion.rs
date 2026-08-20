//! Caret physics and the animation clock.
//!
//! Everything here is a pure function of `dt`. There is no clock, no
//! `Instant::now`, no thread: the caller measures the interval between frames
//! and hands it in. That is what makes seven motion styles testable without a
//! GPU, a window, or a sleep — [`Caret::advance`] with a fixed `dt` is a
//! deterministic sequence, so "does Spring settle?" is a `for` loop and an
//! assertion rather than something you squint at.
//!
//! ## Animation and the zero-idle-frame rule
//!
//! An animation is a *change*, so it is a legitimate reason to draw. It is
//! also the only thing in Mica that can keep drawing when the user has done
//! nothing, which is why every animation here is required to **end**:
//! [`Caret::advance`] returns whether anything is still moving, and every
//! style is tested to return `false` within a bounded number of steps. A style
//! that never settles would turn the terminal into a spinning fan, and the
//! test suite would rather find that than the user's battery.
//!
//! The one deliberate exception is blinking, which by construction never
//! settles. See [`MotionSettings::blink`].

use std::collections::VecDeque;
use std::time::Duration;

/// The longest step the integrators will take.
///
/// A frame that took 300 ms — a stall, a breakpoint, a laptop lid — must not
/// be handed to a spring as a single step, because an explicit integrator
/// with `dt` that large does not converge, it explodes. Clamping means a long
/// stall makes the caret arrive late rather than fly off screen.
const MAX_STEP: f32 = 1.0 / 30.0;

/// How close to its target the caret has to be before it is considered
/// arrived, in cells. Below about a hundredth of a cell nothing is visible at
/// any sane font size, and something has to stop the animation or it runs
/// forever chasing an asymptote.
const SETTLED: f32 = 0.004;

/// Reduce Motion collapses every animation to this.
pub const REDUCED_FADE: Duration = Duration::from_millis(90);

/// A full blink cycle: on, off, on.
const BLINK_PERIOD: f32 = 1.06;

/// How long the caret stays solid after moving. Typing should not blink —
/// the caret is obviously where the text is appearing.
const BLINK_HOLD: f32 = 0.5;

/// The ramp at each end of a blink, so the caret breathes rather than
/// strobing. Set to zero for [`MotionStyle::Snap`], which is the point of it.
const BLINK_RAMP: f32 = 0.11;

/// Trail samples are dropped after this many, oldest first.
const MAX_TRAIL: usize = 24;

/// How far the caret must travel between trail samples, in cells.
const TRAIL_SPACING: f32 = 0.34;

/// How the caret gets from one cell to the next.
///
/// Seven styles, because the reference app has seven and they are genuinely
/// different feels rather than seven tunings of one curve. They divide into
/// how the caret *travels* (Snap, Ease, Spring, Arc) and how it *deforms*
/// while travelling (Smear, Squash, Phosphor) — the deforming styles all use
/// the Ease curve underneath.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MotionStyle {
    /// No interpolation at all. The caret is simply in the new cell.
    Snap,
    /// Exponential approach. The default: quick, unfussy, never overshoots.
    #[default]
    Ease,
    /// A damped spring that overshoots slightly and comes back.
    Spring,
    /// Ease, but the caret softens along its direction of travel.
    Smear,
    /// Ease, but the caret stretches along its travel and thins across it,
    /// conserving area the way a squashed ball does.
    Squash,
    /// Ease with a long, slow trail — a CRT's afterglow.
    Phosphor,
    /// Travels along an arc rather than a straight line.
    Arc,
}

impl MotionStyle {
    pub const ALL: [MotionStyle; 7] = [
        MotionStyle::Snap,
        MotionStyle::Ease,
        MotionStyle::Spring,
        MotionStyle::Smear,
        MotionStyle::Squash,
        MotionStyle::Phosphor,
        MotionStyle::Arc,
    ];

    pub fn id(self) -> &'static str {
        match self {
            MotionStyle::Snap => "snap",
            MotionStyle::Ease => "ease",
            MotionStyle::Spring => "spring",
            MotionStyle::Smear => "smear",
            MotionStyle::Squash => "squash",
            MotionStyle::Phosphor => "phosphor",
            MotionStyle::Arc => "arc",
        }
    }

    pub fn from_id(id: &str) -> Option<MotionStyle> {
        MotionStyle::ALL.into_iter().find(|style| style.id() == id)
    }

    /// Human-readable, for the palette and the settings UI.
    pub fn label(self) -> &'static str {
        match self {
            MotionStyle::Snap => "Snap",
            MotionStyle::Ease => "Ease",
            MotionStyle::Spring => "Spring",
            MotionStyle::Smear => "Smear",
            MotionStyle::Squash => "Squash",
            MotionStyle::Phosphor => "Phosphor",
            MotionStyle::Arc => "Arc",
        }
    }

    /// Whether the caret interpolates at all, before Reduce Motion is applied.
    pub fn interpolates(self) -> bool {
        self != MotionStyle::Snap
    }

    /// How long a trail sample lives, in seconds. Zero means no trail.
    fn trail_life(self) -> f32 {
        match self {
            MotionStyle::Phosphor => 0.40,
            MotionStyle::Smear => 0.18,
            MotionStyle::Squash | MotionStyle::Arc | MotionStyle::Spring | MotionStyle::Ease => 0.13,
            MotionStyle::Snap => 0.0,
        }
    }
}

/// Everything the caret needs to know about how it should behave.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionSettings {
    pub style: MotionStyle,
    /// Multiplier on every rate. Clamped to a sane band on construction:
    /// a speed of zero is a caret that never arrives.
    pub speed: f32,
    /// How strongly the style's characteristic distortion applies, `0..=1`.
    /// At zero, Smear, Squash and Arc are all just Ease.
    pub intensity: f32,
    /// Emit the decay trail. Costs one instance per sample and nothing when
    /// the caret is still.
    pub decay: bool,
    /// Blink the caret when it is idle.
    ///
    /// **A blinking caret never stops animating**, which means it never stops
    /// drawing — roughly one frame per display refresh, forever, for a purely
    /// decorative effect. That is the one thing this design is built to avoid,
    /// so it is off by default and the setting says so.
    pub blink: bool,
    /// Collapse everything to a 90 ms fade, honouring either the app setting
    /// or the system Reduce Motion preference.
    pub reduce: bool,
}

impl Default for MotionSettings {
    fn default() -> MotionSettings {
        MotionSettings {
            style: MotionStyle::default(),
            speed: 1.0,
            intensity: 0.7,
            decay: true,
            blink: false,
            reduce: false,
        }
    }
}

impl MotionSettings {
    /// Clamps the free-floating numbers into ranges the integrators survive.
    ///
    /// Applied on load rather than trusted: `speed = 0` in a hand-edited TOML
    /// would be a caret that never reaches its cell, and the user would have
    /// no way to tell that from a hang.
    pub fn sanitised(mut self) -> MotionSettings {
        self.speed = self.speed.clamp(0.25, 4.0);
        self.intensity = self.intensity.clamp(0.0, 1.0);
        if !self.speed.is_finite() {
            self.speed = 1.0;
        }
        if !self.intensity.is_finite() {
            self.intensity = 0.7;
        }
        self
    }

    /// The effective style, after Reduce Motion.
    pub fn effective_style(&self) -> MotionStyle {
        if self.reduce {
            MotionStyle::Snap
        } else {
            self.style
        }
    }
}

/// One sample of the caret's wake.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrailSample {
    /// Cell coordinates, fractional.
    pub position: [f32; 2],
    /// Unit vector along travel, for the shader's directional fade.
    pub direction: [f32; 2],
    /// `0` just emitted, `1` gone.
    pub age: f32,
}

/// How the caret should be drawn this frame.
///
/// Cell coordinates and multipliers rather than pixels: the renderer owns cell
/// metrics, and this crate must not learn about them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaretPresentation {
    /// Fractional cell position — this is the sub-cell interpolation.
    pub position: [f32; 2],
    /// Multipliers on cell width and height. Squash is the only style that
    /// moves these off `[1, 1]`.
    pub scale: [f32; 2],
    /// `0` hard-edged, `1` fully smeared. Feeds the shape shader directly.
    pub softness: f32,
    /// Blink and Reduce Motion both land here.
    pub alpha: f32,
}

/// The caret's position, velocity, wake, and blink phase.
#[derive(Debug, Clone)]
pub struct Caret {
    position: [f32; 2],
    target: [f32; 2],
    velocity: [f32; 2],
    /// Where the current flight began, and how long it is. Arc needs both to
    /// know where the middle of the journey is.
    flight_from: [f32; 2],
    flight_length: f32,
    /// Cells travelled since the last trail sample was emitted.
    since_sample: f32,
    trail: VecDeque<TrailSample>,
    /// Seconds into the blink cycle.
    blink_phase: f32,
    /// Seconds left of the solid-while-typing hold.
    hold: f32,
    /// The Reduce Motion fade, `0..=1`.
    fade: f32,
}

impl Caret {
    pub fn new(column: f32, line: f32) -> Caret {
        Caret {
            position: [column, line],
            target: [column, line],
            velocity: [0.0, 0.0],
            flight_from: [column, line],
            flight_length: 0.0,
            since_sample: 0.0,
            trail: VecDeque::new(),
            blink_phase: 0.0,
            hold: BLINK_HOLD,
            fade: 1.0,
        }
    }

    pub fn position(&self) -> [f32; 2] {
        self.position
    }

    pub fn target(&self) -> [f32; 2] {
        self.target
    }

    pub fn trail(&self) -> impl Iterator<Item = &TrailSample> {
        self.trail.iter()
    }

    /// Whether anything is still in motion — the answer the frame scheduler
    /// needs. Blinking counts, and is the only thing here that never stops.
    pub fn is_animating(&self, motion: &MotionSettings) -> bool {
        self.position != self.target
            || !self.trail.is_empty()
            || self.fade < 1.0
            || motion.blink
    }

    /// Points the caret at a new cell.
    ///
    /// Called on every cursor move, including the one per keystroke while
    /// typing, so it must be cheap and must not restart an in-flight animation
    /// from scratch — the caret keeps its velocity and simply aims somewhere
    /// new, which is what makes fast typing look like one continuous glide
    /// rather than a sequence of separate hops.
    pub fn retarget(&mut self, column: f32, line: f32, motion: &MotionSettings) {
        let target = [column, line];
        if target == self.target {
            return;
        }
        self.target = target;
        self.hold = BLINK_HOLD;
        self.blink_phase = 0.0;

        if !motion.effective_style().interpolates() {
            // Snap, or Reduce Motion: no travel, but the arrival still fades
            // in so the caret does not simply teleport with a hard cut.
            self.position = target;
            self.velocity = [0.0, 0.0];
            self.trail.clear();
            self.fade = if motion.reduce { 0.0 } else { 1.0 };
            return;
        }

        self.flight_from = self.position;
        self.flight_length = distance(self.position, target);
        self.since_sample = TRAIL_SPACING; // emit one immediately
        self.fade = 1.0;
    }

    /// Places the caret with no animation at all.
    ///
    /// For a resize or a theme change, where interpolating from the old
    /// position would animate a move the user did not make.
    pub fn teleport(&mut self, column: f32, line: f32) {
        self.position = [column, line];
        self.target = self.position;
        self.flight_from = self.position;
        self.velocity = [0.0, 0.0];
        self.flight_length = 0.0;
        self.trail.clear();
        self.fade = 1.0;
        self.hold = BLINK_HOLD;
        self.blink_phase = 0.0;
    }

    /// Steps the physics. Returns whether anything is still animating.
    pub fn advance(&mut self, dt: Duration, motion: &MotionSettings) -> bool {
        let dt = (dt.as_secs_f32()).clamp(0.0, MAX_STEP);
        if dt > 0.0 {
            self.age_trail(dt, motion);
            self.step_position(dt, motion);
            self.step_blink(dt, motion);
            if self.fade < 1.0 {
                self.fade = (self.fade + dt / REDUCED_FADE.as_secs_f32()).min(1.0);
            }
        }
        self.is_animating(motion)
    }

    fn age_trail(&mut self, dt: f32, motion: &MotionSettings) {
        let life = motion.effective_style().trail_life();
        if life <= 0.0 || !motion.decay {
            self.trail.clear();
            return;
        }
        let step = dt / life;
        for sample in &mut self.trail {
            sample.age += step;
        }
        while self.trail.front().is_some_and(|s| s.age >= 1.0) {
            self.trail.pop_front();
        }
    }

    fn step_position(&mut self, dt: f32, motion: &MotionSettings) {
        let style = motion.effective_style();
        if !style.interpolates() {
            self.position = self.target;
            self.velocity = [0.0, 0.0];
            return;
        }
        if self.position == self.target && self.velocity == [0.0, 0.0] {
            return;
        }

        let previous = self.position;
        match style {
            MotionStyle::Spring => self.step_spring(dt, motion.speed),
            _ => self.step_ease(dt, motion.speed),
        }
        if style == MotionStyle::Arc {
            self.apply_arc(motion.intensity);
        }

        // Velocity is recovered from the actual movement rather than kept as
        // the integrator's own state, because Arc bends the path after the
        // fact and Smear and Squash want the velocity of what was *drawn*.
        self.velocity = [
            (self.position[0] - previous[0]) / dt,
            (self.position[1] - previous[1]) / dt,
        ];

        if distance(self.position, self.target) < SETTLED && magnitude(self.velocity) < 0.05 {
            // Snapping exactly is what lets `is_animating` return false. An
            // asymptotic approach that never quite arrives is an animation
            // that never quite ends.
            self.position = self.target;
            self.velocity = [0.0, 0.0];
            self.since_sample = 0.0;
            return;
        }

        if motion.decay && style.trail_life() > 0.0 {
            self.since_sample += distance(previous, self.position);
            if self.since_sample >= TRAIL_SPACING {
                self.since_sample = 0.0;
                self.emit_sample();
            }
        }
    }

    /// Exponential approach: the distance remaining is multiplied by a
    /// constant factor each second, which is frame-rate independent by
    /// construction — `exp` of the elapsed time, not a fixed fraction per
    /// frame. A caret that eases differently at 60 Hz and 120 Hz is the
    /// classic version of this bug.
    fn step_ease(&mut self, dt: f32, speed: f32) {
        let tau = 0.055 / speed;
        let k = 1.0 - (-dt / tau).exp();
        for axis in 0..2 {
            self.position[axis] += (self.target[axis] - self.position[axis]) * k;
        }
    }

    /// A damped spring, integrated semi-implicitly.
    ///
    /// `zeta` below 1 is what produces the overshoot that makes Spring feel
    /// different from Ease; much below 0.6 and the caret visibly wobbles,
    /// which reads as a bug rather than as a flourish.
    fn step_spring(&mut self, dt: f32, speed: f32) {
        let omega = 30.0 * speed;
        let zeta = 0.68;
        for axis in 0..2 {
            let displacement = self.position[axis] - self.target[axis];
            let acceleration = -omega * omega * displacement - 2.0 * zeta * omega * self.velocity[axis];
            self.velocity[axis] += acceleration * dt;
            self.position[axis] += self.velocity[axis] * dt;
        }
    }

    /// Bends the straight-line path sideways, peaking at the midpoint.
    fn apply_arc(&mut self, intensity: f32) {
        if self.flight_length < 0.5 {
            return;
        }
        let travelled = distance(self.flight_from, self.position);
        let progress = (travelled / self.flight_length).clamp(0.0, 1.0);
        let amplitude = (self.flight_length * 0.18).min(0.9) * intensity;
        let direction = normalise([
            self.target[0] - self.flight_from[0],
            self.target[1] - self.flight_from[1],
        ]);
        // Perpendicular, and negated on the vertical so a rightward move bows
        // upward rather than into the line below.
        let perpendicular = [direction[1], -direction[0]];
        let bow = (std::f32::consts::PI * progress).sin() * amplitude;
        self.position[0] += perpendicular[0] * bow;
        self.position[1] += perpendicular[1] * bow;
    }

    fn emit_sample(&mut self) {
        if self.trail.len() >= MAX_TRAIL {
            self.trail.pop_front();
        }
        self.trail.push_back(TrailSample {
            position: self.position,
            direction: normalise(self.velocity),
            age: 0.0,
        });
    }

    fn step_blink(&mut self, dt: f32, motion: &MotionSettings) {
        if self.position != self.target {
            self.hold = BLINK_HOLD;
            self.blink_phase = 0.0;
            return;
        }
        if self.hold > 0.0 {
            self.hold = (self.hold - dt).max(0.0);
            return;
        }
        if !motion.blink {
            self.blink_phase = 0.0;
            return;
        }
        self.blink_phase = (self.blink_phase + dt) % BLINK_PERIOD;
    }

    /// What to draw this frame.
    pub fn presentation(&self, motion: &MotionSettings) -> CaretPresentation {
        let style = motion.effective_style();
        let speed = magnitude(self.velocity);

        let softness = match style {
            MotionStyle::Smear => (speed * 0.045 * motion.intensity).clamp(0.0, 0.85),
            MotionStyle::Phosphor => (speed * 0.020 * motion.intensity).clamp(0.0, 0.45),
            _ => 0.0,
        };

        let scale = if style == MotionStyle::Squash && speed > 0.01 {
            // Stretch along travel, thin across it, keeping the area constant
            // — the caret has the same visual weight moving as it does still.
            let stretch = 1.0 + (speed * 0.030 * motion.intensity).clamp(0.0, 1.1);
            let unit = normalise(self.velocity);
            let horizontal = unit[0].abs();
            [
                1.0 + (stretch - 1.0) * horizontal,
                1.0 + (1.0 / stretch - 1.0) * horizontal,
            ]
        } else {
            [1.0, 1.0]
        };

        CaretPresentation {
            position: self.position,
            scale,
            softness,
            alpha: self.fade * self.blink_alpha(motion),
        }
    }

    /// `1` solid, `0` invisible, with a ramp at each end unless the style is
    /// Snap — where a hard edge is the whole point.
    ///
    /// The cycle is solid for the first half and dark for the second, with the
    /// ramps sitting just *before* each transition so the caret is fully solid
    /// and fully dark for most of its half rather than permanently mid-fade.
    fn blink_alpha(&self, motion: &MotionSettings) -> f32 {
        if !motion.blink || self.hold > 0.0 {
            return 1.0;
        }
        let half = BLINK_PERIOD * 0.5;
        let ramp = if motion.effective_style().interpolates() { BLINK_RAMP } else { 0.0 };
        let phase = self.blink_phase;

        if ramp <= 0.0 {
            return if phase < half { 1.0 } else { 0.0 };
        }
        if phase < half - ramp {
            1.0
        } else if phase < half {
            1.0 - smoothstep((phase - (half - ramp)) / ramp)
        } else if phase < BLINK_PERIOD - ramp {
            0.0
        } else {
            smoothstep((phase - (BLINK_PERIOD - ramp)) / ramp)
        }
    }
}

/// A timed cross-fade, `0` at the start and `1` at the end.
///
/// Used for theme switching, which is the other thing on the page that is
/// claimed to fade rather than cut. Kept here rather than in `material` so
/// that everything with a duration lives in one file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Crossfade {
    elapsed: f32,
    duration: f32,
}

impl Crossfade {
    /// A cross-fade that has already finished, which is the resting state.
    pub fn done() -> Crossfade {
        Crossfade { elapsed: 1.0, duration: 1.0 }
    }

    pub fn start(duration: Duration, motion: &MotionSettings) -> Crossfade {
        let duration = if motion.reduce {
            REDUCED_FADE.as_secs_f32()
        } else {
            (duration.as_secs_f32() / motion.speed).max(0.001)
        };
        Crossfade { elapsed: 0.0, duration }
    }

    /// Returns whether the fade is still running.
    pub fn advance(&mut self, dt: Duration) -> bool {
        self.elapsed = (self.elapsed + dt.as_secs_f32().clamp(0.0, MAX_STEP)).min(self.duration);
        self.is_running()
    }

    pub fn is_running(&self) -> bool {
        self.elapsed < self.duration
    }

    /// Eased `0..=1`, for handing straight to `Material::blend`.
    pub fn t(&self) -> f32 {
        smoothstep((self.elapsed / self.duration).clamp(0.0, 1.0))
    }
}

fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    magnitude([b[0] - a[0], b[1] - a[1]])
}

fn magnitude(v: [f32; 2]) -> f32 {
    (v[0] * v[0] + v[1] * v[1]).sqrt()
}

fn normalise(v: [f32; 2]) -> [f32; 2] {
    let m = magnitude(v);
    if m < 1e-6 {
        [0.0, 0.0]
    } else {
        [v[0] / m, v[1] / m]
    }
}

/// The usual smoothstep, on an input already in `0..=1`.
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One 120 Hz frame. Every test steps in fixed increments so the whole
    /// module is deterministic — no clock, no sleep, no flake.
    const FRAME: Duration = Duration::from_micros(8_333);

    fn settings(style: MotionStyle) -> MotionSettings {
        MotionSettings { style, blink: false, ..MotionSettings::default() }
    }

    /// Steps until the caret reports it has stopped, or gives up.
    fn settle(caret: &mut Caret, motion: &MotionSettings, limit: usize) -> usize {
        for step in 1..=limit {
            if !caret.advance(FRAME, motion) {
                return step;
            }
        }
        panic!(
            "{} did not settle in {limit} frames — position {:?}, target {:?}",
            motion.style.label(),
            caret.position(),
            caret.target()
        );
    }

    #[test]
    fn every_style_settles_and_arrives_exactly() {
        // The load-bearing test of this module. A style that never settles is
        // a terminal that never stops drawing; a style that settles somewhere
        // other than the target is a caret in the wrong cell forever.
        for style in MotionStyle::ALL {
            let motion = settings(style);
            let mut caret = Caret::new(0.0, 0.0);
            caret.retarget(40.0, 12.0, &motion);

            // Two seconds at 120 Hz. Anything slower than that is not motion,
            // it is a wait.
            let frames = settle(&mut caret, &motion, 240);
            assert_eq!(
                caret.position(),
                [40.0, 12.0],
                "{} settled off-target",
                style.label()
            );
            assert!(
                !caret.is_animating(&motion),
                "{} reports itself still animating after settling",
                style.label()
            );
            assert!(frames > 0, "{} settled in zero frames", style.label());
        }
    }

    #[test]
    fn snap_arrives_without_a_single_animated_frame() {
        let motion = settings(MotionStyle::Snap);
        let mut caret = Caret::new(0.0, 0.0);
        caret.retarget(30.0, 5.0, &motion);
        assert_eq!(caret.position(), [30.0, 5.0], "Snap interpolated");
        assert!(!caret.is_animating(&motion), "Snap asked for a frame");
    }

    #[test]
    fn interpolating_styles_actually_take_time() {
        // The other half of the previous test: if a style settles in one frame
        // it is Snap wearing a different name, and the setting does nothing.
        for style in [MotionStyle::Ease, MotionStyle::Spring, MotionStyle::Arc] {
            let motion = settings(style);
            let mut caret = Caret::new(0.0, 0.0);
            caret.retarget(40.0, 0.0, &motion);
            caret.advance(FRAME, &motion);
            let travelled = caret.position()[0];
            assert!(
                travelled > 0.0 && travelled < 40.0,
                "{} was at {travelled} after one frame — it did not interpolate",
                style.label()
            );
        }
    }

    #[test]
    fn reduce_motion_collapses_every_style_to_the_same_fade() {
        // "One toggle disables all of it" has to mean all of it, including the
        // styles whose whole identity is deformation.
        for style in MotionStyle::ALL {
            let motion = MotionSettings { reduce: true, ..settings(style) };
            let mut caret = Caret::new(0.0, 0.0);
            caret.retarget(40.0, 12.0, &motion);

            assert_eq!(caret.position(), [40.0, 12.0], "{} travelled under Reduce Motion", style.label());
            let start = caret.presentation(&motion);
            assert!(start.alpha < 1.0, "{} did not fade in", style.label());
            assert_eq!(start.scale, [1.0, 1.0], "{} deformed under Reduce Motion", style.label());
            assert_eq!(start.softness, 0.0, "{} softened under Reduce Motion", style.label());
            assert!(caret.trail().next().is_none(), "{} left a trail", style.label());

            // The fade is 90 ms, so it is over within 90 ms and not before.
            let mut elapsed = Duration::ZERO;
            while caret.advance(FRAME, &motion) {
                elapsed += FRAME;
                assert!(elapsed <= REDUCED_FADE + FRAME, "{} faded for too long", style.label());
            }
            assert!(elapsed >= REDUCED_FADE - FRAME, "{} faded too fast", style.label());
        }
    }

    #[test]
    fn a_long_stall_does_not_launch_the_caret_into_space() {
        // A breakpoint, a lid, a swapped-out process: `dt` arrives enormous.
        // An unclamped spring diverges on a step that size, and the caret ends
        // up at some coordinate with an exponent in it.
        let motion = settings(MotionStyle::Spring);
        let mut caret = Caret::new(0.0, 0.0);
        caret.retarget(10.0, 0.0, &motion);
        caret.advance(Duration::from_secs(3), &motion);

        let [x, y] = caret.position();
        assert!(x.is_finite() && y.is_finite(), "the spring diverged to {x},{y}");
        assert!(x.abs() < 100.0, "the spring overshot to {x}");
        settle(&mut caret, &motion, 240);
        assert_eq!(caret.position(), [10.0, 0.0]);
    }

    #[test]
    fn easing_is_frame_rate_independent() {
        // The classic bug this guards: interpolating by a fixed fraction each
        // frame makes the caret twice as fast at 120 Hz as at 60 Hz. Same
        // elapsed time must mean the same position, whatever the step size.
        let motion = settings(MotionStyle::Ease);
        let mut fast = Caret::new(0.0, 0.0);
        let mut slow = Caret::new(0.0, 0.0);
        fast.retarget(40.0, 0.0, &motion);
        slow.retarget(40.0, 0.0, &motion);

        for _ in 0..12 {
            fast.advance(Duration::from_micros(8_333), &motion);
        }
        for _ in 0..6 {
            slow.advance(Duration::from_micros(16_666), &motion);
        }
        let difference = (fast.position()[0] - slow.position()[0]).abs();
        assert!(
            difference < 0.25,
            "60 Hz reached {} and 120 Hz reached {} over the same 100 ms",
            slow.position()[0],
            fast.position()[0]
        );
    }

    #[test]
    fn retargeting_mid_flight_keeps_one_continuous_glide() {
        // Typing retargets on every keystroke. If each one restarted the
        // animation the caret would stutter rather than glide.
        let motion = settings(MotionStyle::Ease);
        let mut caret = Caret::new(0.0, 0.0);
        caret.retarget(10.0, 0.0, &motion);
        for _ in 0..3 {
            caret.advance(FRAME, &motion);
        }
        let mid = caret.position()[0];
        caret.retarget(20.0, 0.0, &motion);
        assert_eq!(caret.position()[0], mid, "retargeting teleported the caret");
        settle(&mut caret, &motion, 240);
        assert_eq!(caret.position(), [20.0, 0.0]);
    }

    #[test]
    fn the_trail_is_emitted_while_moving_and_gone_once_still() {
        let motion = settings(MotionStyle::Phosphor);
        let mut caret = Caret::new(0.0, 0.0);
        caret.retarget(40.0, 0.0, &motion);
        for _ in 0..6 {
            caret.advance(FRAME, &motion);
        }
        assert!(caret.trail().count() > 0, "a moving caret left no trail");
        assert!(caret.trail().all(|s| s.age < 1.0), "a dead sample was kept");

        settle(&mut caret, &motion, 240);
        assert_eq!(caret.trail().count(), 0, "the trail outlived the movement");
    }

    #[test]
    fn the_trail_is_bounded_however_far_the_caret_travels() {
        let motion = settings(MotionStyle::Phosphor);
        let mut caret = Caret::new(0.0, 0.0);
        for target in 1..200 {
            caret.retarget(target as f32 * 7.0, 0.0, &motion);
            caret.advance(FRAME, &motion);
            assert!(
                caret.trail().count() <= MAX_TRAIL,
                "the trail grew to {}",
                caret.trail().count()
            );
        }
    }

    #[test]
    fn disabling_decay_removes_the_trail_without_touching_the_motion() {
        let with = settings(MotionStyle::Phosphor);
        let without = MotionSettings { decay: false, ..with };
        let (mut a, mut b) = (Caret::new(0.0, 0.0), Caret::new(0.0, 0.0));
        a.retarget(40.0, 0.0, &with);
        b.retarget(40.0, 0.0, &without);
        for _ in 0..8 {
            a.advance(FRAME, &with);
            b.advance(FRAME, &without);
        }
        assert!(a.trail().count() > 0);
        assert_eq!(b.trail().count(), 0);
        assert_eq!(a.position(), b.position(), "decay changed where the caret is");
    }

    #[test]
    fn squash_conserves_area_and_only_deforms_while_moving() {
        let motion = MotionSettings { intensity: 1.0, ..settings(MotionStyle::Squash) };
        let mut caret = Caret::new(0.0, 0.0);
        caret.retarget(60.0, 0.0, &motion);
        caret.advance(FRAME, &motion);

        let moving = caret.presentation(&motion);
        assert!(moving.scale[0] > 1.0, "the caret did not stretch: {:?}", moving.scale);
        assert!(moving.scale[1] < 1.0, "the caret did not thin: {:?}", moving.scale);

        settle(&mut caret, &motion, 240);
        assert_eq!(caret.presentation(&motion).scale, [1.0, 1.0], "a still caret is deformed");
    }

    #[test]
    fn smear_softens_with_speed_and_hardens_when_still() {
        let motion = settings(MotionStyle::Smear);
        let mut caret = Caret::new(0.0, 0.0);
        caret.retarget(60.0, 0.0, &motion);
        caret.advance(FRAME, &motion);
        let moving = caret.presentation(&motion).softness;
        assert!(moving > 0.0, "a moving smear caret was hard-edged");
        assert!(moving <= 0.85, "softness escaped its clamp: {moving}");

        settle(&mut caret, &motion, 240);
        assert_eq!(caret.presentation(&motion).softness, 0.0);
    }

    #[test]
    fn intensity_zero_makes_the_deforming_styles_behave_like_ease() {
        let ease = settings(MotionStyle::Ease);
        for style in [MotionStyle::Smear, MotionStyle::Squash, MotionStyle::Arc] {
            let motion = MotionSettings { intensity: 0.0, ..settings(style) };
            let (mut plain, mut styled) = (Caret::new(0.0, 0.0), Caret::new(0.0, 0.0));
            plain.retarget(40.0, 3.0, &ease);
            styled.retarget(40.0, 3.0, &motion);
            for _ in 0..10 {
                plain.advance(FRAME, &ease);
                styled.advance(FRAME, &motion);
            }
            let presentation = styled.presentation(&motion);
            assert_eq!(presentation.scale, [1.0, 1.0], "{} deformed at intensity 0", style.label());
            assert_eq!(presentation.softness, 0.0, "{} softened at intensity 0", style.label());
            let drift = distance(plain.position(), styled.position());
            assert!(drift < 0.01, "{} drifted {drift} cells from Ease at intensity 0", style.label());
        }
    }

    #[test]
    fn arc_leaves_the_straight_line_and_comes_back_to_it() {
        let motion = MotionSettings { intensity: 1.0, ..settings(MotionStyle::Arc) };
        let mut caret = Caret::new(0.0, 0.0);
        caret.retarget(40.0, 0.0, &motion);

        let mut peak: f32 = 0.0;
        for _ in 0..60 {
            caret.advance(FRAME, &motion);
            peak = peak.max(caret.position()[1].abs());
        }
        assert!(peak > 0.05, "Arc travelled in a straight line (peak bow {peak})");
        settle(&mut caret, &motion, 240);
        assert_eq!(caret.position(), [40.0, 0.0], "Arc did not come back to the line");
    }

    #[test]
    fn speed_changes_how_long_the_journey_takes() {
        let slow = MotionSettings { speed: 0.25, ..settings(MotionStyle::Ease) };
        let fast = MotionSettings { speed: 4.0, ..settings(MotionStyle::Ease) };
        let mut a = Caret::new(0.0, 0.0);
        let mut b = Caret::new(0.0, 0.0);
        a.retarget(40.0, 0.0, &slow);
        b.retarget(40.0, 0.0, &fast);
        let slow_frames = settle(&mut a, &slow, 600);
        let fast_frames = settle(&mut b, &fast, 600);
        assert!(
            slow_frames > fast_frames * 3,
            "speed barely mattered: {slow_frames} frames slow vs {fast_frames} fast"
        );
    }

    #[test]
    fn a_still_caret_with_blinking_off_asks_for_no_frames() {
        // The property the whole renderer is built on, restated where the one
        // feature that could break it lives.
        let motion = settings(MotionStyle::Ease);
        let mut caret = Caret::new(4.0, 4.0);
        for _ in 0..1200 {
            assert!(!caret.advance(FRAME, &motion), "an idle caret asked for a frame");
        }
        assert_eq!(caret.presentation(&motion).alpha, 1.0);
    }

    #[test]
    fn blinking_is_honestly_reported_as_a_never_ending_animation() {
        // Not a bug to be fixed later: a blink is a change on a schedule, and
        // a change on a schedule is frames. The test exists so nobody
        // "optimises" it into silence and ships a caret that stops blinking.
        let motion = MotionSettings { blink: true, ..settings(MotionStyle::Ease) };
        let mut caret = Caret::new(4.0, 4.0);
        for _ in 0..1200 {
            assert!(caret.advance(FRAME, &motion));
        }
    }

    #[test]
    fn the_caret_stays_solid_while_typing_and_only_then_blinks() {
        let motion = MotionSettings { blink: true, ..settings(MotionStyle::Ease) };
        let mut caret = Caret::new(0.0, 0.0);

        // Type twenty characters at 40 ms apart — faster than the hold, which
        // is the point: the caret must never dim mid-word.
        for column in 1..=20 {
            caret.retarget(column as f32, 0.0, &motion);
            for _ in 0..5 {
                caret.advance(Duration::from_millis(8), &motion);
                assert_eq!(
                    caret.presentation(&motion).alpha,
                    1.0,
                    "the caret dimmed while typing"
                );
            }
        }

        // Then stop, and it should reach fully dark within one cycle.
        let mut darkest: f32 = 1.0;
        for _ in 0..300 {
            caret.advance(FRAME, &motion);
            darkest = darkest.min(caret.presentation(&motion).alpha);
        }
        assert_eq!(darkest, 0.0, "the caret never blinked off once typing stopped");
    }

    #[test]
    fn the_blink_fades_rather_than_strobing_unless_the_style_is_snap() {
        let smooth = MotionSettings { blink: true, ..settings(MotionStyle::Ease) };
        let hard = MotionSettings { blink: true, ..settings(MotionStyle::Snap) };

        let seen = |motion: &MotionSettings| {
            let mut caret = Caret::new(0.0, 0.0);
            let mut partial = 0;
            for _ in 0..400 {
                caret.advance(FRAME, motion);
                let alpha = caret.presentation(motion).alpha;
                if alpha > 0.05 && alpha < 0.95 {
                    partial += 1;
                }
            }
            partial
        };
        assert!(seen(&smooth) > 5, "the smooth blink had no intermediate frames");
        assert_eq!(seen(&hard), 0, "Snap's blink was not a hard edge");
    }

    #[test]
    fn settings_are_clamped_into_ranges_the_integrators_survive() {
        let broken = MotionSettings {
            speed: 0.0,
            intensity: 40.0,
            ..MotionSettings::default()
        }
        .sanitised();
        assert!(broken.speed >= 0.25, "speed 0 is a caret that never arrives");
        assert_eq!(broken.intensity, 1.0);

        let nonsense = MotionSettings {
            speed: f32::NAN,
            intensity: f32::INFINITY,
            ..MotionSettings::default()
        }
        .sanitised();
        assert!(nonsense.speed.is_finite() && nonsense.intensity.is_finite());
    }

    #[test]
    fn style_ids_round_trip_and_are_all_distinct() {
        // The ids go in a TOML file and in palette action names, so they are a
        // public contract.
        let mut seen = std::collections::HashSet::new();
        for style in MotionStyle::ALL {
            assert!(seen.insert(style.id()), "duplicate id {}", style.id());
            assert_eq!(MotionStyle::from_id(style.id()), Some(style));
        }
        assert_eq!(MotionStyle::from_id("nonexistent"), None);
        assert_eq!(MotionStyle::ALL.len(), 7, "the page promises seven styles");
    }

    #[test]
    fn a_crossfade_runs_once_and_stops() {
        let motion = MotionSettings::default();
        let mut fade = Crossfade::start(Duration::from_millis(180), &motion);
        assert_eq!(fade.t(), 0.0);
        let mut frames = 0;
        while fade.advance(FRAME) {
            frames += 1;
            assert!(frames < 240, "the cross-fade never finished");
        }
        assert_eq!(fade.t(), 1.0);
        assert!(!fade.advance(FRAME), "a finished cross-fade restarted");
    }

    #[test]
    fn reduce_motion_shortens_the_crossfade_to_the_same_ninety_milliseconds() {
        let reduced = MotionSettings { reduce: true, ..MotionSettings::default() };
        let mut fade = Crossfade::start(Duration::from_secs(5), &reduced);
        let mut elapsed = Duration::ZERO;
        while fade.advance(FRAME) {
            elapsed += FRAME;
        }
        assert!(
            elapsed <= REDUCED_FADE + FRAME,
            "a five-second fade survived Reduce Motion, running for {elapsed:?}"
        );
    }

    #[test]
    fn a_resting_crossfade_is_finished() {
        assert!(!Crossfade::done().is_running());
        assert_eq!(Crossfade::done().t(), 1.0);
    }
}
