//! Poll-rate One-Euro smoothing for body landmarks (Casiez et al. 2012),
//! adapting the hand provider's pattern: the worker produces poses at the
//! inference cadence; the main thread eases the exposed pose toward the
//! latest result every frame so motion reads as fluid, with a speed-adaptive
//! cutoff normalized by the body's apparent size (distance-invariant
//! smoothing strength, following `MediaPipe`'s `LandmarksSmoothingCalculator`).
//!
//! Per-landmark visibility is separately low-passed (`MediaPipe`'s
//! `VisibilitySmoothingCalculator`, alpha 0.1) so a joint does not flicker in
//! and out of "visible" between frames, and the metric world landmarks use a
//! filter bank tuned apart from the screen bank (`WORLD_MIN_CUTOFF`/`WORLD_BETA`,
//! `value_scale = 1.0`), matching upstream's `pose_landmark_filtering` world
//! options.
//!
//! Velocities: the pinned `BodyTrackingState.velocities` are the finite
//! differences of the *smoothed* screen positions, additionally EMA'd
//! (`VELOCITY_EMA_ALPHA`) so Plan C's limb impulses don't flutter with
//! residual landmark noise.
//!
//! Filter banks are fixed arrays sized [`BODY_LANDMARK_COUNT`]; `clear()`
//! resets filter state in place — no allocation after construction.

use std::f32::consts::TAU;
use std::time::Duration;

use bevy::math::Vec3;

use super::{BodyLandmark, BODY_LANDMARK_COUNT};

/// Default minimum cutoff (Hz) — the at-rest smoothing strength. `MediaPipe`'s
/// pose-landmark filtering default (`one_euro_filter { min_cutoff: 0.05 }`),
/// which is deliberately heavy: a still dancer must read as still. Live
/// tuning lands in Plan C's dev panel via [`BodySmoother::set_params`].
pub const DEFAULT_MIN_CUTOFF: f32 = 0.05;

/// Default speed coefficient (cutoff growth per body-scale/sec of speed) —
/// `MediaPipe`'s pose default (`beta: 80`), so fast limbs cut through the
/// heavy at-rest smoothing with little lag.
pub const DEFAULT_BETA: f32 = 80.0;

/// Cutoff for the derivative low-pass (Hz) — the One-Euro paper's default.
const DERIVATE_CUTOFF: f32 = 1.0;

/// World-landmark One-Euro min cutoff (Hz). Upstream filters the *world*
/// (metric, hip-centred metres) landmarks with a **separate** tuning from the
/// screen landmarks — `pose_landmark_filtering.pbtxt` world bank:
/// `one_euro_filter { min_cutoff: 0.1 beta: 40.0 derivate_cutoff: 1.0
/// disable_value_scaling: true }`. They differ from the screen params
/// (0.05/80) on purpose: world coordinates are in metres, not image-normalized
/// pixels, so their speed is *not* divided by the apparent object scale
/// (`value_scale` stays 1.0), and both the at-rest cutoff and the speed
/// coefficient are tuned for that metric unit rather than reused from the
/// screen bank (reusing 0.05/80 would give world coords half the rest cutoff
/// and double the speed response upstream intends).
pub const WORLD_MIN_CUTOFF: f32 = 0.1;

/// World-landmark One-Euro speed coefficient (`beta: 40.0`; see
/// [`WORLD_MIN_CUTOFF`] for why the world bank is tuned apart from the screen
/// bank).
pub const WORLD_BETA: f32 = 40.0;

/// Low-pass alpha for per-landmark visibility. `MediaPipe` low-passes
/// visibility with `VisibilitySmoothingCalculator { low_pass_filter
/// { alpha: 0.1 } }` (`pose_landmark_filtering.pbtxt`, applied to the
/// normalized, world, and aux banks) *before* the coordinate smoothing.
/// Visibility is a probability, so it gets a plain exponential low-pass, not a
/// One-Euro filter: this stops a landmark flickering in and out of "visible"
/// between frames so a downstream visibility gate does not chatter.
const VISIBILITY_LOW_PASS_ALPHA: f32 = 0.1;

/// Floor for the apparent body size (normalized units), so a degenerate
/// collapsed landmark set never divides the speed by ~0.
const MIN_BODY_SCALE: f32 = 0.05;

/// EMA factor for the published velocities (fraction of the new finite
/// difference blended in per frame).
const VELOCITY_EMA_ALPHA: f32 = 0.5;

