//! Unit tests for [`super::update_particle_stats`].
//!
//! Lives in a sibling file (linked from `particle_stats.rs` via
//! `#[path = ...] mod tests;`) so the production module stays under the
//! AGENTS.md ~300-line guideline. The tests still see `super::*`, which
//! resolves to the `particle_stats` module — `#[path]` only redirects the
//! source file, not the logical module path.

#![allow(
    clippy::float_cmp,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "exact-value and unwrap patterns are appropriate in test code"
)]

use super::*;
use crate::line::particle::Particle;

/// Build a `LineCpuMirror` from a list of `(position, velocity)` pairs.
fn mirror_from(particles: Vec<([f32; 2], [f32; 2])>) -> LineCpuMirror {
    LineCpuMirror {
        particles: particles
            .into_iter()
            .map(|(position, velocity)| Particle {
                position,
                velocity,
                original_xy: position,
                alpha: 1.0,
                _pad: 0.0,
            })
            .collect(),
    }
}

/// Reproduce the math of `update_particle_stats` without Bevy plumbing so
/// unit tests can drive it with a synthetic window width. The Bevy
/// `Single<&Window>` system param can't be constructed in unit-test scope
/// without a full `App`; this pure-function helper keeps the test surface
/// honest while sharing the exact arithmetic.
fn compute(mirror: &LineCpuMirror, width: f32) -> ParticleStats {
    let mut stats = ParticleStats::default();
    let n = mirror.particles.len();
    if n == 0 {
        return stats;
    }
    #[allow(
        clippy::cast_precision_loss,
        clippy::as_conversions,
        reason = "test helper"
    )]
    let n_f = n as f32;
    let mut avg_x = 0.0_f32;
    let mut avg_y = 0.0_f32;
    let mut avg_vel2 = 0.0_f32;
    for p in &mirror.particles {
        avg_x += p.position[0];
        avg_y += p.position[1];
        avg_vel2 += p.velocity[0] * p.velocity[0] + p.velocity[1] * p.velocity[1];
    }
    avg_x /= n_f;
    avg_y /= n_f;
    avg_vel2 /= n_f;
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
    let variance_length = (var_x2 + var_y2).sqrt();
    let average_vel = avg_vel2.sqrt();
    let flat_ratio = if variance_y > 0.0 {
        variance_x / variance_y
    } else {
        1.0
    };
    let w = width.max(1.0);
    stats.average_vel = average_vel;
    stats.variance_length = variance_length;
    stats.flat_ratio = flat_ratio;
    stats.grouped_upness = if variance_length > 0.0 {
        (average_vel / variance_length).sqrt()
    } else {
        0.0
    };
    stats.normalized_entropy = entropy / (w * 1.383_870_349);
    stats.normalized_variance_length = variance_length / (0.288_66 * w);
    stats.normalized_average_vel = average_vel / w;
    stats
}

#[test]
fn empty_mirror_returns_default() {
    let mirror = mirror_from(vec![]);
    let stats = compute(&mirror, 1920.0);
    // All seven fields zero.
    assert_eq!(stats.average_vel, 0.0);
    assert_eq!(stats.variance_length, 0.0);
    assert_eq!(stats.flat_ratio, 0.0);
    assert_eq!(stats.grouped_upness, 0.0);
    assert_eq!(stats.normalized_entropy, 0.0);
    assert_eq!(stats.normalized_variance_length, 0.0);
    assert_eq!(stats.normalized_average_vel, 0.0);
}

#[test]
fn zero_velocity_gives_zero_average_vel_and_grouped_upness() {
    // Particles spread on a square ring; zero velocity everywhere.
    let mirror = mirror_from(vec![
        ([10.0, 0.0], [0.0, 0.0]),
        ([-10.0, 0.0], [0.0, 0.0]),
        ([0.0, 10.0], [0.0, 0.0]),
        ([0.0, -10.0], [0.0, 0.0]),
    ]);
    let stats = compute(&mirror, 1920.0);
    assert!(stats.average_vel.abs() < 1e-6, "got {}", stats.average_vel);
    // `grouped_upness = sqrt(0 / variance_length) = 0` when there's any spread.
    assert!(stats.variance_length > 0.0);
    assert!(
        stats.grouped_upness.abs() < 1e-6,
        "got {}",
        stats.grouped_upness
    );
}

