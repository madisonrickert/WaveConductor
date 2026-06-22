//! Dots sketch ("Fabric") — a grid of dots that ripple and deform under
//! pointer and hand interaction.
//!
//! ## Data flow
//!
//! 1. `OnEnter(AppState::Dots)` runs [`systems::spawn_dots`]: allocates the
//!    particle storage buffer (full-screen grid), spawns the render entity under
//!    [`systems::DotsRoot`], installs
//!    [`crate::particles::compute::ParticleSimParams`], and seeds
//!    [`crate::particles::sim_cpu::CpuMirror`] with the initial grid layout.
//! 2. Every `Update` while `sketch_active(AppState::Dots)` is true:
//!    - a. [`systems::update_dots_sim_params`] writes the current
//!      [`DotsSettings`] values into `ParticleSimParams` (drag, stationary
//!      spring, size scale — no attractor in D2).
//! 3. The render world extracts `ParticleSimParams` and dispatches the compute
//!    pipeline (`assets/shaders/particles/simulate.wgsl`), which updates the
//!    storage buffer in place.
//! 4. Bevy's 2D render path consumes the same buffer through
//!    [`crate::particles::material::ParticleMaterial`] and draws one quad per
//!    particle via the vertex-index-driven
//!    `assets/shaders/particles/render.wgsl`.
//! 5. `OnExit(AppState::Dots)` runs `despawn_with::<DotsRoot>` and
//!    `remove_dots_sim_params` to free the entity tree, drop
//!    `ParticleSimParams` (releases the GPU buffer ref-count), and drop
//!    `CpuMirror` (frees the per-particle `Vec`).
//!
//! Mouse and hand interaction will be wired in Task 3.
//!
//! The shared [`crate::particles::compute::ParticleComputePlugin`] and
//! [`crate::particles::material::ParticleMaterial`] are registered once by
//! the [`crate::SketchesPlugin`] umbrella, not here.

pub mod settings;
pub mod systems;

pub use systems::DotsRoot;

use bevy::prelude::*;
use wc_core::lifecycle::state::AppState;
use wc_core::settings::RegisterSketchSettingsExt;
use wc_core::sketch::{despawn_with, sketch_active, RegisterSketchManifestExt};

/// Plugin that registers the Dots (Fabric) sketch.
pub struct DotsPlugin;

impl Plugin for DotsPlugin {
    fn build(&self, app: &mut App) {
        // Register DotsSettings with the settings system (panel + persistence).
        app.register_sketch_settings::<settings::DotsSettings>();

        // Register the picker-tile manifest entry (async screenshot load).
        register_dots_manifest(app);

        // Lifecycle: spawn the grid on enter, despawn and release VRAM on exit.
        app.add_systems(
            OnEnter(AppState::Dots),
            systems::spawn_dots,
        );
        app.add_systems(
            OnExit(AppState::Dots),
            (despawn_with::<DotsRoot>, remove_dots_sim_params),
        );

        // Per-frame: write updated sim params while the Dots sketch is active.
        app.add_systems(
            Update,
            systems::update_dots_sim_params.run_if(sketch_active(AppState::Dots)),
        );

        // TODO(Task 3): wire mouse and hand interaction systems.
    }
}

/// Register Dots's picker-tile metadata into [`wc_core::sketch::SketchManifest`].
///
/// Factored out of [`DotsPlugin::build`] so it is independently unit-testable
/// without `DotsPlugin`'s rendering dependencies (the shared
/// `ParticleComputePlugin` and `Material2dPlugin::<ParticleMaterial>` both
/// require a full `RenderApp` that `MinimalPlugins` does not provide).
///
/// The `AssetServer` load is async; the picker renders the tile as soon as
/// the image asset finishes loading. Before then the tile shows the dark
/// placeholder fill defined in `OverlayStyle`. This mirrors the behavior of
/// [`crate::line::register_line_manifest`].
pub(crate) fn register_dots_manifest(app: &mut App) {
    let asset_server = app.world().resource::<AssetServer>();
    // Load the picker-tile screenshot as PNG. Bevy's default features include
    // the `png` image loader; JPEG requires the separate `bevy/jpeg` feature
    // which is not enabled in this workspace.
    // v4 calls this sketch "Fabric" in HomePage.tsx.
    let screenshot = asset_server.load("sketches/dots/screenshot.png");
    app.register_sketch_manifest(wc_core::sketch::SketchManifestEntry {
        state: AppState::Dots,
        display_name: "Fabric",
        screenshot,
    });
}

/// `OnExit(AppState::Dots)` companion to [`systems::spawn_dots`].
///
/// Drops `ParticleSimParams` so its `Handle<ShaderBuffer>` clone is freed and
/// the GPU storage buffer's ref-count reaches zero, releasing VRAM on each
/// Enter/Exit cycle. Also drops `CpuMirror` so its per-particle `Vec` is
/// freed; `spawn_dots` re-inserts a fresh snapshot on the next `OnEnter`.
fn remove_dots_sim_params(mut commands: Commands<'_, '_>) {
    commands.remove_resource::<crate::particles::compute::ParticleSimParams>();
    commands.remove_resource::<crate::particles::sim_cpu::CpuMirror>();
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions — panicking on unexpected None is the correct behaviour"
)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use wc_core::sketch::SketchManifest;

    /// Verifies that `register_dots_manifest` appends an entry for
    /// `AppState::Dots` with the correct display name.
    ///
    /// Uses the free-function path rather than constructing the full
    /// `DotsPlugin` because `DotsPlugin::build` adds rendering plugins that
    /// require a real `RenderApp` — unavailable in headless unit tests.
    /// Mirrors `register_line_manifest_appends_entry` in `crate::line::tests`.
    #[test]
    fn register_dots_manifest_appends_entry() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::asset::AssetPlugin::default());
        // `ImagePlugin` registers `Image` as an asset type so `AssetServer`
        // can allocate a `Handle<Image>` for the screenshot path.
        app.add_plugins(bevy::image::ImagePlugin::default());
        register_dots_manifest(&mut app);
        let manifest = app.world().resource::<SketchManifest>();
        let entry = manifest
            .get(AppState::Dots)
            .expect("Dots manifest entry should be registered");
        assert_eq!(entry.display_name, "Fabric");
    }

    /// `remove_dots_sim_params` must drop `ParticleSimParams` and `CpuMirror`
    /// on Dots exit so VRAM and CPU memory are released.
    #[test]
    fn remove_dots_sim_params_drops_resources() {
        use crate::particles::compute::ParticleSimParams;
        use crate::particles::particle::SimParams;
        use crate::particles::sim_cpu::CpuMirror;

        let mut world = World::new();
        world.insert_resource(ParticleSimParams {
            params: SimParams::default(),
            particles_handle: Handle::default(),
            particle_count: 0,
        });
        world.insert_resource(CpuMirror { particles: vec![] });

        world
            .run_system_once(remove_dots_sim_params)
            .expect("remove_dots_sim_params run");

        assert!(
            world.get_resource::<ParticleSimParams>().is_none(),
            "ParticleSimParams must be removed on Dots exit"
        );
        assert!(
            world.get_resource::<CpuMirror>().is_none(),
            "CpuMirror must be removed on Dots exit"
        );
    }
}
