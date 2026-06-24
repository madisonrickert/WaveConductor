//! `WaveConductor` sketches.
//!
//! The [`SketchesPlugin`] umbrella registers every concrete sketch plugin.
//! Each sketch lives in its own module and follows the pattern documented in
//! [`wc_core::sketch`].

pub mod dots;
pub mod hand_mesh;
pub mod line;
pub mod particles;

use bevy::prelude::*;
use bevy::sprite_render::Material2dPlugin;

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

        // Shared particle plugins: registered once here so multiple sketch
        // plugins (Line, Dots, …) can consume them without triggering Bevy's
        // unique-plugin panic. `Material2dPlugin` and `ParticleComputePlugin`
        // are both `Plugin` singletons — adding them more than once would
        // panic at startup.
        app.add_plugins(Material2dPlugin::<
            crate::particles::material::ParticleMaterial,
        >::default());
        app.add_plugins(crate::particles::compute::ParticleComputePlugin);

        // Shared hand-mesh overlay infra, registered once (like the particle
        // plugins above) so each sketch's `HandMeshPlugin` can be added without
        // re-registering the material or composite node. `MaterialPlugin` and
        // `HandMeshCompositePlugin` are `Plugin` singletons.
        app.add_plugins(
            bevy::pbr::MaterialPlugin::<crate::hand_mesh::BoneWireframeMaterial>::default(),
        );
        // `WC_DEBUG_DISABLE_BONE_COMPOSITE` gates the composite globally (debug only).
        #[cfg(debug_assertions)]
        let register_bone_composite = !app
            .world()
            .get_resource::<wc_core::debug::DebugToggles>()
            .is_some_and(|t| t.disable_bone_composite);
        #[cfg(not(debug_assertions))]
        let register_bone_composite = true;
        if register_bone_composite {
            app.add_plugins(crate::hand_mesh::HandMeshCompositePlugin);
        }

        // Concrete sketch plugins.
        app.add_plugins(line::LinePlugin);
        app.add_plugins(dots::DotsPlugin);
    }
}
