//! Engagement scoring: "how likely is this hand a *player's* hand?"
//!
//! Kiosk deployments put the camera beside foot traffic, and the tracker's
//! detect-then-track design means whichever hands grab the two track slots
//! first hold them indefinitely. The observed failure: a bystander's static
//! drink-holding hand locks a slot and the actual player can never get in.
//! The fix ranks hands by an **engagement score** combining, per Madison's
//! directive, how *close* a hand is and how much it is *moving/articulating*:
//!
//! ```text
//! engagement = (1 − w)·proximity + w·activity            (all terms in [0, 1])
//!   proximity = distance falloff over the interaction band (closer ⇒ higher)
//!   activity  = max(motion, articulation)                 (either alone counts)
//!     motion       = |palm velocity| EMA, saturating      (waving hand ⇒ 1)
//!     articulation = (|Δgrab| + |Δpinch|)/s EMA, saturating
//!   w = "motion weight" (live-tunable; how much doing beats being near)
//! ```
//!
//! **Articulation is the drink-holder discriminator**: a static grip has a
//! *high* grab value but *zero* grab change, while a playing hand
//! opens/closes continuously. Scoring the derivative (not the level) is what
//! separates them.
//!
//! Consumed in two places, deliberately through the same formula so on-site
//! tuning of one knob shapes both:
//! - the `MediaPipe` worker pipeline's bystander-eviction logic
//!   (`providers::mediapipe::pipeline`), scoring the tracked slots against a
//!   challenger detection; and
//! - Line's focal-hand pick (`wc_sketches::line::leap_attractors`), scoring
//!   tracked-hand entities from their components at render rate.
//!
//! Everything here is pure `f32` math — no allocation, safe on the worker
//! loop and in per-frame systems.

/// Time constant τ (seconds) for the motion and articulation EMAs.
///
/// ~1 s: long enough that a single frame of landmark jitter cannot spike the
/// activity term, short enough that a player starting to wave reads as
/// engaged within about a second (the admission-latency budget for the
/// bystander-eviction path).
pub const ENGAGEMENT_TAU_S: f32 = 1.0;

/// Palm speed (mm/s, Leap-convention millimetres) at which the motion term
/// saturates to `1`.
///
/// A deliberate wave moves the palm on the order of 300–1000 mm/s; capping at
/// 300 means "clearly waving" already scores full motion and *wild flailing
/// cannot dominate* the score beyond that — the eviction/focal decisions
/// compare engaged-vs-passive, not flail intensity.
pub const MOTION_SATURATION_MM_S: f32 = 300.0;

/// Articulation rate (units of combined grab+pinch strength per second) at
/// which the articulation term saturates to `1`.
///
/// Grab and pinch are each in `[0, 1]`, so `1.0/s` ≈ one full open↔close
/// cycle every two seconds (each cycle traverses ≈ 2.0 of |Δ|). A hand
/// playing Line articulates well above this; a static grip contributes `0`
/// regardless of how strong the grip is.
pub const ARTICULATION_SATURATION_PER_S: f32 = 1.0;

/// Near rail (mm) of the proximity band: at or inside this physical camera
/// distance the proximity term is `1`. Matches the kiosk audio band's
/// standing-distance rail (`LineSettings::synth_full_volume_mm` default).
pub const PROXIMITY_NEAR_MM: f32 = 500.0;

/// Far rail (mm) of the proximity band: at or beyond this distance the
/// proximity term is `0`. Matches the kiosk audio band's silence rail
/// (`LineSettings::synth_silence_mm` default, ≈ 8 ft) — the range beyond
/// which someone is road traffic, not a player.
pub const PROXIMITY_FAR_MM: f32 = 2400.0;

/// Proximity value used when the physical distance is unknown
/// (`camera_distance_mm == 0`, the estimator-off sentinel): neutral, so an
/// unknown-distance hand is ranked purely by its activity rather than being
/// treated as either at-the-lens or out-of-range.
pub const NEUTRAL_PROXIMITY: f32 = 0.5;