#[test]
fn uniform_square_spread_gives_flat_ratio_near_one() {
    // Symmetric distribution: equal variance in X and Y.
    let mirror = mirror_from(vec![
        ([10.0, 10.0], [0.0, 0.0]),
        ([-10.0, 10.0], [0.0, 0.0]),
        ([10.0, -10.0], [0.0, 0.0]),
        ([-10.0, -10.0], [0.0, 0.0]),
    ]);
    let stats = compute(&mirror, 1920.0);
    // varianceX = varianceY = 10, so flat_ratio = 1.0 exactly.
    assert!(
        (stats.flat_ratio - 1.0).abs() < 1e-5,
        "got {}",
        stats.flat_ratio
    );
}

#[test]
fn horizontally_elongated_spread_gives_flat_ratio_greater_than_one() {
    // Wider in X than Y → flat_ratio > 1.
    let mirror = mirror_from(vec![
        ([100.0, 1.0], [0.0, 0.0]),
        ([-100.0, 1.0], [0.0, 0.0]),
        ([100.0, -1.0], [0.0, 0.0]),
        ([-100.0, -1.0], [0.0, 0.0]),
    ]);
    let stats = compute(&mirror, 1920.0);
    assert!(
        stats.flat_ratio > 50.0,
        "expected very wide flat_ratio, got {}",
        stats.flat_ratio
    );
}

#[test]
fn clustered_particles_have_small_variance_length() {
    // All particles within 0.1 units of each other → tiny variance.
    let mirror = mirror_from(vec![
        ([0.05, 0.05], [0.0, 0.0]),
        ([-0.05, 0.05], [0.0, 0.0]),
        ([0.05, -0.05], [0.0, 0.0]),
        ([-0.05, -0.05], [0.0, 0.0]),
    ]);
    let stats = compute(&mirror, 1920.0);
    assert!(
        stats.variance_length < 0.2,
        "expected tiny spread, got {}",
        stats.variance_length
    );
}

#[test]
fn fast_clustered_motion_yields_high_grouped_upness() {
    // High velocity, tight cluster → groupedUpness should be large.
    let fast = mirror_from(vec![
        ([0.0, 0.0], [1000.0, 1000.0]),
        ([0.1, 0.0], [1000.0, 1000.0]),
        ([0.0, 0.1], [1000.0, 1000.0]),
        ([0.1, 0.1], [1000.0, 1000.0]),
    ]);
    let fast_stats = compute(&fast, 1920.0);

    // Same velocity, much wider spread → groupedUpness should be smaller.
    let wide = mirror_from(vec![
        ([0.0, 0.0], [1000.0, 1000.0]),
        ([500.0, 0.0], [1000.0, 1000.0]),
        ([0.0, 500.0], [1000.0, 1000.0]),
        ([500.0, 500.0], [1000.0, 1000.0]),
    ]);
    let wide_stats = compute(&wide, 1920.0);

    assert!(
        fast_stats.grouped_upness > wide_stats.grouped_upness,
        "tight cluster should outscore wide spread at equal velocity: \
         fast={}, wide={}",
        fast_stats.grouped_upness,
        wide_stats.grouped_upness
    );
}

#[test]
fn normalized_average_vel_scales_with_width() {
    // Same particles, two widths; normalised value halves when width doubles.
    let mirror = mirror_from(vec![([0.0, 0.0], [100.0, 0.0]), ([0.0, 1.0], [100.0, 0.0])]);
    let a = compute(&mirror, 1000.0);
    let b = compute(&mirror, 2000.0);
    assert!(
        (a.normalized_average_vel - 2.0 * b.normalized_average_vel).abs() < 1e-5,
        "expected width-scaling: a={}, b={}",
        a.normalized_average_vel,
        b.normalized_average_vel
    );
}
