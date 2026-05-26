//! CPU-side approximated audio control signals for the Line sketch.
//!
//! ## Approach
//!
//! v4 computed full per-particle aggregate statistics each frame (`computeStats`
//! in `src/particles/particleStats.ts` — averageVel, varianceX/Y, entropy,
//! flatRatio, groupedUpness). Plan 7's GPU compute pipeline moved particle
//! physics off-CPU, which would have required either:
//!
//! - a per-frame GPU readback (sync stall, audio reacts to stale visuals), or
//! - a CPU mirror running the same physics twice (Plan 7–10's solution, ~50µs/
//!   frame at 12k particles).
//!
//! Plan 11 Phase F replaces both with smoothed CPU envelopes driven by
//! [`super::systems::MouseAttractorState`] state. The audio coupling doesn't
//! need exact statistics — it needs the right *perceptual shape* of the
//! control signals. `groupedUpness` rising on press, plateauing during hold,
//! decaying after release: that shape can be captured by a handful of
//! attack/release envelopes at near-zero CPU cost (~1µs/frame).
//!
//! Cymatics (v4) is the architectural reference: its audio coupling reads
//! CPU-side input scalars (`activeRadius`, `numCycles`, etc.) that drive the
//! GPU compute simulation, never reading the simulation state back. Phase F
//! brings Line in line with that pattern.
//!
//! ## Perceptual deviation
//!
//! This is a *perceptual* approximation, not mathematical parity. Values
//! within each field's expected range, dynamics within expected attack/
//! release shapes, but a side-by-side comparison against v4 will show
//! frame-by-frame numerical differences. Signed off as an approved deviation
//! in `PARITY.md`.
//!
//! ## Tuning
//!
//! The constants at the bottom of this module (`ATTACK_RATE_FAST`,
//! `GROUPED_UPNESS_PEAK`, `SPREAD_MIN`, etc.) are tunable knobs that shape
//! the musical response. If a specific downstream synth parameter sounds
//! wrong on manual sign-off, adjust the relevant constant and re-test.

use bevy::prelude::*;

use super::systems::mouse::{MouseAttractorState, MOUSE_POWER_FLOOR};

/// Statistics over the current particle population.
///
/// All values are dimensionless or normalised. Default is all-zero; produced
/// by Bevy when the resource is first initialised and populated each frame
/// by [`update_particle_stats`] via smoothed attack/release envelopes keyed
/// on [`MouseAttractorState`]. See module rustdoc for the perceptual-
/// approximation rationale.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct ParticleStats {
    /// RMS speed across all particles, in world-pixel units per second.
    /// Approximated: snaps up on press, decays slowly on release via
    /// [`ATTACK_RATE_FAST`] / [`RELEASE_RATE_SLOW`] envelopes.
    pub average_vel: f32,
    /// 2D variance length `sqrt(varX² + varY²)` over particle positions.
    /// Approximated: high at rest (particles spread along spawn line),
    /// drops during press (cluster forms around attractor).
    pub variance_length: f32,
    /// Position-cloud aspect ratio `varianceX / varianceY`. Held at 1.0
    /// (circular cloud) for this approximation. Tune if the LFO frequency
    /// target sounds off during manual sign-off.
    pub flat_ratio: f32,
    /// `sqrt(average_vel / variance_length)`. High when particles are moving
    /// fast *and* tightly clustered — the v4 "gathering" condition that gates
    /// most of the musical reactivity.
    pub grouped_upness: f32,
    /// Entropy of particle-to-centroid distances, normalised. Approximated
    /// from `variance_length` (same scalar, different downstream formula).
    pub normalized_entropy: f32,
    /// `variance_length` divided by the v4 normalisation constant
    /// `0.28866 × width`. Approximated from `variance_length`.
    pub normalized_variance_length: f32,
    /// `average_vel` divided by canvas width. Approximated from
    /// `average_vel`.
    pub normalized_average_vel: f32,
}

