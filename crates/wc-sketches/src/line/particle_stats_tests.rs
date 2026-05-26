//! Unit tests for [`super::update_particle_stats`].
//!
//! Lives in a sibling file (linked from `particle_stats.rs` via
//! `#[path = ...] mod tests;`) so the production module stays under the
//! AGENTS.md ~300-line guideline. The tests still see `super::*`, which
//! resolves to the `particle_stats` module — `#[path]` only redirects the
//! source file, not the logical module path.
//!
//! These tests cover the envelope-approximation behavior introduced in Plan 11
//! Phase F: that `grouped_upness` rises on press and decays on release, that
//! `variance_length` drops on press and recovers after release, that
//! `flat_ratio` varies between its baseline and peak with excitement, and that
//! `normalized_entropy` is clamped above its floor. The tests call the
//! `step_envelope` pure function directly so the math stays in one place
//! (production wraps it through the Bevy system in `update_particle_stats`).

#![allow(
    clippy::float_cmp,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "exact-value and unwrap patterns are appropriate in test code"
)]

use super::{
    step_envelope, ParticleStats, ENTROPY_FLOOR, FLAT_RATIO_BASELINE, FLAT_RATIO_MIN,
    GROUPED_UPNESS_PEAK, SPREAD_BASELINE, SPREAD_MIN,
};
use crate::line::systems::mouse::MOUSE_POWER_PRESS;

/// Compile-time invariant: `ENTROPY_FLOOR` must be ≤ `SPREAD_MIN`, otherwise
/// `normalized_entropy` would clamp above `variance_length` and decouple
/// from the noise-filter envelope. Lives at module scope (not inside a test
/// body) to satisfy `clippy::items_after_statements`.
const _: () = assert!(
    ENTROPY_FLOOR <= SPREAD_MIN,
    "ENTROPY_FLOOR must be ≤ SPREAD_MIN so the floor is inactive in steady state"
);

/// Advance `ParticleStats` through `n` frames of `dt` seconds each, with
/// the given `mouse_power`. Returns the final stats. Calls the production
/// `step_envelope` directly so the test sees the same envelope math the
/// `update_particle_stats` system applies in production.
fn run_frames(n: u32, dt: f32, mouse_power: f32, initial: ParticleStats) -> ParticleStats {
    let mut stats = initial;
    for _ in 0..n {
        step_envelope(&mut stats, mouse_power, dt);
    }
    stats
}

/// At rest (no attractor activity), `grouped_upness` and `average_vel` should
/// stay near zero when starting from the default (zero) state.
#[test]
fn at_rest_grouped_upness_stays_near_zero() {
    let result = run_frames(60, 1.0 / 60.0, 0.0, ParticleStats::default());
    assert!(
        result.grouped_upness < 0.01,
        "expected grouped_upness ≈ 0 at rest; got {}",
        result.grouped_upness,
    );
    assert!(
        result.average_vel < 0.01,
        "expected average_vel ≈ 0 at rest; got {}",
        result.average_vel,
    );
}

/// On sustained press (excitement = 1.0), `grouped_upness` must rise above 0.3
/// within 60 frames (~1 second at 60 Hz).
#[test]
fn sustained_press_raises_grouped_upness() {
    let result = run_frames(60, 1.0 / 60.0, MOUSE_POWER_PRESS, ParticleStats::default());
    assert!(
        result.grouped_upness > 0.3,
        "expected grouped_upness > 0.3 after 1s press; got {}",
        result.grouped_upness,
    );
}

/// `grouped_upness` peaks at `GROUPED_UPNESS_PEAK` (after a long sustained press),
/// never exceeding it by more than floating-point rounding.
#[test]
fn grouped_upness_is_bounded_by_peak_constant() {
    let result = run_frames(600, 1.0 / 60.0, MOUSE_POWER_PRESS, ParticleStats::default());
    assert!(
        result.grouped_upness <= GROUPED_UPNESS_PEAK + 1e-5,
        "grouped_upness {} exceeded GROUPED_UPNESS_PEAK {}",
        result.grouped_upness,
        GROUPED_UPNESS_PEAK,
    );
}

/// After release, `grouped_upness` must decay back below 0.2 within 60 frames.
#[test]
fn grouped_upness_decays_after_release() {
    let pressed = run_frames(60, 1.0 / 60.0, MOUSE_POWER_PRESS, ParticleStats::default());
    assert!(
        pressed.grouped_upness > 0.3,
        "prerequisite: press must raise grouped_upness"
    );

    let released = run_frames(60, 1.0 / 60.0, 0.0, pressed);
    assert!(
        released.grouped_upness < 0.2,
        "expected decay after release; got {}",
        released.grouped_upness,
    );
}

