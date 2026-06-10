//! Render-rate temporal smoothing for the `MediaPipe` provider.
//!
//! The worker runs the ort backend (`CoreML` on macOS, CPU fallback elsewhere)
//! and produces hand poses at a hardware-dependent inference rate. Re-showing
//! each pose for several frames reads as stutter. [`HandSmoother`] eases the
//! exposed pose toward the latest inference result *every render frame* with a
//! One-Euro filter (Casiez et al. 2012) — a low-pass whose cutoff rises with
//! the signal's speed, so it suppresses jitter when the hand is still and keeps
//! lag low when it moves fast. The result is fluid render-rate motion bridged
//! from whatever cadence the backend achieves on the current hardware.
//!
//! Scope: this is **`MediaPipe`-only**, applied inside the provider's `poll`.
//! The Leap provider (~110 fps, already hardware-smoothed) never passes through
//! here, so its latency/feel is unchanged. Filter parameters
//! ([`DEFAULT_MIN_CUTOFF`], [`DEFAULT_BETA`]) are live-tunable from the dev panel
//! via [`crate::settings::HandTrackingSettings`] — no restart.
//!
//! **Object-scale normalization.** Positional channels (palm position, the 21
//! landmarks) are filtered in the Leap-mm space, where a hand close to the
//! camera produces larger per-frame deltas than the same physical motion of a
//! far hand — so a fixed `beta` would over- or under-smooth with distance.
//! Following `MediaPipe`'s `LandmarksSmoothingCalculator`, the speed that drives
//! the adaptive cutoff is divided by the hand's apparent size ([`object_scale`])
//! so smoothing strength is invariant to how close the hand is. Already-
//! normalized channels (the unit palm normal, `[0, 1]` pinch/grab) are filtered
//! without scaling.

use std::f32::consts::TAU;
use std::time::Duration;

use bevy::math::Vec3;
use smallvec::SmallVec;

use crate::input::hand::{Hand, LANDMARK_COUNT};
use crate::input::state::MAX_HANDS;

/// Default minimum cutoff frequency (Hz) — the cutoff when the hand is still, so
/// it sets the at-rest smoothing. Higher = lighter smoothing (more responsive,
/// less lag) at the cost of more jitter passing. `10.0` is deliberately light:
/// once GPU inference and the stabilized tracking ROI cleaned up the pose, heavy
/// output smoothing mostly added lag, so this hardware-validated value favours
/// responsiveness. Live-tunable from the dev panel
/// (`HandTrackingSettings::smoothing_min_cutoff`).
pub const DEFAULT_MIN_CUTOFF: f32 = 10.0;

/// Default speed coefficient: how fast the cutoff opens up with hand speed (less
/// lag during motion). Because the speed is object-scale-normalized (see the
/// module docs), this is expressed in *hand-lengths per second*. Live-tunable
/// from the dev panel (`HandTrackingSettings::smoothing_beta`).
pub const DEFAULT_BETA: f32 = 6.0;

/// Cutoff for the derivative low-pass (Hz) — the One-Euro paper's fixed default.
const DERIVATE_CUTOFF: f32 = 1.0;

/// Floor for [`object_scale`] (Leap mm), so a degenerate/collapsed landmark set
/// never divides the speed by ~0.
const MIN_OBJECT_SCALE: f32 = 1.0;

/// One-Euro smoothing factor for a cutoff frequency and timestep.
///
/// `alpha = 1 / (1 + tau/dt)` with `tau = 1 / (2*pi*cutoff)`.
fn smoothing_alpha(cutoff: f32, dt: f32) -> f32 {
    let tau = 1.0 / (TAU * cutoff);
    1.0 / (1.0 + tau / dt)
}

/// Exponential low-pass: blend `x` toward `prev` by `alpha`.
fn low_pass(x: f32, alpha: f32, prev: f32) -> f32 {
    alpha * x + (1.0 - alpha) * prev
}

/// One-Euro filter for a single scalar channel.
struct OneEuroFilter {
    min_cutoff: f32,
    beta: f32,
    /// Last filtered value; `None` until the first sample.
    x_prev: Option<f32>,
    /// Last filtered derivative.
    dx_prev: f32,
}