/// Per-frame approximated CPU audio control signals.
///
/// Reads [`MouseAttractorState::power`] and time, smooths them into envelopes
/// shaped like v4's full per-particle reduction, and writes [`ParticleStats`].
/// See module rustdoc for the perceptual-approximation rationale.
pub fn update_particle_stats(
    mouse: Res<'_, MouseAttractorState>,
    time: Res<'_, Time>,
    mut stats: ResMut<'_, ParticleStats>,
) {
    let dt = time.delta_secs();

    // Normalised attractor activity in [0, 1]. Drives all downstream envelopes.
    //
    // Divide by `MOUSE_POWER_FLOOR` (not `MOUSE_POWER_PRESS`) so excitement
    // stays at 1.0 throughout the held period. `decay_mouse_attractor` brings
    // power asymptotically to the floor; with this normalisation, anything
    // power ≥ floor clamps to excitement = 1.0, matching v4 where
    // groupedUpness stays elevated during sustained press (particles keep
    // orbiting the attractor). Power == 0 (explicit release) is the only
    // way excitement reaches 0.
    let excitement = (mouse.power / MOUSE_POWER_FLOOR).clamp(0.0, 1.0);

    // average_vel: snaps up on press, decays slowly. Asymmetric attack/release
    // matches v4's behavior where particles accelerate fast (forces are strong
    // near the attractor) but slow gradually due to drag after release.
    let target_vel = excitement;
    let vel_rate = if target_vel > stats.average_vel {
        ATTACK_RATE_FAST
    } else {
        RELEASE_RATE_SLOW
    };
    stats.average_vel = lerp(stats.average_vel, target_vel, (vel_rate * dt).min(1.0));

    // grouped_upness: lags average_vel slightly (clustering follows acceleration).
    // Peak value tuned to v4's typical sustained-press range (~0.5-0.8).
    let target_grouped = excitement * GROUPED_UPNESS_PEAK;
    stats.grouped_upness = lerp(
        stats.grouped_upness,
        target_grouped,
        (GROUPED_UPNESS_RATE * dt).min(1.0),
    );

    // variance_length: high at rest (particles spread along the horizontal spawn
    // line), drops during press (cluster forms around attractor). Mapped so
    // 1.0 = full spread, SPREAD_MIN = tight cluster.
    //
    // Asymmetric attack/release: clumping on press happens fast (in v4 the
    // particles clump within ~0.2-0.3 s of the gravity well's activation),
    // while re-spreading after release follows the same slow inertial decay
    // as `average_vel`. Note the inverted direction: variance *decreases* on
    // press, so the "attack" branch fires when target < current.
    let target_variance = SPREAD_BASELINE - excitement * (SPREAD_BASELINE - SPREAD_MIN);
    let variance_rate = if target_variance < stats.variance_length {
        ATTACK_RATE_FAST
    } else {
        RELEASE_RATE_SLOW
    };
    stats.variance_length = lerp(
        stats.variance_length,
        target_variance,
        (variance_rate * dt).min(1.0),
    );

    // Normalised stats derive from variance_length. v4 used per-axis math here;
    // for perceptual parity a shared scalar is sufficient (the audio formulas
    // downstream treat them as smooth ranges, not exact ratios).
    stats.normalized_entropy = stats.variance_length;
    stats.normalized_variance_length = stats.variance_length;

    // normalized_average_vel follows average_vel directly.
    stats.normalized_average_vel = stats.average_vel;

    // flat_ratio: aspect ratio of the cloud. v4 varies this with mouse motion
    // direction; for a first cut we hold at 1.0 (circular cloud). Tune if
    // manual sign-off shows the LFO frequency target sounds off.
    stats.flat_ratio = 1.0;
}

/// Linear interpolation. `t` is clamped to [0, 1] by the caller before this is
/// reached, so no clamp inside.
fn lerp(current: f32, target: f32, t: f32) -> f32 {
    current + (target - current) * t
}

// Tuning constants — initial values from architectural review. Adjust during
// manual sign-off if specific musical effects need shaping.

/// Attack rate for `average_vel` rising. Higher = snappier press response.
const ATTACK_RATE_FAST: f32 = 8.0;

/// Release rate for `average_vel` and `variance_length`. Lower = longer audio
/// tail after release. v4's particle drag (`PULLING_DRAG=0.93075`) takes ~1s
/// for velocity to halve; this rate matches that decay shape.
const RELEASE_RATE_SLOW: f32 = 1.5;

/// Attack rate for `grouped_upness`. Slightly slower than `average_vel`
/// because clustering follows acceleration.
const GROUPED_UPNESS_RATE: f32 = 3.0;

/// Peak `grouped_upness` value at sustained-press excitement = 1.0. Tuned to
/// v4's observed sustained-press range. v4's `groupedUpness` peaks roughly in
/// `[0.5, 1.0]` during normal sustained press, with brief excursions toward
/// 1.5+ on very tight, fast clusters. The downstream synth volume formula is
/// `max(grouped_upness - 0.05, 0) * 5`, so this cap of 0.9 produces a volume
/// peak of `4.25` — reclaims the upper dynamic range that the initial 0.7
/// value clipped off the synth's most expressive moments.
const GROUPED_UPNESS_PEAK: f32 = 0.9;

/// `variance_length` at rest (no attractor activity). Particles spread across
/// the canvas; normalised to 1.0 as the "fully spread" baseline.
const SPREAD_BASELINE: f32 = 1.0;

/// `variance_length` at sustained-press peak excitement = 1.0. Particles
/// tightly clustered around the attractor. The downstream noise filter cutoff
/// formula is `2000 * normalized_variance_length`, so this floor of 0.1
/// produces a cutoff of 200 Hz at peak clustering — matches v4's noise
/// lowpass floor (`normalizedVarianceLength → ~0.1` at tightest clustering,
/// noise filter cutoff → ~200 Hz).
const SPREAD_MIN: f32 = 0.1;

#[cfg(test)]
#[path = "particle_stats_tests.rs"]
mod tests;