/// On press, `variance_length` must drop below the midpoint of
/// [`SPREAD_MIN`, `SPREAD_BASELINE`].
#[test]
fn variance_length_drops_on_press() {
    let result = run_frames(60, 1.0 / 60.0, MOUSE_POWER_PRESS, ParticleStats::default());
    let midpoint = f32::midpoint(SPREAD_MIN, SPREAD_BASELINE);
    assert!(
        result.variance_length < midpoint,
        "expected variance_length < {midpoint} on press; got {}",
        result.variance_length,
    );
}

/// `flat_ratio` is derived from `variance_length`: high variance (spread
/// cloud, rest or post-release) → high `flat_ratio` near
/// [`FLAT_RATIO_BASELINE`] (slow LFO); low variance (clustered, peak press)
/// → low `flat_ratio` near [`FLAT_RATIO_MIN`] (faster LFO). This indirect
/// derivation produces v4's signature post-release-elevated LFO behavior
/// for free, since `variance_length` has an asymmetric attack (fast
/// clumping, slow recovery).
#[test]
fn flat_ratio_inversely_tracks_variance_length() {
    // At rest, variance_length defaults to 0 (uninitialised). After a few
    // frames of zero-power input it stays at zero. Per the formula
    // `(variance - SPREAD_MIN) / (BASELINE - SPREAD_MIN)` clamped, that
    // gives variance_normalised = 0 → flat_ratio = FLAT_RATIO_MIN. After
    // the variance envelope has time to climb to baseline, flat_ratio rises
    // toward FLAT_RATIO_BASELINE.
    //
    // We run 600 frames of zero-input so variance_length asymptotes to
    // SPREAD_BASELINE = 1.0, which maps to flat_ratio = FLAT_RATIO_BASELINE.
    // Seed variance to baseline so the test mirrors steady-state rest.
    let stats = ParticleStats {
        variance_length: SPREAD_BASELINE,
        ..ParticleStats::default()
    };
    let at_rest = run_frames(600, 1.0 / 60.0, 0.0, stats);
    assert!(
        (at_rest.flat_ratio - FLAT_RATIO_BASELINE).abs() < 1e-3,
        "expected flat_ratio ≈ {FLAT_RATIO_BASELINE} when variance at baseline; got {}",
        at_rest.flat_ratio,
    );

    // After sustained press, variance asymptotes to SPREAD_MIN → flat_ratio
    // hits FLAT_RATIO_MIN.
    let on_press = run_frames(600, 1.0 / 60.0, MOUSE_POWER_PRESS, ParticleStats::default());
    assert!(
        (on_press.flat_ratio - FLAT_RATIO_MIN).abs() < 1e-3,
        "expected flat_ratio ≈ {FLAT_RATIO_MIN} on sustained press; got {}",
        on_press.flat_ratio,
    );
}

/// `normalized_entropy` and `normalized_variance_length` both track
/// `variance_length` directly. v4 reference capture (2026-05-26) shows their
/// runtime means are within ~1% of each other; treating them as one envelope
/// is well within perceptual tolerance.
///
/// [`ENTROPY_FLOOR`] is kept as a safety floor against future tuning that
/// might drop [`SPREAD_MIN`] below it, but with `SPREAD_MIN = 0.45 >
/// ENTROPY_FLOOR = 0.4` it never fires in practice — the assertion confirms
/// the floor is *at most* an unreachable cap, not an active clamp.
#[test]
fn normalised_entropy_tracks_variance_length() {
    let result = run_frames(600, 1.0 / 60.0, MOUSE_POWER_PRESS, ParticleStats::default());
    // After long sustained press, variance_length asymptotes to SPREAD_MIN.
    assert!(
        (result.variance_length - SPREAD_MIN).abs() < 1e-3,
        "expected variance_length ≈ {SPREAD_MIN} on sustained press; got {}",
        result.variance_length,
    );
    // ENTROPY_FLOOR ≤ SPREAD_MIN invariant is enforced at module load via
    // a `const _: () = assert!(...)` at the top of this file. Here we just
    // confirm the runtime consequence: entropy tracks variance_length
    // directly at peak press.
    assert_eq!(
        result.normalized_entropy, result.variance_length,
        "normalized_entropy should match variance_length when ENTROPY_FLOOR is inactive",
    );
    // normalized_variance_length tracks variance_length identically.
    assert_eq!(
        result.normalized_variance_length, result.variance_length,
        "normalized_variance_length should track variance_length",
    );
}
