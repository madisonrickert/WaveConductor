//! `WaveConductor` sketches.
//!
//! The [`SketchesPlugin`] umbrella registers every concrete sketch plugin.
//! Each sketch lives in its own module and follows the pattern documented in
//! [`wc_core::sketch`].

pub mod cymatics;
pub mod dots;
pub mod flame;
pub mod hand_mesh;
pub mod line;
pub mod particles;
pub mod radiance;

use bevy::prelude::*;
use bevy::sprite_render::Material2dPlugin;

/// Umbrella plugin that registers every concrete sketch.
pub struct SketchesPlugin;

impl Plugin for SketchesPlugin {
    fn build(&self, app: &mut App) {
        // Note: Bevy's `WireframePlugin` is intentionally NOT registered. It
        // requires `WgpuFeatures::POLYGON_MODE_LINE`, which Metal does not
        // support, so it no-ops on macOS and bones rendered solid. The shared
        // `hand_mesh` module instead draws bones as `LineList` meshes
        // with a custom `BoneWireframeMaterial` (see
        // `hand_mesh::bone_wireframe`), which is Metal-safe and — unlike the closed
        // wireframe/gizmo pipelines — shader- and post-process-extensible. Its
        // `MaterialPlugin` is registered below in `SketchesPlugin::build`.

        // Shared particle plugins: registered once here so multiple sketch
        // plugins (Line, Dots, …) can consume them without triggering Bevy's
        // unique-plugin panic. `Material2dPlugin` and `ParticleComputePlugin`
        // are both `Plugin` singletons — adding them more than once would
        // panic at startup.
        app.add_plugins(Material2dPlugin::<
            crate::particles::material::ParticleMaterial,
        >::default());
        app.add_plugins(crate::particles::compute::ParticleComputePlugin);

        // Cymatics ping-pong compute node, registered once here (a `Plugin`
        // singleton). Inert until the Cymatics sketch inserts `CymaticsSimParams`
        // on entry, so it costs nothing on other sketches.
        app.add_plugins(crate::cymatics::compute::CymaticsComputePlugin);

        // Flame level-parallel IFS compute node, registered once here (a `Plugin`
        // singleton). Inert until the Flame sketch inserts `FlameSimParams` on
        // entry, so it costs nothing on other sketches.
        app.add_plugins(crate::flame::compute::pipeline::FlameComputePlugin);

        // Radiance edge-respawn compute node, registered once (a Plugin
        // singleton). Inert until the Radiance sketch inserts
        // RadianceSimParams on entry. Feature-gated: it consumes
        // wc_core::input::body (see radiance::compute::mod's cfg on
        // `pipeline`/`edge_upload`), which is absent from the default
        // feature set the doc gate builds.
        #[cfg(feature = "body-tracking-mediapipe")]
        app.add_plugins(crate::radiance::compute::pipeline::RadianceComputePlugin);

        // Flame additive billboard render material, registered once (a `Plugin`
        // singleton; adding it twice would panic at startup). The mesh + material
        // entity is spawned on Flame entry (`spawn_flame`); registering here keeps
        // the material pipeline compiled even before sketch entry.
        app.add_plugins(Material2dPlugin::<crate::flame::render::FlameMaterial>::default());

        // Cymatics Material2d render material, registered once (a `Plugin`
        // singleton; adding it twice would panic at startup). The quad is
        // spawned in Stage 4 (CymaticsPlugin::build); registering here keeps
        // the material pipeline compiled even before sketch entry.
        app.add_plugins(Material2dPlugin::<crate::cymatics::render::CymaticsMaterial>::default());

        // Radiance additive billboard material, registered once (Plugin
        // singleton; the mesh + material entity spawns on Radiance entry).
        app.add_plugins(Material2dPlugin::<crate::radiance::render::RadianceMaterial>::default());

        // Radiance beat-pulse wave material, registered once (Plugin
        // singleton; the fullscreen quad spawns on Radiance entry).
        // Feature-gated like the silhouette material: the pulse module
        // consumes the body-tracking distance field.
        #[cfg(feature = "body-tracking-mediapipe")]
        app.add_plugins(Material2dPlugin::<
            crate::radiance::pulse::RadiancePulseMaterial,
        >::default());

        // Radiance extremity-sparkle material, registered once (Plugin
        // singleton; same gating — the sparkle module consumes body
        // landmarks).
        #[cfg(feature = "body-tracking-mediapipe")]
        app.add_plugins(Material2dPlugin::<
            crate::radiance::sparkle::RadianceSparkleMaterial,
        >::default());

        // Radiance silhouette fill material, registered once (Plugin
        // singleton; the quad spawns on Radiance entry, Task 9).
        // Feature-gated: it samples the body-tracking mask and its driver
        // reads RadianceState, both behind `body-tracking-mediapipe` (see
        // `radiance::render`'s module doc), absent from the default feature
        // set the doc gate builds.
        #[cfg(feature = "body-tracking-mediapipe")]
        app.add_plugins(Material2dPlugin::<
            crate::radiance::render::RadianceSilhouetteMaterial,
        >::default());

        // Shared hand-mesh overlay infra, registered once (like the particle
        // plugins above) so each sketch's `HandMeshPlugin` can be added without
        // re-registering the material or composite node. `MaterialPlugin` and
        // `HandMeshCompositePlugin` are `Plugin` singletons.
        app.add_plugins(bevy::pbr::MaterialPlugin::<
            crate::hand_mesh::BoneWireframeMaterial,
        >::default());
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
        app.add_plugins(flame::FlamePlugin);
        // Cymatics lifecycle (textures, quad, sim-params bridge). The shared
        // compute node + `Material2dPlugin::<CymaticsMaterial>` are registered
        // above; `CymaticsPlugin` adds the per-sketch lifecycle exactly once.
        app.add_plugins(cymatics::CymaticsPlugin);

        // Radiance lifecycle (settings, tile; sim/render/attract arrive in
        // later Plan C tasks — compute + material plugins are registered
        // above once they exist).
        app.add_plugins(radiance::RadiancePlugin);
    }
}
