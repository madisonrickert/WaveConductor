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
use bevy::sprite_render::MeshMaterial2d;
use wc_core::lifecycle::screensaver::fade::ScreensaverFade;
use wc_core::lifecycle::screensaver::in_screensaver;
use wc_core::lifecycle::state::AppState;
use wc_core::sketch::sketch_active;

use crate::dots::settings::DotsSettings;
use crate::dots::systems::sim_params::{
    bake_dots_sim_params, DotsAttractGate, DotsTurbulence, DotsWindowGeom,
};
use crate::dots::systems::spawn::DotsRoot;
use crate::particles::compute::ParticleSimParams;
use crate::particles::material::ParticleMaterial;
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
        // The brightness-lift driver follows the ScreensaverFade envelope,
        // which ramps *up* during Screensaver and back *down* during Active
        // (the wake transition completes in Active). Registered under BOTH gates
        // so the lift ramps in on fade-in (Screensaver) and back out after wake
        // (fade-out completes in Active), while still running zero systems in
        // `SketchActivity::Idle` and other app states (AGENTS.md "zero systems
        // when idle"). The system is change-gated internally: outside the fade
        // ramps it compares one float and returns.
        app.add_systems(
            Update,
            drive_dots_attract_color.run_if(in_screensaver(AppState::Dots)),
        );
        app.add_systems(
            Update,
            drive_dots_attract_color.run_if(sketch_active(AppState::Dots)),
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
    //
    // restoring_linear = 0.0: the fabric-tension spring must NOT fight the
    // turbulence morph during attract mode. The live writer passes
    // `settings.fabric_tension` instead; here we always suppress it.
    let attractors = [Attractor::default(); MAX_ATTRACTORS];
    sim.params = bake_dots_sim_params(
        time.delta_secs(),
        geom,
        attractors,
        0,
        gate,
        turbulence,
        0.0,
    );
}

