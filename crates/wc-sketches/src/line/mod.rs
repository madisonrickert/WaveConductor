//! Line sketch — particles attracted to a pointer-driven gravity well.
//!
//! ## Data flow
//!
//! 1. `OnEnter(AppState::Line)` runs [`systems::spawn_line`]: allocates the
//!    particle storage buffer, spawns the render entity under [`LineRoot`],
//!    and installs [`compute::LineSimParams`].
//! 2. Every `Update` while `sketch_active(AppState::Line)` is true,
//!    [`systems::update_sim_params`] writes the current pointer position +
//!    `LineSettings` values into `LineSimParams`.
//! 3. The render world extracts `LineSimParams` and dispatches the compute
//!    pipeline (`assets/shaders/line/simulate.wgsl`) which updates the
//!    storage buffer in place.
//! 4. Bevy's 2D render path consumes the same storage buffer through
//!    [`material::LineMaterial`] and draws a quad per particle via the
//!    vertex-index-driven `assets/shaders/line/render.wgsl`.
//! 5. `OnExit(AppState::Line)` runs `despawn_with::<LineRoot>` and
//!    `remove_sim_params` to free the entity tree and drop the
//!    `LineSimParams` resource so its `Handle<ShaderStorageBuffer>` clone is
//!    released, allowing the GPU storage buffer ref-count to reach zero.

pub mod compute;
pub mod material;
pub mod particle;
pub mod settings;
pub mod systems;

pub use systems::LineRoot;

use bevy::prelude::*;
use bevy::sprite_render::Material2dPlugin;
use wc_core::lifecycle::state::AppState;
use wc_core::settings::{RegisterSketchSettingsExt, SketchSettings};
use wc_core::sketch::{despawn_with, sketch_active};

/// Plugin that registers the Line sketch.
pub struct LinePlugin;

impl Plugin for LinePlugin {
    fn build(&self, app: &mut App) {
        // Register LineSettings with the settings system (panel + persistence).
        app.register_sketch_settings::<settings::LineSettings>();

        // Register the Material2d for LineMaterial.
        app.add_plugins(Material2dPlugin::<material::LineMaterial>::default());

        // Wire the compute pipeline.
        app.add_plugins(compute::LineComputePlugin);

        // Lifecycle: spawn on enter, despawn on exit.
        app.add_systems(OnEnter(AppState::Line), systems::spawn_line);
        app.add_systems(
            OnExit(AppState::Line),
            (despawn_with::<LineRoot>, remove_sim_params),
        );

        // Per-frame sim update, gated to active state only.
        app.add_systems(
            Update,
            systems::update_sim_params.run_if(sketch_active(AppState::Line)),
        );

        // Restart listener: exits to Home when particle_count changes.
        app.add_systems(Update, restart_on_settings_change);
    }
}

/// `OnExit(AppState::Line)` companion to [`systems::spawn_line`].
///
/// Drops the `LineSimParams` resource so its `Handle<ShaderStorageBuffer>`
/// clone is freed and the GPU storage buffer's ref-count reaches zero,
/// releasing VRAM on each Enter/Exit cycle.
fn remove_sim_params(mut commands: Commands<'_, '_>) {
    commands.remove_resource::<compute::LineSimParams>();
}

/// Listens for `SketchRestart { storage_key == LineSettings::STORAGE_KEY }`
/// and forces a same-frame `Line → Home → Line` cycle so the `OnExit`/`OnEnter`
/// systems rebuild the sketch with the new settings.
///
/// Uses a one-frame `LineRestartPending` resource as a self-clearing trampoline:
/// on the frame the restart message arrives, we set `NextState::Home` *and*
/// insert `LineRestartPending`. On the following frame's update, the resource is
/// observed → `NextState::Line`, then the resource is removed.
fn restart_on_settings_change(
    mut events: MessageReader<'_, '_, wc_core::settings::SketchRestart>,
    current: Res<'_, State<AppState>>,
    mut next: ResMut<'_, NextState<AppState>>,
    mut commands: Commands<'_, '_>,
    pending: Option<Res<'_, LineRestartPending>>,
) {
    if pending.is_some() {
        // Second frame: complete the cycle by re-entering Line.
        next.set(AppState::Line);
        commands.remove_resource::<LineRestartPending>();
        tracing::info!("LineSettings restart cycle: re-entering Line");
        return;
    }
    let want_restart = events
        .read()
        .any(|e| e.storage_key == settings::LineSettings::STORAGE_KEY);
    if want_restart && **current == AppState::Line {
        next.set(AppState::Home);
        commands.insert_resource(LineRestartPending);
        tracing::info!("LineSettings changed — cycling Line via Home for one frame");
    }
}

/// Trampoline marker for the same-frame Line→Home→Line cycle. Inserted on the
/// frame a restart is detected; the next frame's `restart_on_settings_change`
/// observes it, transitions back to `Line`, and removes the resource.
#[derive(Resource)]
struct LineRestartPending;
