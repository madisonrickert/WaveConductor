//! Dots attract-mode driver (Plan D6a, Task 2).
//!
//! Drives the Dots particle grid with curl-noise turbulence while the
//! screensaver is showing, so the idle grid slowly morphs using the shared
//! kernel's fraction-kill + lifetime-respawn self-heal. This is a v5 kiosk
//! addition (v4 Dots had no screensaver attract mode). The single system here
//! is gated on `in_screensaver(AppState::Dots)` so it runs only during Dots'
//! attract mode and nowhere else (AGENTS.md "zero systems when idle").
//!
//! ## What it writes
//!
//! **`ParticleSimParams.params`** — the normal `update_dots_sim_params` writer
//! is gated on `sketch_active(AppState::Dots)` and does not run here, so this
//! is the param *producer* during attract. It supplies the noise-turbulence
//! parameters for the continuous slow-morph drift, baking everything via the
//! shared [`crate::dots::systems::sim_params::bake_dots_sim_params`] baker
//! (Condition A1 — one baker, two writers, cannot drift).
//!
//! ## Why Dots' driver is simpler than Line's
//!
//! Line's screensaver also updates `LinePostParams` (the gravity-smear
//! uniforms) and drives a "wandering pulse" choreography array of three
//! Lissajous attractors. Dots has neither: there is no gravity-smear post
//! pass, and the turbulence morph is the sole motion. The attractor array is
//! always empty (`count = 0`) — the kernel sits in inertial-drag mode while
//! the curl-noise drift slowly morphs the grid.

use bevy::prelude::*;
use wc_core::lifecycle::screensaver::in_screensaver;
use wc_core::lifecycle::state::AppState;

use crate::dots::settings::DotsSettings;
use crate::dots::systems::sim_params::{
    bake_dots_sim_params, DotsAttractGate, DotsTurbulence, DotsWindowGeom,
};
use crate::particles::compute::ParticleSimParams;
use crate::particles::particle::{Attractor, MAX_ATTRACTORS};

/// Spatial frequency (radians per world unit) of the turbulence flow's base
/// octave. Matches Line's `TURBULENCE_SCALE` (~520-px primary swirl wavelength)
/// for broad, slow swirls rather than a busy pattern.
const DOTS_TURBULENCE_SCALE: f32 = 0.012;

/// Plugin wiring the Dots attract driver.
pub struct DotsScreensaverPlugin;

impl Plugin for DotsScreensaverPlugin {
    fn build(&self, app: &mut App) {
        // The driver runs only while Dots' screensaver is showing — zero work
        // in every other state (AGENTS.md "zero systems when idle").
        app.add_systems(
            Update,
            drive_dots_attract.run_if(in_screensaver(AppState::Dots)),
        );
    }
}

/// Drive `ParticleSimParams.params` from curl-noise turbulence while Dots'
/// screensaver is showing.
///
/// Builds the window geometry, enables the fraction-kill + lifetime-respawn
/// gate, and supplies the turbulence parameters for the continuous slow-morph
/// drift. The attractor array is empty (`count = 0`) — Dots' screensaver has
/// no wandering pulses; the turbulence is the sole motion. All params are
/// baked via the shared [`bake_dots_sim_params`] baker (Condition A1 — one
/// baker, two writers, cannot drift).
///
/// Per-frame no-allocation guarantee: all arithmetic is on stack scalars; the
/// attractor array is a zero-initialized stack value.
fn drive_dots_attract(
    time: Res<'_, Time>,
    settings: Res<'_, DotsSettings>,
    window: Single<'_, '_, &Window>,
    mut sim: ResMut<'_, ParticleSimParams>,
) {
    let geom = DotsWindowGeom::from_window(&window);
    // Attract gate: enables the kernel's fraction kill (spatially uniform
    // thinning by per-index spawn hash) and per-particle lifetime respawn
    // (the field self-heals toward each particle's home on staggered
    // lifespans). Note: Dots bakes `stationary_constant = 0.01` in both paths
    // (for live parity), so the kernel's idle home-drift also runs here with
    // `attractor_count == 0` — under turbulence each `original_xy` eases along
    // the flow, so respawns return to a slowly-drifting home rather than the
    // literal spawn grid. Whether the grid holds its layout over a multi-hour
    // idle is a soak-watch item; the lever (if it drifts undesirably) is to
    // thread a softened `stationary_constant` into the attract bake.
    let gate = DotsAttractGate {
        enabled: true,
        fraction: settings.attract_particle_fraction,
    };
    // Turbulence: the screensaver's primary motion. The curl-noise flow
    // slowly morphs the grid; `time` scrolls the flow over elapsed wall-clock.
    let turbulence = DotsTurbulence {
        amp: settings.attract_turbulence,
        scale: DOTS_TURBULENCE_SCALE,
        time: time.elapsed_secs(),
    };
    // Empty attractor array — Dots' screensaver has no wandering pulses.
    // `count = 0` puts the kernel in inertial-drag mode; the turbulence drift
    // is the only force moving particles.
    let attractors = [Attractor::default(); MAX_ATTRACTORS];
    sim.params = bake_dots_sim_params(time.delta_secs(), geom, attractors, 0, gate, turbulence);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::particles::particle::SimParams;
    use bevy::ecs::system::RunSystemOnce;
    use std::time::Duration;

    /// Build a minimal world with the resources `drive_dots_attract` requires.
    fn setup_world() -> World {
        let mut world = World::new();
        world.insert_resource(ParticleSimParams {
            params: SimParams::default(),
            particles_handle: Handle::default(),
            particle_count: 0,
        });
        world.insert_resource(DotsSettings::default());
        let mut time = Time::<()>::default();
        time.advance_by(Duration::from_millis(16));
        world.insert_resource(time);
        // Window::default() gives 1280 × 720.
        world.spawn(Window::default());
        world
    }

    /// `drive_dots_attract` must enable the attract gate (`attract_gate == 1`),
    /// pass `attract_particle_fraction` and `attract_turbulence` from settings
    /// through verbatim, and write `attractor_count = 0` (Dots' screensaver
    /// has no wandering pulses — the turbulence is the sole motion).
    #[test]
    #[allow(
        clippy::expect_used,
        reason = "test-only: panic on system-run failure is the intended failure mode"
    )]
    fn drive_dots_attract_sets_gate_and_turbulence_with_zero_attractors() {
        let mut world = setup_world();
        // Capture the settings values before running the system (the run borrows the world).
        let (expected_fraction, expected_turb) = {
            let settings = world.resource::<DotsSettings>();
            (
                settings.attract_particle_fraction,
                settings.attract_turbulence,
            )
        };

        world
            .run_system_once(drive_dots_attract)
            .expect("drive_dots_attract run");

        let sim = world.resource::<ParticleSimParams>();
        let params = &sim.params;

        assert_eq!(params.attract_gate, 1, "attract gate must be enabled (1)");
        assert!(
            (params.attract_fraction - expected_fraction).abs() < 1e-6,
            "attract_fraction must match settings.attract_particle_fraction ({expected_fraction}), got {}",
            params.attract_fraction
        );
        assert!(
            (params.turbulence_amp - expected_turb).abs() < 1e-6,
            "turbulence_amp must match settings.attract_turbulence ({expected_turb}), got {}",
            params.turbulence_amp
        );
        assert_eq!(
            params.attractor_count, 0,
            "attractor_count must be 0 (Dots screensaver has no wandering pulses)"
        );
    }
}