/// Default motion weight `w` (see [`engagement`]): activity counts a bit more
/// than proximity, so a hand actively playing at ~1.5 m outranks a static
/// hand near the lens. Live-tunable via
/// `HandTrackingSettings::engagement_motion_weight`.
pub const DEFAULT_MOTION_WEIGHT: f32 = 0.6;

/// Proximity term in `[0, 1]` from a physical camera distance in mm.
///
/// `0` (unknown/estimator off) → [`NEUTRAL_PROXIMITY`]; otherwise a linear
/// falloff `clamp((FAR − d) / (FAR − NEAR), 0, 1)` over the
/// [`PROXIMITY_NEAR_MM`]..[`PROXIMITY_FAR_MM`] interaction band (closer ⇒
/// higher, saturating at the rails). Negative/NaN inputs route to the
/// neutral value like the unknown sentinel.
#[must_use]
pub fn proximity(camera_distance_mm: f32) -> f32 {
    if camera_distance_mm > 0.0 {
        // (FAR − d) / band: 1 at the near rail and closer, 0 at the far rail
        // and beyond. The band is a positive constant, so no divide guard.
        ((PROXIMITY_FAR_MM - camera_distance_mm) / (PROXIMITY_FAR_MM - PROXIMITY_NEAR_MM))
            .clamp(0.0, 1.0)
    } else {
        // `!(x > 0)` catches 0, negatives, and NaN alike.
        NEUTRAL_PROXIMITY
    }
}

