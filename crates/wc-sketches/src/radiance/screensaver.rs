//! Radiance attract-mode performer.
//!
//! Two drivers, both gated `in_screensaver(AppState::Radiance)` (zero systems
//! otherwise — AGENTS.md "zero systems when idle"), running under the
//! established thermal present-rate throttle:
//!
//! - [`drive_phantom`]: an analytic SDF silhouette (the synthetic module's
//!   drifting ellipse cluster) writes a synthetic mask + edge list through
//!   the SAME `MaskTexture` / `SilhouetteEdges` resources the real tracker
//!   uses, so the particle kernel and silhouette material are unchanged.
//!   Rasterization is rate-limited to [`PHANTOM_REGEN_HZ`]; between regens
//!   the phantom costs one accumulator add.
//! - [`drive_radiance_attract_sim`]: the screensaver's [`bake_radiance_sim`]
//!   writer (one baker, two writers — flame's Condition A1). No audio (the
//!   attract mode is not audio-reactive: it bakes the neutral frame), no
//!   impulses, and ember overrides scale emission/flow/buoyancy down on the
//!   `ScreensaverFade` envelope so sleep and wake ease symmetrically. The
//!   ember *palette* blend lives in `render::drive_radiance_materials`
//!   (already gated `in_state`, so it runs through the screensaver).
//!
//! During the screensaver the camera stays at Plan B's detector-only idle
//! rate (the activity sync set `idle_throttle`), so a person walking up
//! resets the `InteractionTimer` and wakes the sketch; the worker's mask
//! writes resume only once a person is actually present, which is also the
//! moment the phantom stops running.

use bevy::prelude::*;
use wc_core::input::body::{MaskTexture, SilhouetteEdges};
use wc_core::lifecycle::screensaver::fade::ScreensaverFade;
use wc_core::lifecycle::screensaver::in_screensaver;
use wc_core::lifecycle::state::AppState;

use crate::radiance::compute::sim_params::RadianceSimParams;
use crate::radiance::settings::RadianceSettings;
use crate::radiance::synthetic::{extract_edges, phantom_pose, rasterize_mask};
use crate::radiance::systems::sim_params::{bake_radiance_sim, neutral_audio, RadianceState};

/// Phantom mask regeneration rate. 12 Hz reads as continuous drift at the
/// screensaver's throttled present rates while keeping the 256² rasterize
/// well under the thermal budget.
pub const PHANTOM_REGEN_HZ: f32 = 12.0;
/// Fraction of live emission at full fade (the "low particle count" of the
/// spec's compute-lite attract mode — fewer births, thinner aura).
pub const EMBER_EMISSION_FRACTION: f32 = 0.25;
/// Flow-strength multiplier at full fade (slow drift).
pub const EMBER_FLOW_FRACTION: f32 = 0.4;
/// Buoyancy multiplier at full fade.
pub const EMBER_BUOYANCY_FRACTION: f32 = 0.6;
/// Phantom time scale: the pose clock runs slower than wall time.
pub const PHANTOM_TIME_SCALE: f32 = 0.6;

/// Phantom driver state: pose clock + regen accumulator.
#[derive(Resource, Default)]
pub struct PhantomClock {
    /// Seconds of screensaver time (drives the pose).
    pub elapsed: f32,
    /// Seconds since the last mask regen.
    pub since_regen: f32,
}

/// Plugin wiring the Radiance attract performer.
pub struct RadianceScreensaverPlugin;

impl Plugin for RadianceScreensaverPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PhantomClock>();
        app.add_systems(
            Update,
            (drive_phantom, drive_radiance_attract_sim)
                .chain()
                .run_if(in_screensaver(AppState::Radiance)),
        );
    }
}

/// `Update` (`in_screensaver(AppState::Radiance)`): advance the phantom and,
/// at [`PHANTOM_REGEN_HZ`], rewrite the shared mask + edge list in place (no
/// allocation: the image bytes and the edge Vec are reused).
pub fn drive_phantom(
    time: Res<'_, Time>,
    mut clock: ResMut<'_, PhantomClock>,
    mask: Option<Res<'_, MaskTexture>>,
    mut images: ResMut<'_, Assets<Image>>,
    edges: Option<ResMut<'_, SilhouetteEdges>>,
) {
    clock.elapsed += time.delta_secs();
    clock.since_regen += time.delta_secs();
    if clock.since_regen < 1.0 / PHANTOM_REGEN_HZ {
        return;
    }
    clock.since_regen = 0.0;

    let (Some(mask), Some(mut edges)) = (mask, edges) else {
        return; // surfaces absent (headless harness): nothing to draw into
    };
    let pose = phantom_pose(clock.elapsed * PHANTOM_TIME_SCALE);
    if let Some(mut image) = images.get_mut(&mask.0) {
        if let Some(data) = image.data.as_mut() {
            rasterize_mask(&pose, data);
            extract_edges(data, &mut edges.points);
            edges.generation = edges.generation.wrapping_add(1);
        }
    }
}