/// One-Euro smoothing factor for a cutoff frequency and timestep.
fn smoothing_alpha(cutoff: f32, dt: f32) -> f32 {
    let tau = 1.0 / (TAU * cutoff);
    1.0 / (1.0 + tau / dt)
}

/// Exponential low-pass: blend `x` toward `prev` by `alpha`.
fn low_pass(x: f32, alpha: f32, prev: f32) -> f32 {
    alpha * x + (1.0 - alpha) * prev
}

/// One-Euro filter for a single scalar channel. Exposed to the rest of the
/// body module (`pub(super)`) so the worker-side aux-ROI filter in `pipeline`
/// can reuse the exact same math instead of duplicating it.
pub(super) struct OneEuroFilter {
    min_cutoff: f32,
    beta: f32,
    /// Last filtered value; `None` until the first sample.
    x_prev: Option<f32>,
    /// Last filtered derivative.
    dx_prev: f32,
}

impl OneEuroFilter {
    pub(super) const fn new(min_cutoff: f32, beta: f32) -> Self {
        Self {
            min_cutoff,
            beta,
            x_prev: None,
            dx_prev: 0.0,
        }
    }

    /// Filter sample `x` over `dt` seconds; `value_scale` divides the speed
    /// driving the adaptive cutoff. First sample (or non-positive `dt`)
    /// passes through / holds.
    pub(super) fn filter(&mut self, x: f32, dt: f32, value_scale: f32) -> f32 {
        let Some(x_prev) = self.x_prev else {
            self.x_prev = Some(x);
            return x;
        };
        if dt <= 0.0 {
            return x_prev;
        }
        let dx = (x - x_prev) / dt;
        let edx = low_pass(dx, smoothing_alpha(DERIVATE_CUTOFF, dt), self.dx_prev);
        self.dx_prev = edx;
        let cutoff = self.min_cutoff + self.beta * (edx * value_scale).abs();
        let x_hat = low_pass(x, smoothing_alpha(cutoff, dt), x_prev);
        self.x_prev = Some(x_hat);
        x_hat
    }

    /// Forget history (cold start) without touching parameters.
    pub(super) fn reset(&mut self) {
        self.x_prev = None;
        self.dx_prev = 0.0;
    }

    /// Retune without disturbing filter state.
    fn set_params(&mut self, min_cutoff: f32, beta: f32) {
        self.min_cutoff = min_cutoff;
        self.beta = beta;
    }
}

/// Three One-Euro filters, one per [`Vec3`] component.
struct Vec3Filter {
    c: [OneEuroFilter; 3],
}

impl Vec3Filter {
    const fn new(min_cutoff: f32, beta: f32) -> Self {
        Self {
            c: [
                OneEuroFilter::new(min_cutoff, beta),
                OneEuroFilter::new(min_cutoff, beta),
                OneEuroFilter::new(min_cutoff, beta),
            ],
        }
    }

    fn filter(&mut self, v: Vec3, dt: f32, value_scale: f32) -> Vec3 {
        Vec3::new(
            self.c[0].filter(v.x, dt, value_scale),
            self.c[1].filter(v.y, dt, value_scale),
            self.c[2].filter(v.z, dt, value_scale),
        )
    }

    fn reset(&mut self) {
        for c in &mut self.c {
            c.reset();
        }
    }

    fn set_params(&mut self, min_cutoff: f32, beta: f32) {
        for c in &mut self.c {
            c.set_params(min_cutoff, beta);
        }
    }
}

/// Apparent body size (normalized units): mean of the landmark bounding
/// box's width and height, floored at [`MIN_BODY_SCALE`]. Divides the speed
/// so smoothing strength is invariant to how close the dancer stands.
fn body_scale(landmarks: &[BodyLandmark; BODY_LANDMARK_COUNT]) -> f32 {
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    for lm in landmarks {
        min = min.min(lm.pos);
        max = max.max(lm.pos);
    }
    (((max.x - min.x) + (max.y - min.y)) * 0.5).max(MIN_BODY_SCALE)
}

/// One frame of smoothed output.
pub struct SmoothedBody {
    /// Smoothed content-norm landmarks (positions One-Euro filtered,
    /// visibility low-passed).
    pub landmarks: [BodyLandmark; BODY_LANDMARK_COUNT],
    /// Smoothed metric world landmarks.
    pub world: [Vec3; BODY_LANDMARK_COUNT],
    /// EMA'd velocities of the smoothed screen positions (units/sec).
    pub velocities: [Vec3; BODY_LANDMARK_COUNT],
}

