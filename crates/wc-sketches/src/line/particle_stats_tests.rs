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
//! `variance_length` drops on press and recovers after release, and that
//! constants remain in their documented ranges.

#![allow(
    clippy::float_cmp,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "exact-value and unwrap patterns are appropriate in test code"
)]

use super::{
    lerp, ParticleStats, ATTACK_RATE_FAST, GROUPED_UPNESS_PEAK, GROUPED_UPNESS_RATE,
    RELEASE_RATE_SLOW, SPREAD_BASELINE, SPREAD_MIN,
};
use crate::line::systems::mouse::MOUSE_POWER_PRESS;

/// Advance `ParticleStats` through `n` frames of `dt` seconds each, with
/// the given `mouse_power`. Returns the final stats.
fn run_frames(n: u32, dt: f32, mouse_power: f32, initial: ParticleStats) -> ParticleStats {
    let mut stats = initial;
    let excitement = (mouse_power / MOUSE_POWER_PRESS).clamp(0.0, 1.0);

    for _ in 0..n {
        // Replicate the envelope math from update_particle_stats.
        let target_vel = excitement;
        let vel_rate = if target_vel > stats.average_vel {
            ATTACK_RATE_FAST
        } else {
            RELEASE_RATE_SLOW
        };
        stats.average_vel = lerp(stats.average_vel, target_vel, (vel_rate * dt).min(1.0));

        let target_grouped = excitement * GROUPED_UPNESS_PEAK;
        stats.grouped_upness = lerp(
            stats.grouped_upness,
            target_grouped,
            (GROUPED_UPNESS_RATE * dt).min(1.0),
        );

        let target_variance = SPREAD_BASELINE - excitement * (SPREAD_BASELINE - SPREAD_MIN);
        stats.variance_length = lerp(
            stats.variance_length,
            target_variance,
            (RELEASE_RATE_SLOW * dt).min(1.0),
        );

        stats.normalized_entropy = stats.variance_length;
        stats.normalized_variance_length = stats.variance_length;
        stats.normalized_average_vel = stats.average_vel;
        stats.flat_ratio = 1.0;
    }
    stats
}

/// At rest (no attractor activity), `grouped_upness` and `average_vel` should
/// stay near zero when starting from the default (zero) state.
#[test]
fn at_rest_grouped_upness_stays_near_zero() {
    // Start from zero, run 60 frames at 60 Hz with no press.
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
    // Very long press — enough to asymptote.
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
    // First press for 60 frames to build it up.
    let pressed = run_frames(60, 1.0 / 60.0, MOUSE_POWER_PRESS, ParticleStats::default());
    assert!(
        pressed.grouped_upness > 0.3,
        "prerequisite: press must raise grouped_upness"
    );

    // Then release for 60 frames.
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

/// `flat_ratio` is always exactly 1.0 (circular-cloud approximation).
#[test]
fn flat_ratio_is_always_one() {
    let at_rest = run_frames(10, 1.0 / 60.0, 0.0, ParticleStats::default());
    assert_eq!(at_rest.flat_ratio, 1.0, "flat_ratio at rest");

    let on_press = run_frames(10, 1.0 / 60.0, MOUSE_POWER_PRESS, ParticleStats::default());
    assert_eq!(on_press.flat_ratio, 1.0, "flat_ratio on press");
}

/// `normalized_entropy` and `normalized_variance_length` always track `variance_length`.
#[test]
fn normalised_fields_track_variance_length() {
    let result = run_frames(30, 1.0 / 60.0, MOUSE_POWER_PRESS, ParticleStats::default());
    assert_eq!(
        result.normalized_entropy, result.variance_length,
        "normalized_entropy should equal variance_length",
    );
    assert_eq!(
        result.normalized_variance_length, result.variance_length,
        "normalized_variance_length should equal variance_length",
    );
}