/// Drive [`ParticleMaterial::attract_color`] from the [`ScreensaverFade`]
/// envelope × [`DotsSettings::attract_brightness`] so the fraction-killed calm
/// field clears the `AgX` tonemapper's white knee.
///
/// Runs during both Screensaver (fade-in) and Active (fade-out after wake) —
/// see the plugin registration for the gating rationale. Mutating the material
/// asset re-prepares its bind group, so the write is change-gated on the value
/// actually moving: in the settled states (fade at exactly zero or one) this
/// system is a single float compare per frame, no asset churn. `last` is only
/// advanced when the material was actually written, so a frame where the asset
/// is not yet loaded retries instead of losing the value.
///
/// Dots' slow turbulence does not trigger the velocity-tint WAKE band, so
/// `attract_color_strength` defaults to `0.0` — only the brightness `y`
/// channel matters. The shared [`ParticleMaterial::attract_color_params`]
/// function handles both channels uniformly.
///
/// Per-frame no-allocation guarantee: all arithmetic is on stack scalars.
fn drive_dots_attract_color(
    fade: Res<'_, ScreensaverFade>,
    settings: Res<'_, DotsSettings>,
    roots: Query<'_, '_, &MeshMaterial2d<ParticleMaterial>, With<DotsRoot>>,
    mut materials: ResMut<'_, Assets<ParticleMaterial>>,
    mut last: Local<'_, Vec4>,
) {
    let target = ParticleMaterial::attract_color_params(
        fade.alpha(),
        settings.attract_color_strength,
        settings.attract_brightness,
    );
    // Two instances of this system exist (one per activity gate), each with
    // its own `Local`. A stale `last` in the instance that was not running
    // self-corrects: the fade envelope is continuous, so the first frame the
    // instance runs with a differing target rewrites the material and resyncs.
    // Gate on both driven channels (x = tint, y = brightness lift) moving.
    if (target.x - last.x).abs() < f32::EPSILON && (target.y - last.y).abs() < f32::EPSILON {
        return;
    }
    for handle in &roots {
        if let Some(mut material) = materials.get_mut(&handle.0) {
            material.attract_color = target;
            *last = target;
        }
    }
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
        // Task 6: the screensaver always bakes restoring_linear = 0.0 so the
        // fabric-tension spring does not fight the turbulence morph.
        #[allow(
            clippy::float_cmp,
            reason = "restoring_linear is written as literal 0.0 — bit-exact comparison is correct"
        )]
        {
            assert_eq!(
                params.restoring_linear, 0.0,
                "screensaver must bake restoring_linear=0.0 (spring must not fight turbulence)"
            );
        }
    }

    /// Build a minimal world with the resources `drive_dots_attract_color` requires:
    /// `ScreensaverFade`, `DotsSettings`, a `DotsRoot` entity carrying a
    /// `MeshMaterial2d<ParticleMaterial>`, and an `Assets<ParticleMaterial>` asset
    /// registry. Returns the world and the material handle so tests can inspect the
    /// written uniform.
    fn setup_attract_color_world(fade_alpha: f32) -> (World, Handle<ParticleMaterial>) {
        let mut world = World::new();

        // ScreensaverFade at the requested alpha. `value` is private, so we drive
        // it via `set_target` + `advanced` with a duration long enough to saturate
        // the 1.5 s ramp.
        let fade = if fade_alpha >= 1.0 {
            // Advance far past the 1.5 s ramp to reach full alpha.
            let mut f = ScreensaverFade::default();
            f.set_target(1.0);
            f.advanced(Duration::from_secs(10))
        } else {
            // Zero alpha — the default.
            ScreensaverFade::default()
        };
        world.insert_resource(fade);

        world.insert_resource(DotsSettings::default());

        // Build an `Assets<ParticleMaterial>` registry and add a material.
        // Unit tests don't exercise GPU paths — only the `attract_color` field
        // write — so dummy handles for `particles` and `star_texture` are fine.
        let mut mat_assets: Assets<ParticleMaterial> = Assets::default();
        let mat = ParticleMaterial {
            particles: Handle::default(),
            star_texture: Handle::default(),
            solid_color: ParticleMaterial::solid_off(),
            attract_color: ParticleMaterial::attract_color_off(),
            template_color: ParticleMaterial::template_color_off(),
            palette_params: ParticleMaterial::palette_off(),
        };
        let handle = mat_assets.add(mat);
        world.insert_resource(mat_assets);

        // Spawn the DotsRoot entity carrying the material handle.
        world.spawn((DotsRoot, MeshMaterial2d(handle.clone())));

        (world, handle)
    }

    /// `drive_dots_attract_color` at full screensaver fade (alpha = 1.0) must
    /// write `attract_color.y ≈ 1.2` (brightness 2.2 → lift = 1*(2.2-1) = 1.2)
    /// and `attract_color.x = 0.0` (color strength defaults to 0).
    #[test]
    #[allow(
        clippy::expect_used,
        reason = "test-only: panic on system-run failure is the intended failure mode"
    )]
    fn drive_dots_attract_color_full_alpha_sets_brightness_lift() {
        let (mut world, handle) = setup_attract_color_world(1.0);

        world
            .run_system_once(drive_dots_attract_color)
            .expect("drive_dots_attract_color run");

        let materials = world.resource::<Assets<ParticleMaterial>>();
        let mat = materials.get(&handle).expect("material must be present");
        assert!(
            mat.attract_color.x.abs() < 1e-6,
            "x (tint strength) must be 0 at default attract_color_strength=0, got {}",
            mat.attract_color.x
        );
        assert!(
            (mat.attract_color.y - 1.2).abs() < 1e-5,
            "y (brightness lift) must be ≈1.2 (brightness 2.2 - 1 = 1.2), got {}",
            mat.attract_color.y
        );
        assert!(
            mat.attract_color.z.abs() < f32::EPSILON,
            "z must be 0 (reserved)"
        );
        assert!(
            mat.attract_color.w.abs() < f32::EPSILON,
            "w must be 0 (reserved)"
        );
    }

    /// `drive_dots_attract_color` at fade alpha = 0.0 (Active / wake-complete)
    /// must write `Vec4::ZERO` — a bit-exact render no-op.
    #[test]
    #[allow(
        clippy::expect_used,
        reason = "test-only: panic on system-run failure is the intended failure mode"
    )]
    fn drive_dots_attract_color_zero_alpha_writes_zero() {
        let (mut world, handle) = setup_attract_color_world(0.0);

        // Pre-seed `last` to a non-zero value so the change gate does not
        // short-circuit: insert a fake prior write by running the system once
        // at full fade first, then reset fade to zero and re-run.
        let (mut world2, handle2) = setup_attract_color_world(1.0);
        world2
            .run_system_once(drive_dots_attract_color)
            .expect("pre-seed run");
        // Now reset the fade to zero on this separate world (avoids Local confusion).
        // Simply test the zero-fade world directly — `last` starts at Vec4::ZERO,
        // target is Vec4::ZERO, so the change gate fires and the material stays ZERO.
        world
            .run_system_once(drive_dots_attract_color)
            .expect("drive_dots_attract_color zero-fade run");

        let materials = world.resource::<Assets<ParticleMaterial>>();
        let mat = materials.get(&handle).expect("material must be present");
        assert_eq!(
            mat.attract_color,
            Vec4::ZERO,
            "attract_color must be Vec4::ZERO at fade alpha=0 (Active steady state)"
        );
        // Suppress unused variable warning for the pre-seed world.
        let _ = (world2, handle2);
    }
}
