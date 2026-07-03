//! Name-change watcher and node-buffer reseed.
//!
//! [`watch_flame_name`] runs every frame while Flame is the active state (even
//! in the screensaver, whose carousel rewrites the name) and rebuilds the whole
//! fractal only when the normalized name or the point budget actually changes.
//! The rebuild allocates (branch build, level layout, node reseed) — acceptable
//! and documented: it is event-driven and rare, like `LineSynth` graph
//! construction, never the per-frame path.

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::storage::ShaderBuffer;
use bytemuck::{cast_slice, Zeroable};

use crate::flame::branches::{build_flame_spec, normalize_name};
use crate::flame::compute::sim_params::{
    encode_branches, encode_levels, FlameNodeGpu, FlameSimParams,
};
use crate::flame::levels::LevelLayout;
use crate::flame::settings::FlameSettings;
use crate::flame::systems::sim_params::FlameState;

/// Rewrite the node storage buffer to `total` slots, seeding node 0 as the
/// root at v4's `jumpiness` position `[3, 3, 3]` (color black) and leaving
/// every child at the origin — v4's fresh tree starts collapsed and lets the
/// 0.8 position lerp bloom the shape in over the first frames.
///
/// Allocates a fresh `Vec` (name-change path — documented as acceptable) and
/// replaces the asset in place, so the render world re-uploads it.
pub fn reseed_nodes(buffers: &mut Assets<ShaderBuffer>, handle: &Handle<ShaderBuffer>, total: u32) {
    let count = usize::try_from(total).unwrap_or(0);
    let mut nodes = vec![FlameNodeGpu::zeroed(); count];
    if let Some(root) = nodes.first_mut() {
        root.pos = [3.0, 3.0, 3.0];
        root.color = [0.0, 0.0, 0.0];
    }
    if let Some(mut buffer) = buffers.get_mut(handle) {
        *buffer = ShaderBuffer::new(
            cast_slice::<FlameNodeGpu, u8>(&nodes),
            RenderAssetUsages::RENDER_WORLD,
        );
    }
}

