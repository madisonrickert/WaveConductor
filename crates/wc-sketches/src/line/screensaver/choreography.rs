//! Pure attract-mode choreography math for the Line sketch (Plan 12, Seam 3).
//!
//! All functions here are deterministic functions of elapsed time and window
//! geometry — no `World`, no resources — so the phantom-hand paths and the
//! invitation-pulse envelope are unit-testable in isolation and reproduce
//! exactly under the fixed-`dt` capture clock.
//!
//! ## Composition (spec §4)
//!
//! ```text
//! RESTING DREAM (base)    1–2 slow wandering attractors; particles drift
//!         │ every ~PULSE_PERIOD_SECS
//!         ▼
//! INVITATION PULSE        two phantom hands fade in above the vessel anchor,
//!   = THE INSTRUCTION      "grab" (power ramps), particles converge, hands
//!                          lift, particles relax back into the dream
//! ```
//!
//! The pulse doubles as the how-to: the two phantom hands perform the literal
//! "hands over the central vessel" gesture a visitor is meant to copy (D5).

use std::f32::consts::TAU;

/// World-space attractor sample: position + raw power, ready to bake into a
/// [`crate::line::particle::Attractor`] (after the caller multiplies power by
/// `gravity_constant`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AttractorSample {
    /// World-space position (centered on origin, +y up).
    pub position: [f32; 2],
    /// Raw attractor power (pre-`gravity_constant`). `0.0` = inactive.
    pub power: f32,
}

/// Full attract-frame snapshot the driver writes to the sim/post params.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AttractFrame {
    /// Two slow "dream" wanderers (base layer). Always low, non-zero power.
    pub dreamers: [AttractorSample; 2],
    /// Two phantom hands (the invitation pulse). Power ramps 0 → peak → 0.
    pub hands: [AttractorSample; 2],
    /// World-space focal point for the gravity smear (`i_mouse`): the vessel
    /// anchor, so the smear glows where the gesture converges.
    pub focal_world: [f32; 2],
    /// Pulse envelope `0..=1` — 0 in the resting dream, 1 at peak grab. The
    /// caption fade and the smear `g_constant` scale with this.
    pub pulse: f32,
}

/// Seconds for one full invitation pulse (dream → grab → release → dream).
pub const PULSE_PERIOD_SECS: f32 = 9.0;

/// Fraction of the pulse period spent actively "grabbing" (the rest is the
/// resting dream between pulses). The grab window is centred in the period.
const GRAB_FRACTION: f32 = 0.45;

/// Dream-wanderer orbit radius as a fraction of the half-height. Modest so the
/// resting motion is a gentle drift.
const DREAM_RADIUS_FRAC: f32 = 0.28;

/// Angular speeds (rad/s) of the two dream wanderers. Different (and
/// opposite-signed) so they never lock into a static pattern.
const DREAM_SPEED_A: f32 = 0.11;
const DREAM_SPEED_B: f32 = -0.067;

/// Constant low power for the dream wanderers (pre-`gravity_constant`). Enough to
/// keep the cloud alive and breathing, far below a real grab.
const DREAM_POWER: f32 = 1.5;

/// Peak phantom-hand power at full grab (pre-`gravity_constant`). Comparable to a
/// real two-hand grab so the convergence reads as the genuine interaction.
const HAND_PEAK_POWER: f32 = 7.0;

/// Horizontal half-separation of the two phantom hands, as a fraction of
/// half-width. The hands sit symmetrically left/right of the vessel.
const HAND_SEPARATION_FRAC: f32 = 0.16;

/// How far above the vessel the hands hover, as a fraction of half-height. The
/// hands descend toward the vessel during the grab and lift away after.
const HAND_LIFT_FRAC: f32 = 0.34;

/// Vessel anchor as a fraction of half-height *below* centre. The vessel sits a
/// little low so the hands come from above it (a head/pie on a plinth).
const VESSEL_DROP_FRAC: f32 = 0.10;

/// Window geometry the choreography needs (half-extents in world units).
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    /// Half the window width in world units.
    pub half_w: f32,
    /// Half the window height in world units.
    pub half_h: f32,
}

impl Bounds {
    /// Build from full window width/height.
    #[must_use]
    pub fn from_size(width: f32, height: f32) -> Self {
        Self {
            half_w: width * 0.5,
            half_h: height * 0.5,
        }
    }
}

/// Smooth 0→1→0 hump over `x in 0..=1`, zero-derivative at both ends
/// (`sin²(πx)`). Ramps the grab power and lift smoothly so the gesture has no
/// hard starts or stops.
#[must_use]
fn smooth_hump(x: f32) -> f32 {
    let s = (std::f32::consts::PI * x).sin();
    s * s
}

/// The pulse envelope at time `t`: 0 during the resting dream, a smooth hump
/// over the grab window (`GRAB_FRACTION` of the period, centred in it).
#[must_use]
pub fn pulse_envelope(t: f32) -> f32 {
    let phase = (t / PULSE_PERIOD_SECS).rem_euclid(1.0);
    let half_grab = GRAB_FRACTION * 0.5;
    let lo = 0.5 - half_grab;
    let hi = 0.5 + half_grab;
    if phase < lo || phase > hi {
        return 0.0;
    }
    let local = (phase - lo) / (hi - lo);
    smooth_hump(local)
}