impl OneEuroFilter {
    fn new(min_cutoff: f32, beta: f32) -> Self {
        Self {
            min_cutoff,
            beta,
            x_prev: None,
            dx_prev: 0.0,
        }
    }

    /// Filter sample `x` given elapsed `dt` seconds. `value_scale` divides the
    /// speed that drives the adaptive cutoff (`1 / object_scale` for positional
    /// channels, `1.0` for already-normalized ones); the value is always blended
    /// in its original units. The first sample — or any non-positive `dt` —
    /// passes through (no time has elapsed to smooth over).
    fn filter(&mut self, x: f32, dt: f32, value_scale: f32) -> f32 {
        let Some(x_prev) = self.x_prev else {
            self.x_prev = Some(x);
            return x;
        };
        if dt <= 0.0 {
            return x_prev;
        }
        // Smoothed derivative → speed-adaptive cutoff → smoothed value. The
        // speed is scale-normalized (`edx * value_scale`) so the cutoff — and
        // thus the smoothing strength — is invariant to apparent hand size; the
        // blend stays in original units, so the output needs no rescaling.
        let dx = (x - x_prev) / dt;
        let edx = low_pass(dx, smoothing_alpha(DERIVATE_CUTOFF, dt), self.dx_prev);
        self.dx_prev = edx;
        let cutoff = self.min_cutoff + self.beta * (edx * value_scale).abs();
        let x_hat = low_pass(x, smoothing_alpha(cutoff, dt), x_prev);
        self.x_prev = Some(x_hat);
        x_hat
    }

    /// Set the filtered value immediately, clearing derivative momentum.
    fn snap_to(&mut self, x: f32) {
        self.x_prev = Some(x);
        self.dx_prev = 0.0;
    }

    /// Live-update the One-Euro parameters without disturbing the filter state,
    /// so a tuning UI can re-tune a tracked hand mid-motion.
    fn set_params(&mut self, min_cutoff: f32, beta: f32) {
        self.min_cutoff = min_cutoff;
        self.beta = beta;
    }
}

/// Three One-Euro filters, one per component of a [`Vec3`].
struct Vec3Filter {
    c: [OneEuroFilter; 3],
}

impl Vec3Filter {
    fn new(min_cutoff: f32, beta: f32) -> Self {
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

    fn set_params(&mut self, min_cutoff: f32, beta: f32) {
        for c in &mut self.c {
            c.set_params(min_cutoff, beta);
        }
    }
}

/// Apparent hand size (Leap mm) used to make positional smoothing invariant to
/// camera distance — the analogue of `MediaPipe`'s object-scale ROI input: the
/// mean of the landmark bounding box's width and height. Floored at
/// [`MIN_OBJECT_SCALE`] so a collapsed landmark set never divides speed by ~0.
fn object_scale(landmarks: &[Vec3; LANDMARK_COUNT]) -> f32 {
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    for lm in landmarks {
        min = min.min(*lm);
        max = max.max(*lm);
    }
    (((max.x - min.x) + (max.y - min.y)) * 0.5).max(MIN_OBJECT_SCALE)
}

/// The filter bank for a single hand: every positional/scalar field that is
/// smoothed. `id`, `chirality`, and `palm_velocity` pass through unchanged.
struct HandFilters {
    palm_position: Vec3Filter,
    palm_normal: Vec3Filter,
    landmarks: [Vec3Filter; LANDMARK_COUNT],
    pinch: OneEuroFilter,
    grab: OneEuroFilter,
}

impl HandFilters {
    fn new(min_cutoff: f32, beta: f32) -> Self {
        Self {
            palm_position: Vec3Filter::new(min_cutoff, beta),
            palm_normal: Vec3Filter::new(min_cutoff, beta),
            landmarks: std::array::from_fn(|_| Vec3Filter::new(min_cutoff, beta)),
            pinch: OneEuroFilter::new(min_cutoff, beta),
            grab: OneEuroFilter::new(min_cutoff, beta),
        }
    }

