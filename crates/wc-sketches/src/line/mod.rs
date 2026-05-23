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
//! 5. `OnExit(AppState::Line)` runs `despawn_with::<LineRoot>` to free the
//!    entity tree. Bevy ref-counts the storage buffer + material; they are
//!    freed when the last handle drops.

pub mod compute;
pub mod material;
pub mod particle;
pub mod settings;
pub mod systems;

pub use systems::LineRoot;

use bevy::prelude::*;
use bevy::sprite_render::Material2dPlugin;
use wc_core::lifecycle::state::AppState;
use wc_core::settings::RegisterSketchSettingsExt;
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
        app.add_systems(OnExit(AppState::Line), despawn_with::<LineRoot>);

        // Per-frame sim update, gated to active state only.
        app.add_systems(
            Update,
            systems::update_sim_params.run_if(sketch_active(AppState::Line)),
        );
    }
}