/// Compute the full attract frame at time `t` for the given bounds.
///
/// Deterministic in `t` — the capture clock pins `t = frame · dt`, so each tier
/// capture samples a reproducible point in the choreography.
#[must_use]
pub fn attract_frame(t: f32, bounds: Bounds) -> AttractFrame {
    let vessel = [0.0, -bounds.half_h * VESSEL_DROP_FRAC];

    // --- Dream wanderers: two slow orbits at slightly different rates. -----
    let r = bounds.half_h * DREAM_RADIUS_FRAC;
    let a_angle = t * DREAM_SPEED_A;
    let b_angle = t * DREAM_SPEED_B + TAU * 0.5; // opposite phase
    let dreamers = [
        AttractorSample {
            position: [r * a_angle.cos(), r * a_angle.sin() * 0.6],
            power: DREAM_POWER,
        },
        AttractorSample {
            position: [
                r * 1.3 * b_angle.cos(),
                r * 0.5 * b_angle.sin() - bounds.half_h * 0.05,
            ],
            power: DREAM_POWER * 0.8,
        },
    ];

    // --- Invitation pulse: two phantom hands above the vessel. -------------
    let pulse = pulse_envelope(t);
    let sep = bounds.half_w * HAND_SEPARATION_FRAC;
    // Hands start high and descend to just above the vessel at peak grab, then
    // lift back. The vertical offset above the vessel shrinks as pulse rises.
    let lift = bounds.half_h * HAND_LIFT_FRAC;
    let hand_y = vessel[1] + lift * (1.0 - pulse) + lift * 0.25;
    let hand_power = HAND_PEAK_POWER * pulse;
    let hands = [
        AttractorSample {
            position: [vessel[0] - sep, hand_y],
            power: hand_power,
        },
        AttractorSample {
            position: [vessel[0] + sep, hand_y],
            power: hand_power,
        },
    ];

    AttractFrame {
        dreamers,
        hands,
        focal_world: vessel,
        pulse,
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "choreography endpoints are exact; equality is the intended check"
)]
mod tests {
    use super::*;

    fn bounds() -> Bounds {
        Bounds::from_size(1280.0, 720.0)
    }

    #[test]
    fn smooth_hump_endpoints_are_zero() {
        assert_eq!(smooth_hump(0.0), 0.0);
        assert!(smooth_hump(1.0).abs() < 1e-6);
        assert!((smooth_hump(0.5) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn pulse_zero_in_resting_dream() {
        assert_eq!(pulse_envelope(0.0), 0.0);
        assert_eq!(pulse_envelope(PULSE_PERIOD_SECS * 0.1), 0.0);
    }

    #[test]
    fn pulse_peaks_mid_period() {
        let peak = pulse_envelope(PULSE_PERIOD_SECS * 0.5);
        assert!(
            (peak - 1.0).abs() < 1e-4,
            "mid-period pulse should peak ~1, got {peak}"
        );
    }

    #[test]
    fn pulse_is_periodic() {
        let a = pulse_envelope(PULSE_PERIOD_SECS * 0.5);
        let b = pulse_envelope(PULSE_PERIOD_SECS * 1.5);
        assert!((a - b).abs() < 1e-5, "pulse must repeat each period");
    }

    #[test]
    fn hands_zero_power_in_dream_high_at_grab() {
        let dream = attract_frame(0.0, bounds());
        assert_eq!(dream.hands[0].power, 0.0);
        assert_eq!(dream.hands[1].power, 0.0);
        let grab = attract_frame(PULSE_PERIOD_SECS * 0.5, bounds());
        assert!(grab.hands[0].power > 0.0);
        assert!(grab.hands[1].power > 0.0);
    }

    #[test]
    fn hands_straddle_vessel_symmetrically() {
        let f = attract_frame(PULSE_PERIOD_SECS * 0.5, bounds());
        assert!(f.hands[0].position[0] < f.focal_world[0]);
        assert!(f.hands[1].position[0] > f.focal_world[0]);
        let left_off = f.focal_world[0] - f.hands[0].position[0];
        let right_off = f.hands[1].position[0] - f.focal_world[0];
        assert!((left_off - right_off).abs() < 1e-4, "hands must be symmetric");
    }

    #[test]
    fn dreamers_always_low_nonzero_power() {
        for i in 0..20 {
            #[allow(
                clippy::cast_precision_loss,
                clippy::as_conversions,
                reason = "test loop counter"
            )]
            let t = i as f32 * 0.5;
            let f = attract_frame(t, bounds());
            assert!(f.dreamers[0].power > 0.0);
            assert!(f.dreamers[1].power > 0.0);
            assert!(f.dreamers[0].power < HAND_PEAK_POWER);
        }
    }

    #[test]
    fn hands_descend_toward_vessel_at_peak() {
        let dream = attract_frame(PULSE_PERIOD_SECS * 0.1, bounds());
        let grab = attract_frame(PULSE_PERIOD_SECS * 0.5, bounds());
        assert!(
            grab.hands[0].position[1] < dream.hands[0].position[1],
            "hands should descend toward the vessel during the grab"
        );
    }
}
