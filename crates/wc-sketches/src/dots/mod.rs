//! Dots sketch ("Fabric") — a grid of dots that ripple and deform under
//! pointer and hand interaction.
//!
//! ## Data flow (planned — Tasks 2 and 3 wire the runtime)
//!
//! 1. `OnEnter(AppState::Dots)` will allocate the dot-grid storage buffer,
//!    spawn the render entity under a `DotsRoot` marker, and install
//!    `ParticleSimParams` (Task 2).
//! 2. Every `Update` while `sketch_active(AppState::Dots)` is true will
//!    write the pointer position and `DotsSettings` values into
//!    `ParticleSimParams` and drive the compute pipeline (Task 2).
//! 3. `OnExit(AppState::Dots)` will despawn the entity tree and release
//!    VRAM (Task 2).
//! 4. Mouse and hand interaction will be wired in Task 3.
//!
//! The shared [`crate::particles::compute::ParticleComputePlugin`] and
//! [`crate::particles::material::ParticleMaterial`] are registered once by
//! the [`crate::SketchesPlugin`] umbrella, not here.

pub mod settings;

use bevy::prelude::*;
use wc_core::lifecycle::state::AppState;
use wc_core::settings::RegisterSketchSettingsExt;
use wc_core::sketch::RegisterSketchManifestExt;

/// Plugin that registers the Dots (Fabric) sketch.
pub struct DotsPlugin;

impl Plugin for DotsPlugin {
    fn build(&self, app: &mut App) {
        // Register DotsSettings with the settings system (panel + persistence).
        app.register_sketch_settings::<settings::DotsSettings>();

        // Register the picker-tile manifest entry (async screenshot load).
        register_dots_manifest(app);

        // TODO(Task 2): wire OnEnter/OnExit spawn/despawn systems.
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

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions — panicking on unexpected None is the correct behaviour"
)]
mod tests {
    use super::*;
    use wc_core::sketch::SketchManifest;

    /// Verifies that `register_dots_manifest` appends an entry for
    /// `AppState::Dots` with the correct display name.
    ///
    /// Uses the free-function path rather than constructing the full
    /// `DotsPlugin` because `DotsPlugin::build` may gain rendering plugins
    /// (Task 2) that require a real `RenderApp` — unavailable in headless
    /// unit tests. Mirrors `register_line_manifest_appends_entry` in
    /// `crate::line::tests`.
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
}
