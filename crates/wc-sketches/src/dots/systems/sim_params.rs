//! Per-frame writer for [`crate::particles::compute::ParticleSimParams`].
//!
//! Produces the Dots-specific [`crate::particles::particle::SimParams`] each
//! frame: drag constants baked against [`V4_FIXED_DT_DOTS`] (v4 Dots parity),
//! `size_scale` from canvas width (canvas-width multiplier only — gravity is
//! baked into attractor power host-side in Task 3), stationary spring
//! (`0.01`), and effectively infinite constrain bounds (`constrainToBox =
//! false` in v4). No attractors in D2; Task 3 adds the mouse at index 0.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "f32 casts for window-derived sizing are intentional"
)]

use bevy::prelude::*;

use crate::particles::compute::ParticleSimParams;
use crate::particles::particle::{Attractor, SimParams, MAX_ATTRACTORS};

/// v4 Dots fixed simulation timestep. Drag constants are baked against this
/// value (not the render `dt`) so each per-frame multiplier matches v4
/// regardless of actual frame rate — matching Line's `V4_FIXED_DT` convention.
pub const V4_FIXED_DT_DOTS: f32 = 0.048;

/// v4 Dots `PULLING_DRAG_CONSTANT`. Baked via `pow(_, V4_FIXED_DT_DOTS)` to
/// produce the per-frame drag the compute kernel applies when attractors are
/// active. Trailing digits are preserved verbatim from v4 for parity auditing.
#[allow(
    clippy::excessive_precision,
    clippy::unreadable_literal,
    reason = "v4 Dots parity"
)]
pub const V4_DOTS_PULLING_DRAG: f32 = 0.96075095702;

/// v4 Dots `INERTIAL_DRAG_CONSTANT`. Baked via `pow(_, V4_FIXED_DT_DOTS)`.
/// Stronger damping (closer to 0 when baked) than pulling drag so free
/// particles decelerate quickly while attracted particles stay responsive.
#[allow(
    clippy::excessive_precision,
    clippy::unreadable_literal,
    reason = "v4 Dots parity"
)]
pub const V4_DOTS_INERTIAL_DRAG: f32 = 0.23913643334;

/// v4 Dots `gravity_constant`. Baked into each attractor's `power` host-side
/// (Task 3 Step 2); declared here so all Dots sim-param constants live in one
/// place. Not used in D2 (no attractors yet).
pub const DOTS_GRAVITY_CONSTANT: f32 = 100.0;