/// `Update` (gated `in_state(AppState::Flame)`): rebuild the fractal when the
/// name or point budget changes.
///
/// Gated on the state, not `sketch_active`, because the screensaver carousel
/// changes the name while the sketch is idle. On a change: rebuild the
/// [`crate::flame::branches::FlameSpec`] + [`LevelLayout`], re-encode the GPU
/// branch/level tables, reseed the node buffer, and update [`FlameState`].
/// (F8 extends this to rebuild the mesh; F14 to push the audio config.)
pub fn watch_flame_name(
    settings: Res<'_, FlameSettings>,
    mut state: ResMut<'_, FlameState>,
    mut sim: ResMut<'_, FlameSimParams>,
    mut buffers: ResMut<'_, Assets<ShaderBuffer>>,
) {
    let name = normalize_name(&settings.name);
    let name_unchanged = name == state.last_name.as_str();
    let points_unchanged = (settings.target_points - state.last_target_points).abs() < f32::EPSILON;
    if name_unchanged && points_unchanged {
        return;
    }

    let spec = build_flame_spec(name);
    let branch_count = u32::try_from(spec.branches.len()).unwrap_or(2);
    let layout = LevelLayout::build(branch_count, f64::from(settings.target_points));

    // Re-encode the frame-constant branch table (warp resets to zero; the
    // per-frame writer re-bakes it) and the per-level dispatch slots.
    sim.params = encode_branches(&spec);
    sim.level_count = encode_levels(&layout, &mut sim.levels);
    reseed_nodes(&mut buffers, &sim.nodes, layout.total);

    state.spec = spec;
    state.layout = layout;
    name.clone_into(&mut state.last_name);
    state.last_target_points = settings.target_points;
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use bevy::asset::AssetPlugin;
    use bevy::ecs::system::RunSystemOnce;

    use crate::flame::branches::build_flame_spec;
    use crate::flame::compute::sim_params::{FlameLevelParamsGpu, FlameSimParams};
    use crate::flame::levels::MAX_LEVELS;

    /// Changing the settings name from "madison" to "xy" rebuilds the spec,
    /// re-encodes the branch table (xy golden: 3 branches), and reseeds the
    /// node buffer to the new tree total with the root at jumpiness.
    #[test]
    fn watch_rebuilds_on_name_change() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        app.init_asset::<ShaderBuffer>();

        // Start built for "madison", but settings now say "xy".
        let start_spec = build_flame_spec("madison");
        let start_layout = LevelLayout::build(4, 100_000.0);
        let mut buffers = app.world_mut().resource_mut::<Assets<ShaderBuffer>>();
        let seed: Vec<FlameNodeGpu> = vec![FlameNodeGpu::zeroed(); 16];
        let handle = buffers.add(ShaderBuffer::new(
            cast_slice::<FlameNodeGpu, u8>(&seed),
            RenderAssetUsages::RENDER_WORLD,
        ));

        let mut levels = [FlameLevelParamsGpu::zeroed(); MAX_LEVELS];
        let level_count = encode_levels(&start_layout, &mut levels);
        app.insert_resource(FlameSimParams {
            params: encode_branches(&start_spec),
            levels,
            level_count,
            nodes: handle.clone(),
        });
        app.insert_resource(FlameState {
            spec: start_spec,
            layout: start_layout,
            last_name: "madison".into(),
            last_target_points: 100_000.0,
            c_x: 0.0,
            warp_input: Vec2::ZERO,
            complexity: 1.0,
        });
        app.insert_resource(FlameSettings {
            name: "xy".into(),
            target_points: 100_000.0,
            ..default()
        });

        app.world_mut()
            .run_system_once(watch_flame_name)
            .expect("watcher runs");

        let state = app.world().resource::<FlameState>();
        assert_eq!(state.last_name, "xy");
        let expected_total = usize::try_from(state.layout.total).expect("fits");
        let sim = app.world().resource::<FlameSimParams>();
        assert_eq!(sim.params.branch_count, 3, "xy golden -> 3 branches");

        let buffers = app.world().resource::<Assets<ShaderBuffer>>();
        let data = buffers
            .get(&handle)
            .expect("buffer present")
            .data
            .as_ref()
            .expect("cpu data present");
        assert_eq!(data.len(), expected_total * 32, "one 32-byte node per slot");
        let root: &[f32] = bytemuck::cast_slice(&data[0..16]);
        assert_eq!(&root[0..3], &[3.0, 3.0, 3.0], "root seeded at jumpiness");
    }

    /// No change to name or point budget -> early return, buffer untouched.
    #[test]
    fn watch_noops_when_unchanged() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        app.init_asset::<ShaderBuffer>();

        let spec = build_flame_spec("madison");
        let layout = LevelLayout::build(4, 100_000.0);
        let mut buffers = app.world_mut().resource_mut::<Assets<ShaderBuffer>>();
        let seed: Vec<FlameNodeGpu> = vec![FlameNodeGpu::zeroed(); 7];
        let handle = buffers.add(ShaderBuffer::new(
            cast_slice::<FlameNodeGpu, u8>(&seed),
            RenderAssetUsages::RENDER_WORLD,
        ));
        let mut levels = [FlameLevelParamsGpu::zeroed(); MAX_LEVELS];
        let level_count = encode_levels(&layout, &mut levels);
        app.insert_resource(FlameSimParams {
            params: encode_branches(&spec),
            levels,
            level_count,
            nodes: handle.clone(),
        });
        app.insert_resource(FlameState {
            spec,
            layout,
            last_name: "madison".into(),
            last_target_points: 100_000.0,
            c_x: 0.0,
            warp_input: Vec2::ZERO,
            complexity: 1.0,
        });
        app.insert_resource(FlameSettings {
            name: "madison".into(),
            target_points: 100_000.0,
            ..default()
        });

        app.world_mut()
            .run_system_once(watch_flame_name)
            .expect("watcher runs");

        // Buffer untouched: still the 7-node seed we inserted.
        let buffers = app.world().resource::<Assets<ShaderBuffer>>();
        let data = buffers
            .get(&handle)
            .expect("buffer present")
            .data
            .as_ref()
            .expect("cpu data present");
        assert_eq!(data.len(), 7 * 32, "unchanged buffer left as-is");
    }
}