    /// Produce the smoothed hand for this frame, easing every filtered field
    /// toward `target` by `dt`.
    fn filter_hand(&mut self, target: &Hand, dt: f32) -> Hand {
        // Positional channels normalize by apparent hand size; already-normalized
        // channels (unit normal, [0,1] pinch/grab) use a unit scale.
        // Known asymmetry: palm_position.z is already EMA-smoothed depth (the
        // tracker's size estimator) and object_scale is xy-based, so z gets an
        // acceptable second light pass here — documented, not a bug.
        let pos_scale = 1.0 / object_scale(&target.landmarks);
        let mut landmarks = target.landmarks;
        for (filter, lm) in self.landmarks.iter_mut().zip(landmarks.iter_mut()) {
            *lm = filter.filter(*lm, dt, pos_scale);
        }
        let grab_strength = if target.grab_strength <= 0.0 {
            // `Pipeline` already applies the configurable rest deadzone. Preserve
            // that exact release through smoothing; otherwise Line's
            // `grab > 0` gate sees an asymptotic tail and keeps the attractor
            // alive after an open/resting hand.
            self.grab.snap_to(0.0);
            0.0
        } else {
            self.grab
                .filter(target.grab_strength, dt, 1.0)
                .clamp(0.0, 1.0)
        };

        Hand {
            palm_position: self
                .palm_position
                .filter(target.palm_position, dt, pos_scale),
            // Smooth the normal's components, then re-normalize to a unit vector.
            palm_normal: self
                .palm_normal
                .filter(target.palm_normal, dt, 1.0)
                .normalize_or_zero(),
            pinch_strength: self
                .pinch
                .filter(target.pinch_strength, dt, 1.0)
                .clamp(0.0, 1.0),
            grab_strength,
            landmarks,
            // id, chirality, palm_velocity, and camera_distance_mm carry
            // through unchanged (the distance is already EMA-smoothed by the
            // tracker; a second One-Euro pass would add nothing but lag).
            ..target.clone()
        }
    }

    /// Live-update the One-Euro parameters of every channel in this bank.
    fn set_params(&mut self, min_cutoff: f32, beta: f32) {
        self.palm_position.set_params(min_cutoff, beta);
        self.palm_normal.set_params(min_cutoff, beta);
        for lm in &mut self.landmarks {
            lm.set_params(min_cutoff, beta);
        }
        self.pinch.set_params(min_cutoff, beta);
        self.grab.set_params(min_cutoff, beta);
    }
}

/// Eases the exposed `MediaPipe` hand pose toward the latest inference result at
/// render rate. One filter bank per hand `id`; banks are created when a hand
/// appears and dropped when it leaves (so a returning hand starts fresh, with no
/// stale momentum).
pub struct HandSmoother {
    min_cutoff: f32,
    beta: f32,
    /// Monotonic time of the previous [`Self::smooth`]; `None` until the first.
    last_now: Option<Duration>,
    /// `(hand id, filter bank)` for each currently-tracked hand.
    banks: SmallVec<[(u32, HandFilters); MAX_HANDS]>,
}

impl HandSmoother {
    /// Construct a smoother with the given One-Euro parameters.
    #[must_use]
    pub fn new(min_cutoff: f32, beta: f32) -> Self {
        Self {
            min_cutoff,
            beta,
            last_now: None,
            banks: SmallVec::new(),
        }
    }

    /// Forget all per-hand state (e.g. on provider restart). The next
    /// [`Self::smooth`] behaves as a cold start (first samples pass through).
    pub fn clear(&mut self) {
        self.last_now = None;
        self.banks.clear();
    }

    /// Live-retune the One-Euro parameters: applies to every currently-tracked
    /// hand's bank *and* becomes the default for banks created later (no state
    /// is reset, so a tracked hand keeps smoothing through the change).
    pub fn set_params(&mut self, min_cutoff: f32, beta: f32) {
        self.min_cutoff = min_cutoff;
        self.beta = beta;
        for (_, bank) in &mut self.banks {
            bank.set_params(min_cutoff, beta);
        }
    }

