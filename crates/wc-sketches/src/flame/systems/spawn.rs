//! `OnEnter(AppState::Flame)` spawn plus the `OnExit` resource teardown.
//!
//! Allocates the GPU node storage buffer (capacity [`MAX_POINTS`]), encodes the
//! persisted name's branch/level tables, and installs
//! [`FlameSimParams`] (render-world source) and [`FlameState`] (main-world
//! mirror). On exit the resources are dropped, releasing the buffer handle and
//! its VRAM; the render-world copy of [`FlameSimParams`] dies via the F6
//! `ExtractSchedule` removal companion.

use bevy::asset::RenderAssetUsages;
use bevy::mesh::PrimitiveTopology;
use bevy::prelude::*;
use bevy::render::storage::ShaderBuffer;
use bytemuck::{cast_slice, Zeroable};

use crate::flame::branches::{build_flame_spec, normalize_name};
use crate::flame::compute::sim_params::{
    encode_branches, encode_levels, FlameLevelParamsGpu, FlameNodeGpu, FlameSimParams,
};
use crate::flame::levels::{LevelLayout, MAX_LEVELS, MAX_POINTS};
use crate::flame::render::{default_view_matrices, flame_fog_color, FlameMaterial};
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
#[allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "layout.total is bounded by MAX_POINTS (200k), exact as f32"
)]
pub fn spawn_flame(
    settings: Res<'_, FlameSettings>,
    mut buffers: ResMut<'_, Assets<ShaderBuffer>>,
    mut meshes: ResMut<'_, Assets<Mesh>>,
    mut materials: ResMut<'_, Assets<FlameMaterial>>,
    asset_server: Res<'_, AssetServer>,
    window: Single<'_, '_, &Window>,
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

    // Flat TriangleList mesh of `total * 6` origin vertices (data unused): the
    // vertex shader derives each node + quad corner from `vertex_index`, so the
    // mesh only needs to exist to trigger the draw call. Rebuilt on name change.
    let vertex_count = usize::try_from(layout.total).unwrap_or(0) * 6;
    let positions: Vec<[f32; 3]> = vec![[0.0, 0.0, 0.0]; vertex_count];
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    let mesh_handle = meshes.add(mesh);

    // Seed the material with the v4 start pose; `drive_flame_material` overwrites
    // every uniform from settings + FlameState next Update (and F9 swaps in the
    // live orbit matrices), so these are one-frame placeholders.
    let w = window.width().max(1.0);
    let h = window.height().max(1.0);
    let aspect = w / h;
    let (view_from_model, clip_from_view) = default_view_matrices(aspect);
    let material_handle = materials.add(FlameMaterial {
        nodes: handle.clone(),
        disc_texture: asset_server.load("sketches/flame/disc.png"),
        view_from_model,
        clip_from_view,
        render_a: Vec4::new(0.782_6, 2.0, 3.0, 0.2),
        render_b: Vec4::new(layout.total as f32, 0.545, 1.0, 50.0),
        fog_color: flame_fog_color(),
        fog_range: Vec4::new(2.0, 60.0, w, h),
    });

    commands.spawn((
        FlameRoot,
        bevy::mesh::Mesh2d(mesh_handle),
        bevy::sprite_render::MeshMaterial2d(material_handle),
        Transform::default(),
        GlobalTransform::default(),
        Visibility::default(),
    ));

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
        // F8: `spawn_flame` now also spawns a mesh + material entity, so the
        // system needs the Mesh/FlameMaterial asset stores and a Window.
        app.init_asset::<Mesh>();
        app.init_asset::<Image>();
        app.init_asset::<FlameMaterial>();
        app.world_mut().spawn(Window::default());
        app.insert_resource(FlameSettings {
            name: "madison".into(),
            ..default()
        });

        app.world_mut()
            .run_system_once(spawn_flame)
            .expect("spawn runs");

        // Read the scalars we need as owned values, then drop the immutable
        // world borrows before the `world_mut` mesh query below.
        let (branch_count, total, handle) = {
            let sim = app.world().resource::<FlameSimParams>();
            let state = app.world().resource::<FlameState>();
            assert_eq!(state.last_name, "madison");
            assert!((state.complexity - 1.0).abs() < f32::EPSILON);
            (
                sim.params.branch_count,
                usize::try_from(state.layout.total).expect("fits"),
                sim.nodes.clone(),
            )
        };
        assert_eq!(branch_count, 4, "madison -> 4 branches");

        // The FlameRoot mesh entity carries `madison`'s total * 6 vertices.
        let mesh_handle = app
            .world_mut()
            .query_filtered::<&Mesh2d, With<FlameRoot>>()
            .single(app.world())
            .expect("FlameRoot mesh entity present")
            .0
            .clone();
        let meshes = app.world().resource::<Assets<Mesh>>();
        let mesh = meshes.get(&mesh_handle).expect("mesh present");
        assert_eq!(mesh.count_vertices(), total * 6, "total * 6 vertices");

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
