#![allow(
    clippy::similar_names,
    clippy::excessive_precision,
    reason = "var_x2/var_y2 and dx2/dy2 names mirror v4 particleStats.ts directly; 1.383_870_349 and 0.288_66 are v4 constants preserved verbatim for audit-trail parity"
)]

//! Per-frame statistics over the CPU particle mirror.
//!
//! Direct port of v4's `src/particles/particleStats.ts::computeStats`. Reads
//! [`LineCpuMirror`] and writes [`ParticleStats`] each frame while the Line
//! sketch is `Active`. Plan 9's reactivity-coupling system
//! ([`super::audio_coupling::drive_audio_and_shader`]) reads `ParticleStats`
//! and drives the Line synth voice + gravity-smear post-process uniforms.
//!
//! ## Why CPU?
//!
//! Stats are scalars summarising the full particle population. A GPU reduction
//! would require a readback stall (frame N+1 sees frame N's values, GPU/CPU
//! sync cost). Plan 7's [`LineCpuMirror`] is a parallel CPU integrator
//! deliberately introduced so this module can read per-particle state with
//! zero readback latency. The GPU sim remains authoritative for rendering;
//! drift between the two is bounded at ≤1% from float-op order differences,
//! which is well within tolerance for the smooth audio control signals here.
//!
//! ## Formula provenance
//!
//! All seven fields are direct ports of v4's `computeStats`:
//! - `averageVel = sqrt(mean(vx² + vy²))` — RMS velocity magnitude.
//! - `varianceLength = sqrt(varX² + varY²)` — 2D spread of positions.
//! - `flatRatio = varianceX / varianceY` — aspect ratio of the cloud (1.0
//!   when varianceY is zero to avoid div-by-zero; in practice this only
//!   happens during the first frame before any motion).
//! - `groupedUpness = sqrt(averageVel / varianceLength)` — high when the
//!   particles are moving fast *and* clustered together (the "gathering"
//!   condition that v4 keys most of its musical reactivity on).
//! - `entropy = sum(length × ln(length)) / N` where `length` is the distance
//!   from each particle to the mean position. Normalised by
//!   `width × 1.383870349` (the v4 constant; documented in v4 as the
//!   theoretical max for a uniform spread).
//! - `normalizedVarianceLength = varianceLength / (0.28866 × width)`.
//! - `normalizedAverageVel = averageVel / width`.

use bevy::prelude::*;

use super::sim_cpu::LineCpuMirror;

/// Statistics over the current particle population.
///
/// All values are dimensionless or normalised except `average_vel` (px/s) and
/// `variance_length` (px). Default is all-zero; produced by Bevy when the
/// resource is first initialised and reset by [`update_particle_stats`] if
/// the mirror is empty.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct ParticleStats {
    /// RMS speed across all particles, in world-pixel units per second.
    pub average_vel: f32,
    /// 2D variance length `sqrt(varX² + varY²)` over particle positions,
    /// in world-pixel units. Measures how spread out the cloud is.
    pub variance_length: f32,
    /// Position-cloud aspect ratio `varianceX / varianceY`. Hovers near 1
    /// for a circular spread, grows when the cloud is horizontally elongated.
    /// Forced to 1.0 if `varianceY` is zero (first-frame degenerate case).
    pub flat_ratio: f32,
    /// `sqrt(average_vel / variance_length)`. High when particles are moving
    /// fast *and* tightly clustered — the v4 "gathering" condition that gates
    /// most of the musical reactivity.
    pub grouped_upness: f32,
    /// Entropy of particle-to-centroid distances, normalised by
    /// `width × 1.383870349` (v4 constant). Dimensionless.
    pub normalized_entropy: f32,
    /// `variance_length` divided by `0.28866 × width` (v4 normalisation
    /// constant). Dimensionless.
    pub normalized_variance_length: f32,
    /// `average_vel` divided by canvas width. Dimensionless.
    pub normalized_average_vel: f32,
}