/// `Update` — gated by `sketch_active(AppState::Dots)`.
///
/// Writes [`SimParams`] into [`ParticleSimParams`] each frame with v4 Dots
/// values:
///
/// - Drag baked against [`V4_FIXED_DT_DOTS`] (not render dt) for v4 parity.
/// - `size_scale = min(2^(w/836 − 1), 1)` — canvas-width multiplier ONLY.
///   The compute kernel uses `force_mag = a.power × size_scale`; `power`
///   carries `gravity_constant × raw_power` (baked host-side in Task 3).
///   This matches Line's convention exactly so the kernel formula is uniform.
/// - `fade_duration = 3.0`, `stationary_constant = 0.01`.
/// - `constrain_min/max = ±1 × 10⁹` — OOB→home reset never fires (v4
///   `constrainToBox = false`).
/// - `attract_gate = 0`, attract/turbulence fields = off. `attractor_count = 0`.
///
/// Per-frame no-allocation guarantee: all arithmetic is on stack scalars; the
/// attractor array is a zero-initialized stack array.
pub fn update_dots_sim_params(
    time: Res<'_, Time>,
    window: Single<'_, '_, &Window>,
    mut sim: ResMut<'_, ParticleSimParams>,
) {
    let w = window.width();

    // --- Drag baking (v4 Dots parity, against the fixed Dots dt) --------
    let pulling_drag_baked = V4_DOTS_PULLING_DRAG.powf(V4_FIXED_DT_DOTS);
    let inertial_drag_baked = V4_DOTS_INERTIAL_DRAG.powf(V4_FIXED_DT_DOTS);

    // --- Size scaling: canvas-width multiplier ONLY ----------------------
    // v4: `min(2^(w / 836 - 1), 1)`. Gravity is baked into attractor power
    // host-side (Task 3); size_scale carries only the width term, matching
    // Line's `bake_sim_params` convention exactly so the kernel formula is
    // uniform across sketches (`force_mag = a.power * size_scale`).
    let size_scale = (2.0_f32.powf(w / 836.0 - 1.0)).min(1.0);

    // --- No attractors in D2 -------------------------------------------
    // Task 3 adds the mouse attractor at index 0 and bakes gravity_constant
    // into the power. Until then the array stays zero and attractor_count = 0;
    // the grid sits at home positions held only by the stationary spring.
    let attractors = [Attractor::default(); MAX_ATTRACTORS];

    sim.params = SimParams {
        // dt: per-frame delta capped at 50 ms. Matches Line's convention:
        // `bake_sim_params` applies `dt.min(0.05)` before passing to the kernel.
        dt: time.delta_secs().min(0.05),
        attractor_count: 0,
        pulling_drag_baked,
        inertial_drag_baked,
        size_scale,
        // v4 Dots FADE_DURATION = 3.0 seconds per-particle fade-in.
        fade_duration: 3.0,
        // v4 constrainToBox = false: effectively infinite bounds so the
        // OOB→home teleport never fires. Dots grid dots should only return
        // home via the stationary spring, not via a hard position reset.
        constrain_min: [-1e9, -1e9],
        constrain_max: [1e9, 1e9],
        // Attract-mode gate and fraction: off for D2 (D6 wires the screensaver).
        attract_gate: 0,
        attract_fraction: 1.0,
        // Turbulence: off for D2.
        turbulence_amp: 0.0,
        turbulence_scale: 0.0,
        turbulence_time: 0.0,
        // v4 STATIONARY_CONSTANT = 0.01. Each particle is pulled toward its
        // original_xy home. Line passes 0.0 (provable no-op); Dots needs 0.01
        // so the grid stays anchored and returns home after interaction.
        stationary_constant: 0.01,
        attractors,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use std::time::Duration;

    #[test]
    #[allow(
        clippy::expect_used,
        reason = "test-only: panic on system-run failure is the intended failure mode"
    )]
    #[allow(
        clippy::float_cmp,
        reason = "turbulence_amp is written as literal 0.0 — bit-exact zero comparison is correct"
    )]
    fn update_dots_sim_params_writes_expected_values() {
        let mut world = World::new();
        world.insert_resource(ParticleSimParams {
            params: SimParams::default(),
            particles_handle: Handle::default(),
            particle_count: 0,
        });
        let mut time = Time::<()>::default();
        time.advance_by(Duration::from_millis(16));
        world.insert_resource(time);
        // Window::default() gives 1280 × 720.
        world.spawn(Window::default());

        world
            .run_system_once(update_dots_sim_params)
            .expect("update_dots_sim_params run");

        let sim = world.resource::<ParticleSimParams>();
        let params = &sim.params;

        assert!(
            (params.stationary_constant - 0.01).abs() < 1e-6,
            "stationary_constant must be 0.01, got {}",
            params.stationary_constant
        );
        assert!(
            (params.fade_duration - 3.0).abs() < 1e-6,
            "fade_duration must be 3.0, got {}",
            params.fade_duration
        );
        assert_eq!(
            params.attractor_count, 0,
            "D2 has no attractors; count must be 0"
        );
        assert!(
            params.constrain_max[0] >= 1e8,
            "constrain_max must be huge (no OOB reset), got {}",
            params.constrain_max[0]
        );
        assert!(
            params.constrain_min[0] <= -1e8,
            "constrain_min must be huge-negative (no OOB reset), got {}",
            params.constrain_min[0]
        );
        assert_eq!(params.attract_gate, 0, "attract gate must be off in D2");
        assert_eq!(
            params.turbulence_amp, 0.0,
            "turbulence must be off in D2"
        );
    }

    #[test]
    fn drag_baking_produces_valid_multipliers() {
        // Baked drag must be in (0, 1): 1.0 means no drag (wrong), 0.0 means
        // instant full stop (wrong). Any physical drag is strictly in between.
        let pulling = V4_DOTS_PULLING_DRAG.powf(V4_FIXED_DT_DOTS);
        let inertial = V4_DOTS_INERTIAL_DRAG.powf(V4_FIXED_DT_DOTS);

        assert!(
            pulling > 0.0 && pulling < 1.0,
            "pulling drag baked = {pulling} must be in (0, 1)"
        );
        assert!(
            inertial > 0.0 && inertial < 1.0,
            "inertial drag baked = {inertial} must be in (0, 1)"
        );
        // Inertial drag is stronger (closer to 0) than pulling drag.
        assert!(
            inertial < pulling,
            "inertial drag {inertial} must be stronger than pulling drag {pulling}"
        );
    }

    #[test]
    fn size_scale_is_at_most_one() {
        // size_scale = min(2^(w/836 - 1), 1) ≤ 1 for all positive widths.
        // It reaches 1.0 at w = 836 px and is capped there for wider windows.
        for w in [400.0_f32, 836.0, 1280.0, 1920.0, 3840.0] {
            let scale = (2.0_f32.powf(w / 836.0 - 1.0)).min(1.0);
            assert!(
                scale > 0.0 && scale <= 1.0,
                "size_scale {scale} out of (0, 1] at width {w}"
            );
        }
    }
}
