//! Radiance sketch: a webcam-tracked dancer's silhouette rendered as a dark
//! glassy form with an emissive rim, wrapped in an aura of additive HDR
//! particles born on the silhouette edge and driven by curl-noise flow,
//! buoyancy, limb motion, and live audio input. Radiance does not generate
//! audio; it listens (Plan A's input analysis) and watches (Plan B's body
//! tracking).
//!
//! ## Data flow (grows stage by stage; see the 2026-07-12 Plan C document)
//!
//! 1. Settings register with the shared panel/persistence system; the
//!    `RenderProfile` applier drives the main camera's tonemapping/bloom
//!    while Radiance is active.
//! 2. Later tasks add: spawn/teardown, the sim baker, the render-world
//!    compute pipeline, materials, camera arbitration, activity sync, the
//!    screensaver phantom, and debug/capture drivers. `build` below stays the
//!    single source of truth for wiring order.

pub mod compute;
pub mod settings;
pub mod systems;

use bevy::prelude::*;
use wc_core::lifecycle::state::AppState;
use wc_core::settings::RegisterSketchSettingsExt;

/// Plugin that registers the Radiance sketch.
pub struct RadiancePlugin;

impl Plugin for RadiancePlugin {
    fn build(&self, app: &mut App) {
        // Settings: panel + persistence (storage key "radiance").
        app.register_sketch_settings::<settings::RadianceSettings>();

        // Picker-tile manifest entry (async screenshot load).
        register_radiance_manifest(app);

        // Restart listener (requires_restart fields fade out/in via the
        // shared reload overlay). Always-on sanctioned listener.
        app.add_systems(
            Update,
            wc_core::sketch::restart_on_settings_change::<settings::RadianceSettings>,
        );

        // Re-run the spawn path at the new window size when a resize settles
        // (silent/instant reload). Always-on sanctioned listener; defensive
        // add_message mirrors FlamePlugin (Bevy dedups; LifecyclePlugin is
        // canonical).
        app.add_message::<wc_core::lifecycle::window_resize::WindowResizeSettled>();
        app.add_systems(
            Update,
            wc_core::sketch::reload_on_resize_settled::<settings::RadianceSettings>,
        );

        // Tonemapping + bloom profile onto the main camera while Radiance is
        // up (live dev-panel tuning), via the shared generic applier.
        app.add_systems(
            Update,
            wc_core::sketch::apply_render_profile::<settings::RadianceSettings>
                .run_if(in_state(AppState::Radiance)),
        );
    }
}

/// Register Radiance's picker-tile metadata. Factored out of
/// `RadiancePlugin::build` so it is unit-testable without rendering plugins
/// (mirrors `register_flame_manifest`).
pub(crate) fn register_radiance_manifest(app: &mut App) {
    use wc_core::settings::SketchSettings as _;
    wc_core::sketch::register_sketch_tile(
        app,
        AppState::Radiance,
        "Radiance",
        settings::RadianceSettings::STORAGE_KEY,
        "sketches/radiance/screenshot.png",
    );
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use wc_core::sketch::SketchManifest;

    /// Mirrors `register_flame_manifest_appends_entry`: the free-function path
    /// registers a Radiance tile without needing a `RenderApp`.
    #[test]
    fn register_radiance_manifest_appends_entry() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::asset::AssetPlugin::default());
        app.add_plugins(bevy::image::ImagePlugin::default());
        register_radiance_manifest(&mut app);
        let manifest = app.world().resource::<SketchManifest>();
        let entry = manifest
            .get(AppState::Radiance)
            .expect("Radiance manifest entry should be registered");
        assert_eq!(entry.display_name, "Radiance");
        assert_eq!(entry.settings_key, "radiance");
    }
}