/// Activity term in `[0, 1]`: `max(motion, articulation)`, each normalized by
/// its saturation constant.
///
/// `max` (not a sum): waving *or* articulating alone reads as fully active —
/// a player rarely does both at once, and requiring both would under-score a
/// hand that is plainly playing. NaN inputs contribute `0` (the `clamp`
/// then `max` ordering keeps NaN from propagating).
#[must_use]
pub fn activity(motion_mm_s: f32, articulation_per_s: f32) -> f32 {
    // Each term: rate / saturation, clamped to [0, 1]. clamp(NaN) is NaN, so
    // filter non-finite inputs to 0 first (a NaN EMA upstream must degrade to
    // "no activity", never poison the score).
    let motion = if motion_mm_s.is_finite() {
        (motion_mm_s / MOTION_SATURATION_MM_S).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let articulation = if articulation_per_s.is_finite() {
        (articulation_per_s / ARTICULATION_SATURATION_PER_S).clamp(0.0, 1.0)
    } else {
        0.0
    };
    motion.max(articulation)
}

/// Combined engagement score in `[0, 1]`:
/// `(1 − w)·proximity + w·activity`, with `w = motion_weight` clamped to
/// `[0, 1]`.
///
/// `w` is the live-tunable balance between *being near* and *doing
/// something*: `0` ranks purely by distance (pre-fix behaviour plus depth),
/// `1` purely by motion/articulation. Callers pass the outputs of
/// [`proximity`] and [`activity`].
#[must_use]
pub fn engagement(proximity: f32, activity: f32, motion_weight: f32) -> f32 {
    let w = motion_weight.clamp(0.0, 1.0);
    // Convex blend: stays in [0, 1] because both terms are.
    (1.0 - w).mul_add(proximity, w * activity)
}

/// One framerate-independent EMA step: returns the blend factor
/// `alpha = 1 − e^(−dt/τ)` for time constant `tau_s`.
///
/// The exact discrete step of a first-order low-pass: smoothing strength does
/// not depend on the inference/frame rate (`dt = 0 → alpha = 0`, value
/// unchanged). Shared by the worker-side per-track EMAs
/// (`signals::HandTracker`) and Line's render-rate focal-pick EMAs so both
/// lanes converge identically.
#[must_use]
pub fn ema_alpha(dt_s: f32, tau_s: f32) -> f32 {
    if dt_s <= 0.0 || tau_s <= 0.0 {
        0.0
    } else {
        1.0 - (-dt_s / tau_s).exp()
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "deterministic float arithmetic is the test subject"
)]
mod tests {
    use super::*;

    #[test]
    fn proximity_saturates_at_the_rails() {
        assert_eq!(proximity(PROXIMITY_NEAR_MM), 1.0);
        assert_eq!(proximity(100.0), 1.0, "inside the near rail stays full");
        assert_eq!(proximity(PROXIMITY_FAR_MM), 0.0);
        assert_eq!(proximity(5000.0), 0.0, "beyond the far rail stays zero");
        // Midpoint of the band → 0.5.
        let mid = f32::midpoint(PROXIMITY_NEAR_MM, PROXIMITY_FAR_MM);
        assert!((proximity(mid) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn unknown_distance_is_neutral_not_near_or_far() {
        assert_eq!(proximity(0.0), NEUTRAL_PROXIMITY);
        assert_eq!(proximity(-1.0), NEUTRAL_PROXIMITY);
        assert_eq!(proximity(f32::NAN), NEUTRAL_PROXIMITY);
    }

    #[test]
    fn activity_is_max_of_motion_and_articulation() {
        // Waving alone saturates.
        assert_eq!(activity(MOTION_SATURATION_MM_S, 0.0), 1.0);
        // Articulating alone saturates.
        assert_eq!(activity(0.0, ARTICULATION_SATURATION_PER_S), 1.0);
        // Static grip: zero either way.
        assert_eq!(activity(0.0, 0.0), 0.0);
        // Half of each → 0.5 (max, not sum).
        assert_eq!(
            activity(
                MOTION_SATURATION_MM_S * 0.5,
                ARTICULATION_SATURATION_PER_S * 0.5
            ),
            0.5
        );
    }

    #[test]
    fn activity_saturates_so_flailing_cannot_dominate() {
        assert_eq!(activity(10_000.0, 0.0), 1.0);
        assert_eq!(activity(0.0, 50.0), 1.0);
    }

    #[test]
    fn activity_degrades_nan_to_zero() {
        assert_eq!(activity(f32::NAN, 0.0), 0.0);
        assert_eq!(activity(0.0, f32::NAN), 0.0);
    }

    #[test]
    fn engagement_blends_by_motion_weight() {
        // w = 0: pure proximity.
        assert_eq!(engagement(0.8, 0.2, 0.0), 0.8);
        // w = 1: pure activity.
        assert_eq!(engagement(0.8, 0.2, 1.0), 0.2);
        // Default weight favours activity slightly.
        let e = engagement(1.0, 0.0, DEFAULT_MOTION_WEIGHT);
        assert!((e - 0.4).abs() < 1e-6, "static hand at the lens scores {e}");
    }

    #[test]
    fn engagement_clamps_out_of_range_weight() {
        assert_eq!(engagement(1.0, 0.0, -3.0), engagement(1.0, 0.0, 0.0));
        assert_eq!(engagement(1.0, 0.0, 7.0), engagement(1.0, 0.0, 1.0));
    }

    #[test]
    fn drink_holder_loses_to_active_player_at_default_weight() {
        // The deployment scenario: a static gripping hand near the lens
        // (proximity 0.9, zero activity — a static grip has high grab but no
        // grab CHANGE) vs a player waving at ~1.2 m.
        let bystander = engagement(0.9, activity(0.0, 0.0), DEFAULT_MOTION_WEIGHT);
        let player = engagement(
            proximity(1200.0),
            activity(400.0, 0.0),
            DEFAULT_MOTION_WEIGHT,
        );
        assert!(
            player > bystander * 1.4,
            "player ({player}) must decisively beat bystander ({bystander})"
        );
    }

    #[test]
    fn ema_alpha_is_framerate_independent() {
        // dt = τ → 1 − e⁻¹ ≈ 0.632.
        let a = ema_alpha(1.0, 1.0);
        assert!((a - (1.0 - (-1.0f32).exp())).abs() < 1e-6);
        // Degenerate inputs are inert, not NaN.
        assert_eq!(ema_alpha(0.0, 1.0), 0.0);
        assert_eq!(ema_alpha(-1.0, 1.0), 0.0);
        assert_eq!(ema_alpha(0.016, 0.0), 0.0);
    }
}
