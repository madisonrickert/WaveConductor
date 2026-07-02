//! Flame sketch: a name-seeded IFS fractal flame, evaluated level-parallel on
//! the GPU and drawn as an additive point cloud with a fake depth of field.
//!
//! ## Data flow (grows stage by stage; see the 2026-07-02 flame port plan)
//!
//! 1. `OnEnter(AppState::Flame)` swaps the clear color to v4's `#10101f`
//!    (stashing the previous value) — the fog fades points into this color.
//! 2. Settings register with the shared panel/persistence system; the
//!    `RenderProfile` applier drives the main camera's tonemapping/bloom
//!    while Flame is active.
//! 3. `OnExit` restores the clear color and resets the render profile.
//!
//! Simulation, rendering, interaction, audio, and the attract performer are
//! wired in later stages of the port plan.

pub mod branches;
pub mod levels;
pub mod settings;

use bevy::prelude::*;
use wc_core::lifecycle::state::AppState;
use wc_core::settings::RegisterSketchSettingsExt;

/// Plugin that registers the Flame sketch.
pub struct FlamePlugin;

impl Plugin for FlamePlugin {
    fn build(&self, app: &mut App) {
        // Settings: panel + persistence (storage key "flame").
        app.register_sketch_settings::<settings::FlameSettings>();

        // Picker-tile manifest entry (async screenshot load).
        register_flame_manifest(app);

        // v4 scene background: #10101f. The whole sketch reads against it
        // (fog fades points toward it), so it is swapped at the state seam.
        app.add_systems(OnEnter(AppState::Flame), enter_flame_clear_color);
        app.add_systems(
            OnExit(AppState::Flame),
            (
                exit_flame_clear_color,
                wc_core::sketch::reset_render_profile,
            ),
        );

        // Tonemapping + bloom profile onto the main camera while Flame is
        // active (live dev-panel tuning), via the shared generic applier.
        app.add_systems(
            Update,
            wc_core::sketch::apply_render_profile::<settings::FlameSettings>
                .run_if(in_state(AppState::Flame)),
        );

        // Restart listener (requires_restart fields fade out/in via the
        // shared reload overlay). The name is NOT requires_restart — it
        // rebuilds live through the name-change watcher (later stage).
        app.add_systems(
            Update,
            wc_core::sketch::restart_on_settings_change::<settings::FlameSettings>,
        );
    }
}

/// Register Flame's picker-tile metadata. Factored out of `FlamePlugin::build`
/// so it is unit-testable without rendering plugins (mirrors
/// `register_dots_manifest`).
pub(crate) fn register_flame_manifest(app: &mut App) {
    // Delegates to the shared `register_sketch_tile` helper (async PNG load +
    // manifest append). v4 calls this sketch "You-niverse" in HomePage.tsx.
    wc_core::sketch::register_sketch_tile(
        app,
        AppState::Flame,
        "You-niverse",
        "sketches/flame/screenshot.png",
    );
}

/// Stash for the pre-Flame clear color, restored on exit.
#[derive(Resource)]
struct SavedClearColor(ClearColor);

/// `OnEnter(AppState::Flame)`: stash the current clear color and swap in
/// v4's scene background `#10101f`.
fn enter_flame_clear_color(mut commands: Commands<'_, '_>, current: Res<'_, ClearColor>) {
    commands.insert_resource(SavedClearColor(current.clone()));
    commands.insert_resource(ClearColor(Color::srgb_u8(0x10, 0x10, 0x1f)));
}

/// `OnExit(AppState::Flame)`: restore the stashed clear color.
fn exit_flame_clear_color(mut commands: Commands<'_, '_>, saved: Option<Res<'_, SavedClearColor>>) {
    if let Some(saved) = saved {
        commands.insert_resource(saved.0.clone());
    }
    commands.remove_resource::<SavedClearColor>();
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use wc_core::sketch::SketchManifest;

    /// Mirrors `register_dots_manifest_appends_entry`: the free-function path
    /// registers a Flame tile without needing a `RenderApp`.
    #[test]
    fn register_flame_manifest_appends_entry() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::asset::AssetPlugin::default());
        app.add_plugins(bevy::image::ImagePlugin::default());
        register_flame_manifest(&mut app);
        let manifest = app.world().resource::<SketchManifest>();
        let entry = manifest
            .get(AppState::Flame)
            .expect("Flame manifest entry should be registered");
        assert_eq!(entry.display_name, "You-niverse");
    }

    /// `OnEnter` swaps the clear color to v4's #10101f and stashes the prior
    /// value; `OnExit` restores it and drops the stash.
    #[test]
    fn clear_color_swap_and_restore() {
        let mut world = World::new();
        world.insert_resource(ClearColor(Color::WHITE));

        world
            .run_system_once(enter_flame_clear_color)
            .expect("enter runs");
        let cc = world.resource::<ClearColor>();
        assert_eq!(cc.0, Color::srgb_u8(0x10, 0x10, 0x1f));
        assert!(world.get_resource::<SavedClearColor>().is_some());

        world
            .run_system_once(exit_flame_clear_color)
            .expect("exit runs");
        assert_eq!(world.resource::<ClearColor>().0, Color::WHITE);
        assert!(world.get_resource::<SavedClearColor>().is_none());
    }
}
