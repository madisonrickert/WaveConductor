//! `WaveConductor` sketches.
//!
//! The [`SketchesPlugin`] umbrella registers every concrete sketch plugin.
//! Each sketch lives in its own module and follows the pattern documented in
//! [`wc_core::sketch`].

pub mod line;

use bevy::prelude::*;

/// Umbrella plugin that registers every concrete sketch.
pub struct SketchesPlugin;

impl Plugin for SketchesPlugin {
    fn build(&self, app: &mut App) {
        // `WireframePlugin` enables opt-in per-entity wireframe rendering used
        // by the Line sketch's `hand_mesh` module. It is registered here in
        // `SketchesPlugin` (not in `wc_core::CorePlugin`) because:
        //
        // 1. `bevy_pbr` is a heavy rendering dependency with no business in
        //    the headless-friendly `wc-core` crate.
        // 2. `WireframePlugin::build()` calls `init_asset::<WireframeMaterial>`,
        //    which requires `AssetPlugin`; that is not part of `MinimalPlugins`
        //    used in `wc-core`'s unit tests — placing it in `CorePlugin` would
        //    break those tests.
        //
        // `WireframePlugin::finish()` checks `WgpuFeatures::POLYGON_MODE_LINE`
        // and bails with a warning on hardware that doesn't support it; the
        // rest of the app continues unaffected.
        app.add_plugins(bevy::pbr::wireframe::WireframePlugin::default());
        app.add_plugins(line::LinePlugin);
    }
}
