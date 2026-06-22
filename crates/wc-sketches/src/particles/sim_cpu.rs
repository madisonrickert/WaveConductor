//! Shared CPU-side particle integrator — kernel-parity reference and
//! spawn-snapshot test fixture used by particle sketches' integration tests.
//! Not registered in any production schedule.
//!
//! This module is the CPU mirror of the WGSL compute kernel in
//! `assets/shaders/particles/simulate.wgsl`. Relocated from
//! `crate::line::sim_cpu` to this shared location so multiple particle sketches
//! can share the reference integrator and test fixture without duplicating code.
//!
//! Plan 11 Phase F removed the per-frame CPU-mirror step from the production
//! `LinePlugin` schedule. [`step_cpu_mirror`] is no longer registered as a
//! production system. The CPU mirror's role narrowed to:
//!
//! - **Spawn snapshot**: `spawn_line` still inserts [`CpuMirror`] with the
//!   initial particle layout so tests can read the spawn positions (used by
//!   `crates/wc-sketches/tests/line_heatmap_e2e.rs` to verify the heatmap
//!   sampler's output).
//! - **Test scaffolding**: tests that want to advance the CPU mirror
//!   explicitly can register [`step_cpu_mirror`] in their app builder. See
//!   the existing pattern in `crates/wc-sketches/tests/line_lifecycle.rs`
//!   if any tests still use it post-Phase F.
//!
//! The two integrators (WGSL kernel + Rust [`step_one`]) remain
//! mathematically equivalent to ≤1% float-op drift, documented in
//! `crates/wc-sketches/src/line/PARITY.md`.

use bevy::prelude::*;

#[cfg(test)]
use super::particle::Attractor;
use super::particle::{Particle, SimParams, MAX_ATTRACTORS};

/// CPU mirror of the particle storage buffer.
///
/// Populated by [`crate::line::systems::spawn_line`] with the initial
/// particle layout (spawn-time snapshot). In production (Plan 11 Phase F),
/// this resource is no longer stepped each frame — it serves as a read-only
/// snapshot for heatmap integration tests
/// (`crates/wc-sketches/tests/line_heatmap_e2e.rs`). Tests that need a
/// stepped mirror can register [`step_cpu_mirror`] in their own app builder.
#[derive(Resource, Default)]
pub struct CpuMirror {
    /// Particle state in the same layout as the GPU buffer.
    pub particles: Vec<Particle>,
}

/// Step the CPU mirror by one frame. The math mirrors the WGSL kernel
/// exactly; if you change one, change both, and re-check the parity test.
///
/// Not registered in `LinePlugin`'s production schedule as of Plan 11 Phase F.
/// Tests that want to advance the mirror can register this system explicitly
/// in their own `App` builder.
pub fn step_cpu_mirror(
    mut mirror: ResMut<'_, CpuMirror>,
    sim: Res<'_, super::compute::ParticleSimParams>,
) {
    let params = sim.params;
    for p in &mut mirror.particles {
        step_one(p, &params);
    }
}

/// Hermite smoothstep over `e0..e1`, clamped — mirrors WGSL's built-in
/// `smoothstep`, used for the localized-attractor influence falloff.
#[inline]
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * 2.0_f32.mul_add(-t, 3.0)
}

/// Divergence-free curl-noise turbulence flow — the exact CPU mirror of
/// `turbulence_force` in `simulate.wgsl`. Returns the flow direction at `pos`;
/// the caller advects position by `flow * speed * dt`. Allocation-free; keep in
/// lock-step with the WGSL version (and re-check parity) on any change.
#[inline]
#[allow(
    clippy::similar_names,
    reason = "a1/b1/a2/b2 are the stream-function octave arguments; the paired \
              names mirror the WGSL source and the math, renaming hurts clarity"
)]
fn turbulence_force(pos: [f32; 2], scale: f32, t: f32) -> [f32; 2] {
    let x = pos[0] * scale;
    let y = pos[1] * scale;
    let a1 = x + 0.13 * t;
    let b1 = y - 0.11 * t;
    let a2 = 2.0 * x - 0.17 * t;
    let b2 = 2.0 * y + 0.15 * t;
    let dpsi_dx = a1.cos() * b1.cos() + a2.cos() * b2.cos();
    let dpsi_dy = -a1.sin() * b1.sin() - a2.sin() * b2.sin();
    // curl = (d psi/dy, -d psi/dx).
    [dpsi_dy, -dpsi_dx]
}