/// `Update` (`in_screensaver(AppState::Radiance)`, after [`drive_phantom`]):
/// bake the neutral-audio, no-body frame, then apply the ember overrides on
/// the fade envelope.
pub fn drive_radiance_attract_sim(
    time: Res<'_, Time>,
    window: Single<'_, '_, &Window>,
    settings: Res<'_, RadianceSettings>,
    fade: Res<'_, ScreensaverFade>,
    edges: Option<Res<'_, SilhouetteEdges>>,
    mut state: ResMut<'_, RadianceState>,
    mut sim: ResMut<'_, RadianceSimParams>,
) {
    let edge_count = edges.map_or(0, |e| e.points.len());
    let window_size = Vec2::new(window.width(), window.height());
    let quiet = neutral_audio();
    bake_radiance_sim(
        &settings,
        &quiet,
        None,
        edge_count,
        window_size,
        time.delta_secs(),
        time.elapsed_secs(),
        &mut state,
        &mut sim.params,
    );
    // Ember overrides ride the fade in both directions, so the decay into
    // the ember and the roar-back on wake are symmetric.
    let a = fade.alpha().clamp(0.0, 1.0);
    sim.params.emission_prob *= 1.0 - a * (1.0 - EMBER_EMISSION_FRACTION);
    sim.params.flow_strength *= 1.0 - a * (1.0 - EMBER_FLOW_FRACTION);
    sim.params.buoyancy *= 1.0 - a * (1.0 - EMBER_BUOYANCY_FRACTION);
    sim.params.burst_speed = 0.0;
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use bevy::asset::AssetPlugin;
    use bevy::ecs::system::RunSystemOnce;
    use bytemuck::Zeroable;
    use std::time::Duration;

    use crate::radiance::compute::sim_params::RadianceSimParamsGpu;
    use crate::radiance::systems::spawn::ensure_body_surfaces;

    fn phantom_app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        app.init_asset::<Image>();
        app.insert_resource(RadianceSettings::default());
        app.init_resource::<PhantomClock>();
        let mut time = Time::<()>::default();
        time.advance_by(Duration::from_millis(200)); // past the regen period
        app.insert_resource(time);
        app.world_mut()
            .run_system_once(ensure_body_surfaces)
            .expect("surfaces");
        app
    }

    /// The phantom writes a real mask and a fresh edge list, bumping the
    /// generation so the GPU upload path sees it.
    #[test]
    fn phantom_writes_mask_and_edges() {
        let mut app = phantom_app();
        let gen_before = app.world().resource::<SilhouetteEdges>().generation;
        app.world_mut()
            .run_system_once(drive_phantom)
            .expect("phantom runs");
        let edges = app.world().resource::<SilhouetteEdges>();
        assert!(edges.generation != gen_before, "generation bumped");
        assert!(!edges.points.is_empty(), "phantom has a rim");
        let mask = app.world().resource::<MaskTexture>().0.clone();
        let images = app.world().resource::<Assets<Image>>();
        let data = images
            .get(&mask)
            .and_then(|i| i.data.as_ref())
            .expect("mask bytes");
        assert!(data.iter().any(|&v| v > 128), "phantom body rasterized");
    }

    /// At full fade the attract writer emits less, flows slower, and rises
    /// less than the live bake; burst is zeroed.
    #[test]
    fn attract_writer_applies_ember_overrides() {
        let mut world = World::new();
        world.insert_resource(RadianceSettings::default());
        world.insert_resource(RadianceState::default());
        world.insert_resource(RadianceSimParams {
            params: RadianceSimParamsGpu::zeroed(),
            particles: Handle::default(),
            particle_count: 1_000,
        });
        world.insert_resource(SilhouetteEdges {
            points: Vec::with_capacity(8),
            generation: 1,
        });
        let mut fade = ScreensaverFade::default();
        fade.set_target(1.0);
        let fade = fade.advanced(Duration::from_secs(10));
        world.insert_resource(fade);
        world.insert_resource(Time::<()>::default());
        world.spawn(Window::default());

        world
            .run_system_once(drive_radiance_attract_sim)
            .expect("attract writer runs");
        let sim = world.resource::<RadianceSimParams>();
        // Live neutral value: rate(0.5) * EMISSION_BASE_HZ * dt; ember cuts
        // it to the fraction.
        let live = 0.5 * crate::radiance::systems::sim_params::EMISSION_BASE_HZ * sim.params.dt;
        assert!(
            (sim.params.emission_prob - live * EMBER_EMISSION_FRACTION).abs() < 1e-6,
            "ember emission: {} vs live {live}",
            sim.params.emission_prob
        );
        assert!(sim.params.burst_speed.abs() < f32::EPSILON);
        assert!(sim.params.impulse_count == 0, "no body in attract mode");
    }
}