    /// Advance smoothing to `now`, easing toward the `target` hands, and return
    /// the smoothed hands. `target` is the latest inference result, held
    /// constant between inference frames; calling this every render frame yields
    /// render-rate motion. Banks for hands absent from `target` are dropped; a
    /// hand id seen for the first time passes through (no lag on appearance).
    pub fn smooth(&mut self, target: &[Hand], now: Duration) -> SmallVec<[Hand; MAX_HANDS]> {
        let dt = self
            .last_now
            .map_or(0.0, |prev| now.saturating_sub(prev).as_secs_f32());
        self.last_now = Some(now);

        // Drop banks for hands that are no longer present.
        self.banks
            .retain(|(id, _)| target.iter().any(|h| h.id == *id));

        let mut out: SmallVec<[Hand; MAX_HANDS]> = SmallVec::new();
        for hand in target {
            let bank = self.bank_for(hand.id);
            out.push(bank.filter_hand(hand, dt));
        }
        out
    }

    /// Existing bank for `id`, or a freshly-created one.
    fn bank_for(&mut self, id: u32) -> &mut HandFilters {
        if let Some(i) = self.banks.iter().position(|(bid, _)| *bid == id) {
            &mut self.banks[i].1
        } else {
            let i = self.banks.len();
            self.banks
                .push((id, HandFilters::new(self.min_cutoff, self.beta)));
            &mut self.banks[i].1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::hand::Chirality;

    fn hand_with(id: u32, palm_x: f32) -> Hand {
        Hand {
            id,
            chirality: Chirality::Right,
            palm_position: Vec3::new(palm_x, 0.0, 0.0),
            palm_normal: Vec3::Y,
            palm_velocity: Vec3::ZERO,
            pinch_strength: 0.0,
            grab_strength: 0.0,
            landmarks: [Vec3::new(palm_x, 0.0, 0.0); LANDMARK_COUNT],
            camera_distance_mm: 0.0,
        }
    }

    fn hand_with_grab(id: u32, grab_strength: f32) -> Hand {
        Hand {
            grab_strength,
            ..hand_with(id, 0.0)
        }
    }

    #[test]
    fn first_sample_passes_through() {
        let mut f = OneEuroFilter::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        assert!((f.filter(5.0, 0.016, 1.0) - 5.0).abs() < 1e-6);
    }

    #[test]
    fn zero_dt_holds_previous() {
        let mut f = OneEuroFilter::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        f.filter(3.0, 0.016, 1.0); // establish
        assert!((f.filter(99.0, 0.0, 1.0) - 3.0).abs() < 1e-6);
    }

    #[test]
    fn steps_toward_then_converges_to_a_new_target() {
        let mut f = OneEuroFilter::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        f.filter(0.0, 0.016, 1.0); // baseline 0
        let first = f.filter(10.0, 0.016, 1.0); // first response toward 10
        assert!(first > 0.0 && first < 10.0, "first response {first}");
        let mut last = first;
        for _ in 0..120 {
            last = f.filter(10.0, 0.016, 1.0);
        }
        assert!((last - 10.0).abs() < 0.1, "converged to {last}");
        assert!(last > first, "monotonic toward target");
    }

    #[test]
    fn scale_normalization_changes_smoothing_strength() {
        // Identical absolute motion; a larger value_scale (a smaller apparent
        // hand) raises the adaptive cutoff, so the filtered value tracks closer
        // to the target. This is the distance-invariance mechanism.
        let mut near = OneEuroFilter::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        let mut far = OneEuroFilter::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        near.filter(0.0, 0.016, 0.1); // baselines
        far.filter(0.0, 0.016, 10.0);
        let near_step = near.filter(1.0, 0.016, 0.1); // big apparent hand → heavier smoothing
        let far_step = far.filter(1.0, 0.016, 10.0); // small apparent hand → lighter smoothing
        assert!(
            far_step > near_step,
            "larger value_scale tracks closer: far={far_step} near={near_step}",
        );
    }

    #[test]
    fn object_scale_floors_a_collapsed_hand() {
        let collapsed = [Vec3::ZERO; LANDMARK_COUNT];
        assert!((object_scale(&collapsed) - MIN_OBJECT_SCALE).abs() < 1e-6);
        // A spread hand reports its mean bbox extent.
        let mut lm = [Vec3::ZERO; LANDMARK_COUNT];
        lm[0] = Vec3::new(-10.0, -20.0, 0.0);
        lm[1] = Vec3::new(10.0, 20.0, 0.0); // bbox 20 wide, 40 tall → mean 30
        assert!((object_scale(&lm) - 30.0).abs() < 1e-6);
    }

    #[test]
    fn smoother_first_frame_passes_through() {
        let mut s = HandSmoother::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        let out = s.smooth(&[hand_with(1, 100.0)], Duration::from_millis(0));
        assert_eq!(out.len(), 1);
        // dt is 0 on the first call → exact passthrough, no lag on appearance.
        assert!((out[0].palm_position.x - 100.0).abs() < 1e-6);
    }

    #[test]
    fn smoother_eases_landmarks_toward_a_moved_target() {
        let mut s = HandSmoother::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        // Establish at x=0, then hold a target at x=100 across render frames.
        s.smooth(&[hand_with(1, 0.0)], Duration::from_millis(0));
        let step = s.smooth(&[hand_with(1, 100.0)], Duration::from_millis(16));
        assert!(
            step[0].landmarks[0].x > 0.0 && step[0].landmarks[0].x < 100.0,
            "eased partway: {}",
            step[0].landmarks[0].x
        );
        let mut last = step;
        for i in 2..122u64 {
            last = s.smooth(&[hand_with(1, 100.0)], Duration::from_millis(i * 16));
        }
        assert!(
            (last[0].landmarks[0].x - 100.0).abs() < 1.0,
            "converged to {}",
            last[0].landmarks[0].x
        );
    }

    #[test]
    fn smoother_drops_absent_hand_and_returning_hand_is_fresh() {
        let mut s = HandSmoother::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        s.smooth(&[hand_with(1, 0.0)], Duration::from_millis(0));
        s.smooth(&[hand_with(1, 50.0)], Duration::from_millis(16)); // bank now mid-ease
                                                                    // Hand leaves.
        let gone = s.smooth(&[], Duration::from_millis(32));
        assert!(gone.is_empty());
        // Same id returns: a fresh bank → first sample passes straight through,
        // not eased from the stale mid-value.
        let back = s.smooth(&[hand_with(1, 80.0)], Duration::from_millis(48));
        assert!(
            (back[0].palm_position.x - 80.0).abs() < 1e-6,
            "returning hand should be fresh, got {}",
            back[0].palm_position.x
        );
    }

    #[test]
    fn smoother_preserves_exact_grab_release() {
        let mut s = HandSmoother::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        s.smooth(&[hand_with_grab(1, 1.0)], Duration::from_millis(0));

        let released = s.smooth(&[hand_with_grab(1, 0.0)], Duration::from_millis(16));

        assert!(
            released[0].grab_strength <= f32::EPSILON,
            "a deadzoned open hand must not publish a smoothed positive grab tail"
        );
    }

    #[test]
    fn set_params_retunes_a_live_bank_without_resetting_it() {
        let mut s = HandSmoother::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        s.smooth(&[hand_with(1, 0.0)], Duration::from_millis(0));
        s.smooth(&[hand_with(1, 0.0)], Duration::from_millis(16)); // establish a bank
                                                                   // Crank to near-zero cutoff with no velocity adaptivity → very heavy
                                                                   // smoothing. A big target jump should barely move this frame, and the
                                                                   // bank must NOT be reset (a reset would pass the new target through).
        s.set_params(0.01, 0.0);
        let out = s.smooth(&[hand_with(1, 1000.0)], Duration::from_millis(32));
        assert!(
            out[0].palm_position.x < 100.0,
            "retuned heavy smoothing, not reset: {}",
            out[0].palm_position.x,
        );
    }

    #[test]
    fn clear_resets_to_cold_start() {
        let mut s = HandSmoother::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        s.smooth(&[hand_with(1, 0.0)], Duration::from_millis(0));
        s.smooth(&[hand_with(1, 50.0)], Duration::from_millis(16));
        s.clear();
        // After clear, the next frame is a cold start → passthrough.
        let out = s.smooth(&[hand_with(1, 70.0)], Duration::from_millis(32));
        assert!((out[0].palm_position.x - 70.0).abs() < 1e-6);
    }
}