/// Pure function, allocation-free: step a single particle. Called once per
/// particle per frame from [`step_cpu_mirror`]; extracted for unit testing.
/// Hot path — do not introduce branches or allocations.
pub fn step_one(p: &mut Particle, params: &SimParams) {
    // Attract-mode fraction kill (early-out), mirroring the WGSL kernel:
    // dead particles only fade their alpha out; they skip all sim math.
    let attract = params.attract_gate != 0;
    if attract && p.spawn_hash >= params.attract_fraction {
        if p.alpha > 0.0 && params.fade_duration > 0.0 {
            p.alpha = (p.alpha - params.dt / params.fade_duration).max(0.0);
        }
        return;
    }

    // Accumulate force. v4: constant-magnitude in unit direction toward attractor.
    let mut accel = [0.0_f32, 0.0];
    // `attractor_count` is u32 → usize is lossless on every supported target;
    // `try_from` keeps clippy happy without an explicit `as` cast.
    let active_count = usize::try_from(params.attractor_count)
        .unwrap_or(MAX_ATTRACTORS)
        .min(MAX_ATTRACTORS);
    for a in &params.attractors[..active_count] {
        if a.power <= 0.0 {
            continue;
        }
        let dx = a.position[0] - p.position[0];
        let dy = a.position[1] - p.position[1];
        let dist = (dx * dx + dy * dy).sqrt().max(1e-6);
        let inv_dist = 1.0 / dist;
        let mut force_mag = a.power * params.size_scale;
        // Localized attractors (radius > 0) fade to zero by `radius`; radius == 0
        // keeps the unbounded constant-magnitude pull (every current attractor).
        if a.radius > 0.0 {
            force_mag *= 1.0 - smoothstep(0.0, a.radius, dist);
        }
        accel[0] += dx * inv_dist * force_mag;
        accel[1] += dy * inv_dist * force_mag;
    }

    // Attract-mode noise turbulence (off — and provably inert — during Active,
    // where turbulence_amp is 0).
    if attract && params.turbulence_amp > 0.0 {
        let turb = turbulence_force(p.position, params.turbulence_scale, params.turbulence_time);
        accel[0] += turb[0] * params.turbulence_amp;
        accel[1] += turb[1] * params.turbulence_amp;
    }

    p.velocity[0] += accel[0] * params.dt;
    p.velocity[1] += accel[1] * params.dt;

    // Drag.
    let drag = if params.attractor_count > 0 {
        params.pulling_drag_baked
    } else {
        params.inertial_drag_baked
    };
    p.velocity[0] *= drag;
    p.velocity[1] *= drag;

    // Integrate.
    p.position[0] += p.velocity[0] * params.dt;
    p.position[1] += p.velocity[1] * params.dt;

    // Constrain.
    let oob = p.position[0] < params.constrain_min[0]
        || p.position[0] > params.constrain_max[0]
        || p.position[1] < params.constrain_min[1]
        || p.position[1] > params.constrain_max[1];
    if oob {
        p.position = p.original_xy;
        p.velocity = [0.0, 0.0];
        p.alpha = 0.0;
    }

    // Attract-mode lifetime respawn (mirrors the WGSL kernel): survivors age
    // while attract is on; past their CPU-seeded lifespan they reset exactly
    // like an OOB particle. During Active the age is pinned to 0.
    if attract {
        p.age += params.dt;
        if p.lifespan > 0.0 && p.age >= p.lifespan {
            p.age = 0.0;
            p.position = p.original_xy;
            p.velocity = [0.0, 0.0];
            p.alpha = 0.0;
        }
    } else {
        p.age = 0.0;
    }

    // Fade.
    if p.alpha < 1.0 && params.fade_duration > 0.0 {
        p.alpha = (p.alpha + params.dt / params.fade_duration).min(1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zero_attractor_params() -> SimParams {
        SimParams {
            dt: 0.016,
            attractor_count: 0,
            pulling_drag_baked: 0.9,
            inertial_drag_baked: 0.5,
            size_scale: 1.0,
            fade_duration: 3.0,
            constrain_min: [-100.0, -100.0],
            constrain_max: [100.0, 100.0],
            attract_gate: 0,
            attract_fraction: 1.0,
            turbulence_amp: 0.0,
            turbulence_scale: 0.0,
            turbulence_time: 0.0,
            stationary_constant: 0.0,
            attractors: [Attractor::default(); MAX_ATTRACTORS],
        }
    }

    #[test]
    fn no_attractors_uses_inertial_drag() {
        let params = zero_attractor_params();
        let mut p = Particle {
            position: [0.0, 0.0],
            velocity: [10.0, 0.0],
            original_xy: [0.0, 0.0],
            alpha: 1.0,
            age: 0.0,
            lifespan: 0.0,
            spawn_hash: 0.0,
            spawn_color: f32::from_bits(0x00FF_FFFF), // white = no colour tint
            _pad: 0.0,
        };
        step_one(&mut p, &params);
        // Inertial drag = 0.5, applied to velocity before integration.
        assert!((p.velocity[0] - 5.0).abs() < 1e-5, "got {}", p.velocity[0]);
    }

    #[test]
    fn one_attractor_pulls_particle() {
        let mut params = zero_attractor_params();
        params.attractor_count = 1;
        params.attractors[0] = Attractor {
            position: [100.0, 0.0],
            power: 1000.0,
            radius: 0.0,
        };
        let mut p = Particle {
            position: [0.0, 0.0],
            velocity: [0.0, 0.0],
            original_xy: [0.0, 0.0],
            alpha: 1.0,
            age: 0.0,
            lifespan: 0.0,
            spawn_hash: 0.0,
            spawn_color: f32::from_bits(0x00FF_FFFF), // white = no colour tint
            _pad: 0.0,
        };
        step_one(&mut p, &params);
        // Attractor at (100, 0), particle at (0, 0) → purely x-aligned pull.
        // Expected x-acceleration = power * size_scale * (dx/dist) = 1000 * 1.0 * 1.0.
        // Expected x-velocity ≈ accel * dt = power * size_scale * dt (before drag).
        // Pulling drag (params.pulling_drag_baked = 0.9) then scales the result,
        // so the post-drag value is ~10 % below `power * size_scale * dt`.
        // ±10 % tolerance catches any regression in the force formula while
        // admitting the single drag step.
        let expected_vx = params.attractors[0].power * params.size_scale * params.dt;
        let tolerance = expected_vx * 0.11; // ±11 % (absorbs one pulling-drag step of ~10%)
        assert!(
            (p.velocity[0] - expected_vx).abs() <= tolerance,
            "velocity[0] = {} should be within 11% of expected {} (power * size_scale * dt)",
            p.velocity[0],
            expected_vx,
        );
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "OOB reset writes exact-bit values; bit-for-bit equality is the correct check"
    )]
    fn oob_resets_to_original() {
        let mut params = zero_attractor_params();
        params.constrain_min = [-10.0, -10.0];
        params.constrain_max = [10.0, 10.0];
        let mut p = Particle {
            position: [50.0, 0.0],
            velocity: [10.0, 0.0],
            original_xy: [-5.0, 2.5],
            alpha: 1.0,
            age: 0.0,
            lifespan: 0.0,
            spawn_hash: 0.0,
            spawn_color: f32::from_bits(0x00FF_FFFF), // white = no colour tint
            _pad: 0.0,
        };
        step_one(&mut p, &params);
        assert_eq!(p.position, [-5.0, 2.5]);
        assert_eq!(p.velocity, [0.0, 0.0]);
        // OOB reset zeros alpha; the same step then applies one fade tick, so
        // the visible result is a partial fade-in from 0 toward `dt /
        // fade_duration`. This matches the WGSL kernel's ordering exactly.
        let expected_alpha_after_fade = params.dt / params.fade_duration;
        assert!(
            (p.alpha - expected_alpha_after_fade).abs() < 1e-6,
            "OOB-reset then fade should leave alpha at dt/fade_duration; got {}",
            p.alpha,
        );
    }

    #[test]
    fn alpha_fades_in() {
        let params = zero_attractor_params();
        let mut p = Particle {
            position: [0.0, 0.0],
            velocity: [0.0, 0.0],
            original_xy: [0.0, 0.0],
            alpha: 0.0,
            age: 0.0,
            lifespan: 0.0,
            spawn_hash: 0.0,
            spawn_color: f32::from_bits(0x00FF_FFFF), // white = no colour tint
            _pad: 0.0,
        };
        step_one(&mut p, &params);
        let expected = params.dt / params.fade_duration;
        assert!((p.alpha - expected).abs() < 1e-6, "got {}", p.alpha);
    }

    /// A live-mode particle, for the attract-gate tests below.
    fn live_particle() -> Particle {
        Particle {
            position: [3.0, 4.0],
            velocity: [1.0, -2.0],
            original_xy: [-5.0, 2.5],
            alpha: 1.0,
            age: 0.0,
            lifespan: 30.0,
            spawn_hash: 0.9,
            spawn_color: f32::from_bits(0x00FF_FFFF), // white = no colour tint
            _pad: 0.0,
        }
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "the dead branch must not touch position/velocity at all; exact equality is the check"
    )]
    fn attract_fraction_kills_high_hash_particles() {
        let mut params = zero_attractor_params();
        params.attract_gate = 1;
        params.attract_fraction = 0.6;
        let mut p = live_particle(); // spawn_hash 0.9 >= 0.6 -> dead
        step_one(&mut p, &params);
        // Dead particles fade out and freeze: no force, drag, or integration.
        assert!((p.alpha - (1.0 - params.dt / params.fade_duration)).abs() < 1e-6);
        assert_eq!(p.position, [3.0, 4.0], "dead particle must not move");
        assert_eq!(p.velocity, [1.0, -2.0], "dead particle keeps its velocity");
        // The fade-out bottoms at exactly 0 and stays there.
        p.alpha = 0.0;
        step_one(&mut p, &params);
        assert_eq!(p.alpha, 0.0);
    }

    #[test]
    fn attract_fraction_spares_low_hash_particles() {
        let mut params = zero_attractor_params();
        params.attract_gate = 1;
        params.attract_fraction = 0.6;
        let mut p = live_particle();
        p.spawn_hash = 0.3; // < 0.6 -> survivor
        step_one(&mut p, &params);
        // Survivors run the normal sim: inertial drag halves the velocity.
        assert!((p.velocity[0] - 0.5).abs() < 1e-5, "got {}", p.velocity[0]);
        assert!((p.age - params.dt).abs() < 1e-6, "survivor must age");
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "lifetime respawn writes exact-bit values; bit-for-bit equality is the correct check"
    )]
    fn attract_lifetime_respawns_at_lifespan() {
        let mut params = zero_attractor_params();
        params.attract_gate = 1;
        params.attract_fraction = 1.0; // everyone survives the fraction gate
        let mut p = live_particle();
        p.age = p.lifespan - params.dt * 0.5; // next tick crosses the lifespan
        step_one(&mut p, &params);
        // Reset exactly like an OOB particle: home, still, alpha-0 re-fade
        // (one fade tick applies in the same step, matching the kernel order).
        assert_eq!(p.position, [-5.0, 2.5]);
        assert_eq!(p.velocity, [0.0, 0.0]);
        assert_eq!(p.age, 0.0);
        assert!((p.alpha - params.dt / params.fade_duration).abs() < 1e-6);
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "Active-mode inertness must be exact, not approximate"
    )]
    fn attract_gate_off_is_inert() {
        // With the gate off (live interaction), a particle with a hash above
        // any fraction and an age beyond its lifespan still steps exactly like
        // the pre-attract kernel: no kill, no respawn, age pinned to zero.
        let params = zero_attractor_params(); // attract_gate = 0
        let mut gated = live_particle();
        gated.spawn_hash = 0.99;
        gated.age = 100.0; // way past lifespan; must be ignored and zeroed
        let mut baseline = gated;
        baseline.age = 0.0;
        step_one(&mut gated, &params);
        step_one(&mut baseline, &params);
        assert_eq!(gated.position, baseline.position);
        assert_eq!(gated.velocity, baseline.velocity);
        assert_eq!(gated.alpha, baseline.alpha);
        assert_eq!(gated.age, 0.0, "Active mode pins age to zero");
    }

    /// A roomy params (no constrain reset) for the radius / turbulence tests.
    fn unbounded_box_params() -> SimParams {
        let mut params = zero_attractor_params();
        params.constrain_min = [-1.0e6, -1.0e6];
        params.constrain_max = [1.0e6, 1.0e6];
        params
    }

    fn still_particle(x: f32, y: f32) -> Particle {
        Particle {
            position: [x, y],
            velocity: [0.0, 0.0],
            original_xy: [x, y],
            alpha: 1.0,
            age: 0.0,
            lifespan: 1.0e6,
            spawn_hash: 0.0,
            spawn_color: f32::from_bits(0x00FF_FFFF),
            _pad: 0.0,
        }
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "the past-radius particle feels zero force, so its velocity/position stay bit-exact"
    )]
    fn localized_attractor_pull_falls_off_past_its_radius() {
        // A radius-100 attractor at the origin pulls a particle inside the
        // radius but leaves one well outside it untouched — the localized-pull
        // falloff (an unbounded radius-0 attractor would pull both).
        let mut params = unbounded_box_params();
        params.attractor_count = 1;
        params.attractors[0] = Attractor {
            position: [0.0, 0.0],
            power: 1000.0,
            radius: 100.0,
        };
        let mut near = still_particle(40.0, 0.0); // dist 40 < radius
        let mut far = still_particle(300.0, 0.0); // dist 300 > radius → no pull
        step_one(&mut near, &params);
        step_one(&mut far, &params);
        assert!(
            near.velocity[0] < -1.0,
            "particle inside the radius is pulled inward, got {}",
            near.velocity[0]
        );
        // smoothstep(0,100,300) == 1 → falloff 0 → no force; the only velocity
        // change would be drag on its (zero) velocity, so it stays put.
        assert_eq!(
            far.velocity,
            [0.0, 0.0],
            "particle past the radius feels no pull"
        );
        assert_eq!(
            far.position,
            [300.0, 0.0],
            "untouched particle does not move"
        );
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "the off/inert paths must leave position bit-identical"
    )]
    fn turbulence_advects_only_in_attract_and_when_enabled() {
        let mut params = unbounded_box_params();
        params.attract_gate = 1;
        params.attract_fraction = 1.0; // everyone survives the fraction gate
        params.turbulence_scale = 0.01;
        params.turbulence_time = 1.0;

        // amp 0: no advection (the still particle has no other reason to move).
        params.turbulence_amp = 0.0;
        let mut off = still_particle(123.0, 45.0);
        step_one(&mut off, &params);
        assert_eq!(off.position, [123.0, 45.0], "amp 0 advects nothing");

        // amp > 0 in attract: the curl flow advects the position.
        params.turbulence_amp = 50.0;
        let mut on = still_particle(123.0, 45.0);
        step_one(&mut on, &params);
        assert_ne!(
            on.position,
            [123.0, 45.0],
            "turbulence advects the position"
        );

        // Same amp but Active (gate off): provably inert — position unchanged.
        params.attract_gate = 0;
        let mut active = still_particle(123.0, 45.0);
        step_one(&mut active, &params);
        assert_eq!(
            active.position,
            [123.0, 45.0],
            "turbulence is inert when the attract gate is off"
        );
    }
}
