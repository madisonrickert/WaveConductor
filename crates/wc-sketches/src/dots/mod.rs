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
//!    - b. [`systems::update_dots_post_params`] writes [`post_process::DotsPostParams`]
//!      from the live cursor (v4 UV convention), window resolution, and
//!      [`settings::DotsSettings::gamma`].
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
//! Mouse interaction (pointer/touch attractor) is wired in
//! [`systems::update_dots_mouse_attractor`] / [`systems::decay_dots_mouse_attractor`]
//! (Task 3). Hand attractors are wired in Plan D3.
//!
//! The shared [`crate::particles::compute::ParticleComputePlugin`] and
//! [`crate::particles::material::ParticleMaterial`] are registered once by
//! the [`crate::SketchesPlugin`] umbrella, not here.

pub mod post_process;
pub mod settings;
pub mod systems;

pub use systems::DotsRoot;

use bevy::prelude::*;
use wc_core::lifecycle::state::AppState;
use wc_core::lifecycle::RegisterIdleVetoExt;
use wc_core::settings::RegisterSketchSettingsExt;
use wc_core::sketch::{despawn_with, sketch_active, RegisterSketchManifestExt};

/// Plugin that registers the Dots (Fabric) sketch.
pub struct DotsPlugin;

impl Plugin for DotsPlugin {
    fn build(&self, app: &mut App) {
        // Register DotsSettings with the settings system (panel + persistence).
        app.register_sketch_settings::<settings::DotsSettings>();

        // Explode post-process: render node + uniform extract.
        app.add_plugins(post_process::DotsPostProcessPlugin);

        // Register the picker-tile manifest entry (async screenshot load).
        register_dots_manifest(app);

        // Lifecycle: spawn the grid on enter, despawn and release VRAM on exit.
        app.add_systems(
            OnEnter(AppState::Dots),
            (systems::spawn_dots, insert_dots_post_params).chain(),
        );
        app.add_systems(
            OnExit(AppState::Dots),
            (despawn_with::<DotsRoot>, remove_dots_sim_params),
        );

        // Mouse attractor state (persists across frames; updated each frame in
        // the `Update` chain below). The idle veto below keeps Dots `Active`
        // while the attractor has non-zero power so the decay system continues
        // to fire until the pull fully releases.
        app.init_resource::<systems::DotsMouseAttractorState>();
        // Register an idle veto that keeps Dots `Active` while the mouse
        // attractor's power is still decaying — otherwise the sketch would
        // transition to `Idle` mid-decay and the (gated) decay system would
        // never finish releasing the pull.
        app.register_idle_veto(dots_idle_veto);

        // Per-frame: update mouse state, decay the attractor, then write sim
        // params (sim-params reads the mouse state, so ordering is required).
        // All four systems run inside the `sketch_active` gate so they do not
        // execute while Dots is idle.
        //
        // `update_dots_post_params` writes DotsPostParams (cursor → UV, window
        // resolution, gamma). It only writes a resource that the render world
        // extracts; it has no ordering dependency on the mouse/sim chain, so it
        // runs as an independent system in the same gate.
        app.add_systems(
            Update,
            (
                systems::update_dots_mouse_attractor,
                systems::decay_dots_mouse_attractor,
                systems::update_dots_sim_params,
            )
                .chain()
                .run_if(sketch_active(AppState::Dots)),
        );
        app.add_systems(
            Update,
            systems::update_dots_post_params.run_if(sketch_active(AppState::Dots)),
        );

        // Hand attractors (D3) and screensaver (D6) will be added here.
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

/// Idle veto for the Dots sketch. Returns `true` while the mouse attractor's
/// power is non-zero (i.e., active or still decaying) — keeps the sketch in
/// `SketchActivity::Active` so [`systems::decay_dots_mouse_attractor`] continues
/// to fire until the attractor is fully released.
///
/// Mirrors [`crate::line::line_idle_veto`] for the Line sketch.
fn dots_idle_veto(world: &World) -> bool {
    world
        .get_resource::<systems::DotsMouseAttractorState>()
        .is_some_and(|s| s.power > 0.0)
}

/// `OnEnter(AppState::Dots)` — insert [`post_process::DotsPostParams`] with
/// static seed values. [`systems::update_dots_post_params`] overwrites these
/// every frame with live cursor, resolution, and gamma; the values here are
/// only visible on the first frame before the `Update` systems run.
///
/// Seed values:
/// - `shrink_factor = 0.98` — v4 default.
/// - `gamma = 1.0` — identity; the Update driver reads `DotsSettings` each frame.
/// - `i_mouse = [0.5, 0.5]` — screen centre (normalised UV); prevents a corner
///   explode on the first frame before any cursor is known.
/// - `i_resolution` — from the primary window; falls back to `[1920.0, 1080.0]`.
fn insert_dots_post_params(mut commands: Commands<'_, '_>, window: Query<'_, '_, &Window>) {
    let (w, h) = window
        .single()
        .map_or((1920.0, 1080.0), |win| (win.width(), win.height()));
    commands.insert_resource(post_process::DotsPostParams {
        i_resolution: [w, h],
        i_mouse: [0.5, 0.5],
        shrink_factor: 0.98,
        gamma: 1.0,
    });
}

/// `OnExit(AppState::Dots)` companion to [`systems::spawn_dots`].
///
/// Drops `ParticleSimParams` so its `Handle<ShaderBuffer>` clone is freed and
/// the GPU storage buffer's ref-count reaches zero, releasing VRAM on each
/// Enter/Exit cycle. Also drops `CpuMirror` so its per-particle `Vec` is
/// freed; `spawn_dots` re-inserts a fresh snapshot on the next `OnEnter`.
/// Also drops [`post_process::DotsPostParams`] so the render system no-ops
/// outside Dots (the `Option<Res<DotsPostParams>>` gate returns `None`).
fn remove_dots_sim_params(mut commands: Commands<'_, '_>) {
    commands.remove_resource::<crate::particles::compute::ParticleSimParams>();
    commands.remove_resource::<crate::particles::sim_cpu::CpuMirror>();
    commands.remove_resource::<post_process::DotsPostParams>();
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

    /// `dots_idle_veto` returns `true` while power > 0 and `false` when power
    /// is zero — ensures the sketch stays `Active` during attractor decay.
    /// Mirrors `line_idle_veto` behavior.
    #[test]
    fn dots_idle_veto_true_while_power_nonzero() {
        use systems::DotsMouseAttractorState;

        let mut world = World::new();

        // No resource at all → veto returns false (no attractor in flight).
        assert!(
            !dots_idle_veto(&world),
            "veto must be false when DotsMouseAttractorState is absent"
        );

        // Power = 0.0 → veto false.
        world.insert_resource(DotsMouseAttractorState {
            power: 0.0,
            position: [0.0, 0.0],
        });
        assert!(
            !dots_idle_veto(&world),
            "veto must be false when power == 0.0"
        );

        // Power > 0.0 → veto true (attractor still active or decaying).
        world.insert_resource(DotsMouseAttractorState {
            power: 1.0,
            position: [0.0, 0.0],
        });
        assert!(dots_idle_veto(&world), "veto must be true when power > 0.0");
    }

    /// `remove_dots_sim_params` must drop `ParticleSimParams`, `CpuMirror`,
    /// and `DotsPostParams` on Dots exit so VRAM and CPU memory are released
    /// and the render system no-ops outside Dots.
    #[test]
    fn remove_dots_sim_params_drops_resources() {
        use crate::particles::compute::ParticleSimParams;
        use crate::particles::particle::SimParams;
        use crate::particles::sim_cpu::CpuMirror;
        use post_process::DotsPostParams;

        let mut world = World::new();
        world.insert_resource(ParticleSimParams {
            params: SimParams::default(),
            particles_handle: Handle::default(),
            particle_count: 0,
        });
        world.insert_resource(CpuMirror { particles: vec![] });
        world.insert_resource(DotsPostParams {
            i_resolution: [1920.0, 1080.0],
            i_mouse: [0.5, 0.5],
            shrink_factor: 0.98,
            gamma: 1.0,
        });

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
        assert!(
            world.get_resource::<DotsPostParams>().is_none(),
            "DotsPostParams must be removed on Dots exit so render system no-ops"
        );
    }

    /// `insert_dots_post_params` must insert `DotsPostParams` on Dots enter
    /// with the static defaults: `shrink_factor=0.98`, `gamma=1.0`,
    /// `i_mouse=[0.5, 0.5]`, and `i_resolution` read from the window (or the
    /// fallback `[1920, 1080]` when no window entity is present).
    #[test]
    #[allow(clippy::float_cmp, reason = "comparing literal defaults")]
    fn insert_dots_post_params_inserts_resource() {
        use post_process::DotsPostParams;

        let mut world = World::new();
        // No window entity — the system falls back to [1920, 1080].
        world
            .run_system_once(insert_dots_post_params)
            .expect("insert_dots_post_params run");

        let params = world
            .get_resource::<DotsPostParams>()
            .expect("DotsPostParams must be present after OnEnter(Dots)");
        assert_eq!(params.shrink_factor, 0.98, "shrink_factor must be 0.98");
        assert_eq!(params.gamma, 1.0, "gamma must be 1.0");
        assert_eq!(params.i_mouse, [0.5, 0.5], "i_mouse must default to centre");
        // Fallback resolution when no window is present.
        assert_eq!(
            params.i_resolution,
            [1920.0, 1080.0],
            "i_resolution must fall back to [1920, 1080] when no window"
        );
    }
}
