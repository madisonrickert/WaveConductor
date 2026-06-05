//! Render-rate temporal smoothing for the `MediaPipe` provider.
//!
//! The worker produces hand poses at the inference rate (~15–20 fps), but the
//! app renders at ~60 fps. Re-showing each pose for several frames reads as
//! stutter. [`HandSmoother`] eases the exposed pose toward the latest inference
//! result *every render frame* with a One-Euro filter (Casiez et al. 2012) — a
//! low-pass whose cutoff rises with the signal's speed, so it suppresses jitter
//! when the hand is still and keeps lag low when it moves fast. The result is
//! fluid 60 fps motion derived from a 15–20 fps source.
//!
//! Scope: this is **`MediaPipe`-only**, applied inside the provider's `poll`.
//! The Leap provider (~110 fps, already hardware-smoothed) never passes through
//! here, so its latency/feel is unchanged. Filter parameters
//! ([`DEFAULT_MIN_CUTOFF`], [`DEFAULT_BETA`]) are starting points to tune during
//! hardware acceptance.

use std::f32::consts::TAU;
use std::time::Duration;

use bevy::math::Vec3;
use smallvec::SmallVec;

use crate::input::hand::{Hand, LANDMARK_COUNT};
use crate::input::state::MAX_HANDS;

/// Default minimum cutoff frequency (Hz). Lower = smoother but laggier when the
/// hand moves slowly. Tunable during hardware acceptance.
pub const DEFAULT_MIN_CUTOFF: f32 = 2.5;

/// Default speed coefficient. Higher = less lag during fast motion (the filter
/// cutoff rises with hand speed). Tunable during hardware acceptance.
pub const DEFAULT_BETA: f32 = 0.02;

/// Cutoff for the derivative low-pass (Hz) — the One-Euro paper's fixed default.
const DERIVATE_CUTOFF: f32 = 1.0;

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

    /// Filter sample `x` given elapsed `dt` seconds. The first sample — or any
    /// non-positive `dt` — passes through (no time has elapsed to smooth over).
    fn filter(&mut self, x: f32, dt: f32) -> f32 {
        let Some(x_prev) = self.x_prev else {
            self.x_prev = Some(x);
            return x;
        };
        if dt <= 0.0 {
            return x_prev;
        }
        // Smoothed derivative → speed-adaptive cutoff → smoothed value.
        let dx = (x - x_prev) / dt;
        let edx = low_pass(dx, smoothing_alpha(DERIVATE_CUTOFF, dt), self.dx_prev);
        self.dx_prev = edx;
        let cutoff = self.min_cutoff + self.beta * edx.abs();
        let x_hat = low_pass(x, smoothing_alpha(cutoff, dt), x_prev);
        self.x_prev = Some(x_hat);
        x_hat
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

    fn filter(&mut self, v: Vec3, dt: f32) -> Vec3 {
        Vec3::new(
            self.c[0].filter(v.x, dt),
            self.c[1].filter(v.y, dt),
            self.c[2].filter(v.z, dt),
        )
    }
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
        let mut landmarks = target.landmarks;
        for (filter, lm) in self.landmarks.iter_mut().zip(landmarks.iter_mut()) {
            *lm = filter.filter(*lm, dt);
        }
        Hand {
            palm_position: self.palm_position.filter(target.palm_position, dt),
            // Smooth the normal's components, then re-normalize to a unit vector.
            palm_normal: self
                .palm_normal
                .filter(target.palm_normal, dt)
                .normalize_or_zero(),
            pinch_strength: self.pinch.filter(target.pinch_strength, dt).clamp(0.0, 1.0),
            grab_strength: self.grab.filter(target.grab_strength, dt).clamp(0.0, 1.0),
            landmarks,
            // id, chirality, and palm_velocity carry through unchanged.
            ..target.clone()
        }
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
        }
    }

    #[test]
    fn first_sample_passes_through() {
        let mut f = OneEuroFilter::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        assert!((f.filter(5.0, 0.016) - 5.0).abs() < 1e-6);
    }

    #[test]
    fn zero_dt_holds_previous() {
        let mut f = OneEuroFilter::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        f.filter(3.0, 0.016); // establish
        assert!((f.filter(99.0, 0.0) - 3.0).abs() < 1e-6);
    }

    #[test]
    fn steps_toward_then_converges_to_a_new_target() {
        let mut f = OneEuroFilter::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        f.filter(0.0, 0.016); // baseline 0
        let first = f.filter(10.0, 0.016); // first response toward 10
        assert!(first > 0.0 && first < 10.0, "first response {first}");
        let mut last = first;
        for _ in 0..120 {
            last = f.filter(10.0, 0.016);
        }
        assert!((last - 10.0).abs() < 0.1, "converged to {last}");
        assert!(last > first, "monotonic toward target");
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