/// Eases the exposed body pose toward the latest inference result at poll
/// rate. One filter bank per landmark; [`Self::clear`] on person-loss so a
/// returning person starts fresh (no stale momentum).
pub struct BodySmoother {
    min_cutoff: f32,
    beta: f32,
    /// Monotonic time of the previous smooth; `None` until the first.
    last_now: Option<Duration>,
    pos: [Vec3Filter; BODY_LANDMARK_COUNT],
    /// World-landmark filter bank. Tuned apart from `pos` with the fixed
    /// upstream world params ([`WORLD_MIN_CUTOFF`]/[`WORLD_BETA`]) and driven
    /// with `value_scale = 1.0` (no object-scale normalization, matching
    /// `disable_value_scaling: true`); [`Self::set_params`] deliberately does
    /// not retune it.
    world: [Vec3Filter; BODY_LANDMARK_COUNT],
    /// Per-landmark low-passed visibility (`VISIBILITY_LOW_PASS_ALPHA`); see
    /// [`Self::smooth`].
    vis: [f32; BODY_LANDMARK_COUNT],
    /// Whether `vis` holds real history (first frame passes visibility through).
    has_vis: bool,
    /// Previous smoothed positions (velocity finite differences).
    prev_pos: [Vec3; BODY_LANDMARK_COUNT],
    /// Whether `prev_pos` holds real history.
    has_prev: bool,
    /// EMA'd velocities.
    vel: [Vec3; BODY_LANDMARK_COUNT],
}

impl BodySmoother {
    /// Construct a smoother with the given One-Euro parameters.
    #[must_use]
    pub fn new(min_cutoff: f32, beta: f32) -> Self {
        Self {
            min_cutoff,
            beta,
            last_now: None,
            pos: std::array::from_fn(|_| Vec3Filter::new(min_cutoff, beta)),
            world: std::array::from_fn(|_| Vec3Filter::new(WORLD_MIN_CUTOFF, WORLD_BETA)),
            vis: [0.0; BODY_LANDMARK_COUNT],
            has_vis: false,
            prev_pos: [Vec3::ZERO; BODY_LANDMARK_COUNT],
            has_prev: false,
            vel: [Vec3::ZERO; BODY_LANDMARK_COUNT],
        }
    }

    /// Forget all state (person left / worker restart). The next
    /// [`Self::smooth`] is a cold start: passthrough, zero velocity. Resets
    /// in place — no allocation.
    pub fn clear(&mut self) {
        self.last_now = None;
        self.has_prev = false;
        self.has_vis = false;
        self.vel = [Vec3::ZERO; BODY_LANDMARK_COUNT];
        for f in &mut self.pos {
            f.reset();
        }
        for f in &mut self.world {
            f.reset();
        }
    }

