//! `WaveConductor` sketches.
//!
//! The [`SketchesPlugin`] umbrella registers every concrete sketch plugin.
//! Each sketch lives in its own module and follows the pattern documented in
//! [`wc_core::sketch`].

pub mod line;
pub mod particles;

use bevy::prelude::*;

/// Umbrella plugin that registers every concrete sketch.
pub struct SketchesPlugin;

impl Plugin for SketchesPlugin {
    fn build(&self, app: &mut App) {
        // Note: Bevy's `WireframePlugin` is intentionally NOT registered. It
        // requires `WgpuFeatures::POLYGON_MODE_LINE`, which Metal does not
        // support, so it no-ops on macOS and bones rendered solid. The Line
        // sketch's `hand_mesh` module instead draws bones as `LineList` meshes
        // with a custom `BoneWireframeMaterial` (see
        // `line::bone_wireframe`), which is Metal-safe and — unlike the closed
        // wireframe/gizmo pipelines — shader- and post-process-extensible. Its
        // `MaterialPlugin` is registered by `LineHandMeshPlugin`.
        app.add_plugins(line::LinePlugin);
    }
}