/// Update `ParticleStats` from the current [`LineCpuMirror`] state.
///
/// Two passes over the particle array:
/// 1. Compute mean position and mean squared velocity.
/// 2. Compute X/Y variances around the mean and the
///    `sum(length × ln(length))` entropy term.
///
/// Then derive the seven `ParticleStats` fields from those reductions.
///
/// When the mirror is empty (between `OnExit` resetting and the next
/// `spawn_line` populating it), writes [`ParticleStats::default`] so
/// downstream consumers see zero values rather than stale data.
pub fn update_particle_stats(
    mirror: Res<'_, LineCpuMirror>,
    window: Single<'_, '_, &bevy::window::Window>,
    mut stats: ResMut<'_, ParticleStats>,
) {
    let n = mirror.particles.len();
    if n == 0 {
        *stats = ParticleStats::default();
        return;
    }
    // `usize → f32` is intentional for the reduction divisor; particle counts
    // up to ~16M are exactly representable in f32, well beyond our needs.
    #[allow(
        clippy::cast_precision_loss,
        clippy::as_conversions,
        reason = "particle counts are well under f32's exact-int limit (~16M)"
    )]
    let n_f = n as f32;

    // --- Pass 1: mean position, mean velocity² ---
    let mut avg_x = 0.0_f32;
    let mut avg_y = 0.0_f32;
    let mut avg_vel2 = 0.0_f32;
    for p in &mirror.particles {
        avg_x += p.position[0];
        avg_y += p.position[1];
        // Squared speed = vx² + vy²; mean of squared speeds (the RMS² value).
        avg_vel2 += p.velocity[0] * p.velocity[0] + p.velocity[1] * p.velocity[1];
    }
    avg_x /= n_f;
    avg_y /= n_f;
    avg_vel2 /= n_f;

    // --- Pass 2: position variances around the mean + entropy term ---
    let mut var_x2 = 0.0_f32;
    let mut var_y2 = 0.0_f32;
    let mut entropy = 0.0_f32;
    for p in &mirror.particles {
        let dx = p.position[0] - avg_x;
        let dy = p.position[1] - avg_y;
        let dx2 = dx * dx;
        let dy2 = dy * dy;
        var_x2 += dx2;
        var_y2 += dy2;
        // `length × ln(length)` term — guard the `ln(0)` case. Particles
        // exactly on the centroid contribute zero (which is `lim x→0 x·ln(x)`).
        let length = (dx2 + dy2).sqrt();
        if length > 0.0 {
            entropy += length * length.ln();
        }
    }
    entropy /= n_f;
    var_x2 /= n_f;
    var_y2 /= n_f;

    let variance_x = var_x2.sqrt();
    let variance_y = var_y2.sqrt();
    // 2D "spread length" — Euclidean combination of per-axis std deviations.
    let variance_length = (var_x2 + var_y2).sqrt();
    let average_vel = avg_vel2.sqrt();

    // Aspect ratio of the cloud. Degenerate when `variance_y` is zero
    // (only possible when all particles share a y-coordinate, e.g. before
    // any motion). v4 returns 1.0 in that case; mirror it here.
    let flat_ratio = if variance_y > 0.0 {
        variance_x / variance_y
    } else {
        1.0
    };
    // Clamp width to ≥1 to avoid div-by-zero in headless / minimised cases.
    let width = window.width().max(1.0);

    stats.average_vel = average_vel;
    stats.variance_length = variance_length;
    stats.flat_ratio = flat_ratio;
    // `grouped_upness` is the v4 musical-reactivity primary driver:
    // sqrt(speed / spread) — large when fast + clustered.
    stats.grouped_upness = if variance_length > 0.0 {
        (average_vel / variance_length).sqrt()
    } else {
        0.0
    };
    // v4 normalisation constants (`1.383870349`, `0.28866`) preserved verbatim
    // as the audit trail back to v4's source. They are empirical scaling
    // factors that bring the normalised values into ~[0, 1] for typical
    // canvas widths.
    stats.normalized_entropy = entropy / (width * 1.383_870_349);
    stats.normalized_variance_length = variance_length / (0.288_66 * width);
    stats.normalized_average_vel = average_vel / width;
}

#[cfg(test)]
#[path = "particle_stats_tests.rs"]
mod tests;