    /// Current One-Euro parameters, `(min_cutoff, beta)`. Test-only
    /// introspection to verify the worker-start plumbing (Plan C Task 14)
    /// reaches this smoother; production code only ever sets params
    /// (construction / [`Self::set_params`]) and never needs to read them
    /// back.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn params(&self) -> (f32, f32) {
        (self.min_cutoff, self.beta)
    }

    /// Live-retune the **screen** landmark filter without resetting filter
    /// state. The world bank keeps its fixed upstream params
    /// ([`WORLD_MIN_CUTOFF`]/[`WORLD_BETA`]) — the Dev knob these arguments come
    /// from tunes screen-space smoothing, and world coordinates are a different
    /// (metric) unit whose tuning must not track it.
    pub fn set_params(&mut self, min_cutoff: f32, beta: f32) {
        self.min_cutoff = min_cutoff;
        self.beta = beta;
        for f in &mut self.pos {
            f.set_params(min_cutoff, beta);
        }
    }

    /// Advance smoothing to `now`, easing toward the target arrays (the
    /// latest worker result, held constant between inference frames), and
    /// return the smoothed pose + velocities.
    pub fn smooth(
        &mut self,
        target: &[BodyLandmark; BODY_LANDMARK_COUNT],
        target_world: &[Vec3; BODY_LANDMARK_COUNT],
        now: Duration,
    ) -> SmoothedBody {
        let dt = self
            .last_now
            .map_or(0.0, |prev| now.saturating_sub(prev).as_secs_f32());
        self.last_now = Some(now);
        // Screen positions normalize speed by apparent body size; metric
        // world positions use unit scale.
        let pos_scale = 1.0 / body_scale(target);

        let mut out = SmoothedBody {
            landmarks: *target,
            world: *target_world,
            velocities: [Vec3::ZERO; BODY_LANDMARK_COUNT],
        };
        for i in 0..BODY_LANDMARK_COUNT {
            out.landmarks[i].pos = self.pos[i].filter(target[i].pos, dt, pos_scale);
            out.world[i] = self.world[i].filter(target_world[i], dt, 1.0);
            // Visibility low-pass (alpha 0.1); first frame passes through so a
            // fresh track has no fade-in from the zero state.
            let new_vis = target[i].visibility;
            out.landmarks[i].visibility = if self.has_vis {
                low_pass(new_vis, VISIBILITY_LOW_PASS_ALPHA, self.vis[i])
            } else {
                new_vis
            };
            self.vis[i] = out.landmarks[i].visibility;
            // Velocity: finite-difference the SMOOTHED position, then EMA.
            let v_raw = if self.has_prev && dt > 0.0 {
                (out.landmarks[i].pos - self.prev_pos[i]) / dt
            } else {
                Vec3::ZERO
            };
            self.vel[i] += (v_raw - self.vel[i]) * VELOCITY_EMA_ALPHA;
            out.velocities[i] = self.vel[i];
            self.prev_pos[i] = out.landmarks[i].pos;
        }
        self.has_prev = true;
        self.has_vis = true;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_at(
        x: f32,
    ) -> (
        [BodyLandmark; BODY_LANDMARK_COUNT],
        [Vec3; BODY_LANDMARK_COUNT],
    ) {
        let mut lms = [BodyLandmark::default(); BODY_LANDMARK_COUNT];
        for (i, lm) in lms.iter_mut().enumerate() {
            // A spread body so object scale is well-defined.
            lm.pos = Vec3::new(x + f32_i(i) * 0.001, 0.2 + f32_i(i) * 0.015, 0.0);
            lm.visibility = 0.9;
        }
        let world = [Vec3::new(x, 0.0, 0.0); BODY_LANDMARK_COUNT];
        (lms, world)
    }

    fn f32_i(i: usize) -> f32 {
        u16::try_from(i).map_or(0.0, f32::from)
    }

    #[test]
    fn first_frame_passes_through_without_lag() {
        let mut s = BodySmoother::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        let (lms, world) = body_at(0.5);
        let out = s.smooth(&lms, &world, Duration::from_millis(0));
        assert!((out.landmarks[0].pos.x - 0.5).abs() < 1e-6);
        assert!((out.world[0].x - 0.5).abs() < 1e-6);
        assert_eq!(out.velocities[0], Vec3::ZERO, "no history → zero velocity");
        assert!(
            (out.landmarks[0].visibility - 0.9).abs() < 1e-6,
            "visibility passes through"
        );
    }

    #[test]
    fn eases_toward_a_moved_target_then_converges() {
        let mut s = BodySmoother::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        let (a, wa) = body_at(0.0);
        let (b, wb) = body_at(0.5);
        s.smooth(&a, &wa, Duration::from_millis(0));
        let step = s.smooth(&b, &wb, Duration::from_millis(16));
        assert!(
            step.landmarks[0].pos.x > 0.0 && step.landmarks[0].pos.x < 0.5,
            "eased partway: {}",
            step.landmarks[0].pos.x
        );
        let mut last = step;
        for i in 2..240_u64 {
            last = s.smooth(&b, &wb, Duration::from_millis(i * 16));
        }
        assert!(
            (last.landmarks[0].pos.x - 0.5).abs() < 0.01,
            "converged: {}",
            last.landmarks[0].pos.x
        );
    }

    #[test]
    fn velocity_tracks_motion_and_settles_to_zero() {
        let mut s = BodySmoother::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        let (a, wa) = body_at(0.0);
        s.smooth(&a, &wa, Duration::from_millis(0));
        // Target jumps and holds: velocity spikes positive, then decays as
        // the smoothed position converges.
        let (b, wb) = body_at(0.4);
        let moving = s.smooth(&b, &wb, Duration::from_millis(16));
        assert!(
            moving.velocities[0].x > 0.0,
            "moving toward +x: {:?}",
            moving.velocities[0]
        );
        let mut settled = moving;
        for i in 2..300_u64 {
            settled = s.smooth(&b, &wb, Duration::from_millis(i * 16));
        }
        assert!(
            settled.velocities[0].length() < 0.05,
            "settled velocity ~0: {:?}",
            settled.velocities[0]
        );
    }

    #[test]
    fn clear_resets_to_cold_start() {
        let mut s = BodySmoother::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        let (a, wa) = body_at(0.0);
        let (b, wb) = body_at(0.7);
        s.smooth(&a, &wa, Duration::from_millis(0));
        s.smooth(&b, &wb, Duration::from_millis(16));
        s.clear();
        // Cold start again: passthrough, zero velocity — a returning person
        // carries no stale momentum.
        let back = s.smooth(&b, &wb, Duration::from_millis(160));
        assert!((back.landmarks[0].pos.x - 0.7).abs() < 1e-5);
        assert_eq!(back.velocities[0], Vec3::ZERO);
    }

    #[test]
    fn set_params_retunes_without_resetting_state() {
        let mut s = BodySmoother::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        let (a, wa) = body_at(0.0);
        s.smooth(&a, &wa, Duration::from_millis(0));
        s.smooth(&a, &wa, Duration::from_millis(16));
        // Near-zero cutoff, no adaptivity → very heavy smoothing; a big jump
        // barely moves. A reset would instead pass the target through.
        s.set_params(0.001, 0.0);
        let (b, wb) = body_at(1.0);
        let out = s.smooth(&b, &wb, Duration::from_millis(32));
        assert!(
            out.landmarks[0].pos.x < 0.1,
            "retuned heavy smoothing, not reset: {}",
            out.landmarks[0].pos.x
        );
    }

    #[test]
    fn visibility_low_passes_toward_a_stepped_target() {
        let mut s = BodySmoother::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        let (mut a, wa) = body_at(0.5); // visibility 0.9
                                        // First frame passes visibility straight through (no fade-in).
        let f0 = s.smooth(&a, &wa, Duration::from_millis(0));
        assert!((f0.landmarks[0].visibility - 0.9).abs() < 1e-6);
        // Step visibility to 0 and hold. Low-pass (alpha 0.1) must EASE, not
        // snap: one step = 0.1·0.0 + 0.9·0.9 = 0.81.
        for lm in &mut a {
            lm.visibility = 0.0;
        }
        let f1 = s.smooth(&a, &wa, Duration::from_millis(16));
        assert!(
            (f1.landmarks[0].visibility - 0.81).abs() < 1e-5,
            "vis={}",
            f1.landmarks[0].visibility
        );
        // Converges toward 0 over many frames.
        let mut last = f1;
        for i in 2..80_u64 {
            last = s.smooth(&a, &wa, Duration::from_millis(i * 16));
        }
        assert!(
            last.landmarks[0].visibility < 0.02,
            "settled vis={}",
            last.landmarks[0].visibility
        );
    }

    #[test]
    fn clear_resets_visibility_history() {
        let mut s = BodySmoother::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        let (mut a, wa) = body_at(0.5);
        s.smooth(&a, &wa, Duration::from_millis(0)); // seed vis 0.9
        for lm in &mut a {
            lm.visibility = 0.2;
        }
        s.smooth(&a, &wa, Duration::from_millis(16)); // mid-decay (~0.83)
        s.clear();
        // Cold start: the new visibility passes straight through, not eased
        // from the pre-clear history.
        let back = s.smooth(&a, &wa, Duration::from_millis(160));
        assert!((back.landmarks[0].visibility - 0.2).abs() < 1e-6);
    }

    #[test]
    fn world_filter_keeps_its_own_params_when_screen_is_retuned() {
        let mut s = BodySmoother::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        let (a, wa) = body_at(0.0);
        s.smooth(&a, &wa, Duration::from_millis(0));
        // Freeze the SCREEN filter (near-zero cutoff, no adaptivity).
        s.set_params(0.0001, 0.0);
        let (b, wb) = body_at(1.0); // screen x and world x both jump to 1.0
        let out = s.smooth(&b, &wb, Duration::from_millis(16));
        // Screen barely moves (retuned heavy)…
        assert!(
            out.landmarks[0].pos.x < 0.05,
            "screen x={}",
            out.landmarks[0].pos.x
        );
        // …but the world bank kept WORLD_MIN_CUTOFF/WORLD_BETA (set_params does
        // not touch it), so it eases visibly toward the target.
        assert!(out.world[0].x > 0.05, "world x={}", out.world[0].x);
    }
}
