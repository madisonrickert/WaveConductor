//! `OnEnter(AppState::Flame)` spawn plus the `OnExit` resource teardown.
//!
//! Allocates the GPU node storage buffer (capacity [`MAX_POINTS`]), encodes the
//! persisted name's branch/level tables, and installs
//! [`FlameSimParams`] (render-world source) and [`FlameState`] (main-world
//! mirror). On exit the resources are dropped, releasing the buffer handle and
//! its VRAM; the render-world copy of [`FlameSimParams`] dies via the F6
//! `ExtractSchedule` removal companion.

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::storage::ShaderBuffer;
use bytemuck::{cast_slice, Zeroable};

use crate::flame::branches::{build_flame_spec, normalize_name};
use crate::flame::compute::sim_params::{
    encode_branches, encode_levels, FlameLevelParamsGpu, FlameNodeGpu, FlameSimParams,
};
use crate::flame::levels::{LevelLayout, MAX_LEVELS, MAX_POINTS};
use crate::flame::settings::FlameSettings;
use crate::flame::systems::name_change::reseed_nodes;
use crate::flame::systems::sim_params::FlameState;

/// Marker component placed on every entity owned by the Flame sketch.
///
/// `OnExit(AppState::Flame)` despawns everything tagged with this marker via
/// [`wc_core::sketch::despawn_with`]. The node buffer itself is owned by the
/// [`FlameSimParams`] resource (removed separately), not an entity; the mesh /
/// material entities that carry this marker are added in a later stage.
#[derive(Component)]
pub struct FlameRoot;

/// `OnEnter(AppState::Flame)`: allocate the node buffer, encode the persisted
/// name, and insert the sim resources.
///
/// The buffer is created at full [`MAX_POINTS`] capacity, then [`reseed_nodes`]
/// resizes it to the live tree and seeds the root — the same fresh-tree start
/// v4 uses (children bloom in from the origin under the 0.8 position lerp).
pub fn spawn_flame(
    settings: Res<'_, FlameSettings>,
    mut buffers: ResMut<'_, Assets<ShaderBuffer>>,
    mut commands: Commands<'_, '_>,
) {
    let name = normalize_name(&settings.name);
    let spec = build_flame_spec(name);
    let branch_count = u32::try_from(spec.branches.len()).unwrap_or(2);
    let layout = LevelLayout::build(branch_count, f64::from(settings.target_points));

    // Allocate the storage buffer at full capacity (mirror the Line spawn's
    // `ShaderBuffer::new` construction; the default usage flags already carry
    // STORAGE | COPY_SRC | COPY_DST).
    let capacity = usize::try_from(MAX_POINTS).unwrap_or(0);
    let zeroed = vec![FlameNodeGpu::zeroed(); capacity];
    let handle = buffers.add(ShaderBuffer::new(
        cast_slice::<FlameNodeGpu, u8>(&zeroed),
        RenderAssetUsages::RENDER_WORLD,
    ));
    // Seed the root + size the buffer to the live tree.
    reseed_nodes(&mut buffers, &handle, layout.total);

    // Encode the frame-constant branch table and the per-level dispatch slots.
    let params = encode_branches(&spec);
    let mut levels = [FlameLevelParamsGpu::zeroed(); MAX_LEVELS];
    let level_count = encode_levels(&layout, &mut levels);

    commands.insert_resource(FlameSimParams {
        params,
        levels,
        level_count,
        nodes: handle,
    });
    commands.insert_resource(FlameState {
        spec,
        layout,
        last_name: name.to_owned(),
        last_target_points: settings.target_points,
        c_x: 0.0,
        warp_input: Vec2::ZERO,
        complexity: 1.0,
    });
}

/// `OnExit(AppState::Flame)`: drop the sim resources.
///
/// Removing [`FlameSimParams`] drops the sole `Handle<ShaderBuffer>`, so the
/// asset (and its VRAM) is released; the render-world mirror is torn down by
/// the compute plugin's removal companion.
pub fn remove_flame_resources(mut commands: Commands<'_, '_>) {
    commands.remove_resource::<FlameSimParams>();
    commands.remove_resource::<FlameState>();
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use bevy::asset::AssetPlugin;
    use bevy::ecs::system::RunSystemOnce;

    /// `spawn_flame` inserts both sim resources, sizes the node buffer to the
    /// live tree, and seeds the root at v4's jumpiness position.
    #[test]
    fn spawn_inserts_resources_and_seeds_root() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        app.init_asset::<ShaderBuffer>();
        app.insert_resource(FlameSettings {
            name: "madison".into(),
            ..default()
        });

        app.world_mut()
            .run_system_once(spawn_flame)
            .expect("spawn runs");

        let sim = app.world().resource::<FlameSimParams>();
        assert_eq!(sim.params.branch_count, 4, "madison -> 4 branches");
        let state = app.world().resource::<FlameState>();
        assert_eq!(state.last_name, "madison");
        assert!((state.complexity - 1.0).abs() < f32::EPSILON);

        let handle = sim.nodes.clone();
        let total = usize::try_from(state.layout.total).expect("fits");
        let buffers = app.world().resource::<Assets<ShaderBuffer>>();
        let buffer = buffers.get(&handle).expect("node buffer present");
        let data = buffer.data.as_ref().expect("cpu data present");
        assert_eq!(data.len(), total * 32, "one 32-byte node per slot");
        let root: &[f32] = bytemuck::cast_slice(&data[0..16]);
        assert_eq!(&root[0..3], &[3.0, 3.0, 3.0], "root seeded at jumpiness");

        // Teardown drops the resources.
        app.world_mut()
            .run_system_once(remove_flame_resources)
            .expect("teardown runs");
        assert!(app.world().get_resource::<FlameSimParams>().is_none());
        assert!(app.world().get_resource::<FlameState>().is_none());
    }
}
