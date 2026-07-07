//! Name-change watcher.
//!
//! [`watch_flame_name`] runs every frame while Flame is the active state (even
//! in the screensaver, whose carousel rewrites the name) and rebuilds the whole
//! fractal only when the normalized name or the point budget actually changes.
//! The rebuild allocates (branch build, level layout) — acceptable and
//! documented: it is event-driven and rare, like `LineSynth` graph
//! construction, never the per-frame path.
//!
//! The node buffer is **never** rewritten here. It is seeded once by
//! `spawn_flame` (root at v4's jumpiness position, children at the origin) and
//! from then on the compute pass owns it: on any name change the live GPU shape
//! is lerped into the new attractor (the direct seed-to-seed morph), typed and
//! carousel changes alike. That is why the buffer is `RENDER_WORLD`-only — no
//! CPU mirror, no per-change re-upload.

use bevy::ecs::system::NonSendMut;
use bevy::prelude::*;

use wc_core::audio::command::AudioCommand;
use wc_core::audio::ring::AudioCommandSender;

use crate::flame::audio_coupling::{push_flame_config, FlameChordDegreeCache};
use crate::flame::branches::{build_flame_spec, normalize_name};
use crate::flame::compute::sim_params::{encode_branches, encode_levels, FlameSimParams};
use crate::flame::levels::LevelLayout;
use crate::flame::settings::FlameSettings;
use crate::flame::systems::sim_params::FlameState;

/// `Update` (gated `in_state(AppState::Flame)`): rebuild the fractal when the
/// name or point budget changes.
///
/// Gated on the state, not `sketch_active`, because the screensaver carousel
/// changes the name while the sketch is idle. On a change: rebuild the
/// [`crate::flame::branches::FlameSpec`] + [`LevelLayout`] and re-encode the GPU
/// branch/level tables, then update [`FlameState`]. The node buffer is left
/// alone — the compute morphs the live shape into the new attractor (see the
/// module docs). On a rebuild also pushes the audio config: an instant
/// `"duck_pulse"` mute (v4's anti-click dip before the swap; the synth's
/// `follow(0.016)` smoother turns it into a fast dip rather than an audible pop)
/// followed by the whole name-derived param surface via `push_flame_config`
/// (F14).
pub fn watch_flame_name(
    settings: Res<'_, FlameSettings>,
    mut state: ResMut<'_, FlameState>,
    mut sim: ResMut<'_, FlameSimParams>,
    mut degree_cache: Option<ResMut<'_, FlameChordDegreeCache>>,
    audio_cmd: Option<NonSendMut<'_, AudioCommandSender>>,
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
    // The node buffer is deliberately NOT rewritten. The live shape lives in the
    // GPU buffer; the compute lerps it into the new name's attractor (the direct
    // seed-to-seed morph — `update_flame_sim`, ordered after this system, holds
    // the morph lerp). Re-uploading a CPU copy would re-extract and collapse the
    // shape back to the seed. Every tree fits the fixed `MAX_POINTS` allocation
    // seeded once by `spawn_flame`, so no resize or reseed is ever needed. On a
    // point-budget *increase*, nodes newly promoted out of the dead tail morph in
    // from wherever they last sat (origin on first growth; the lerp smooths it).
    // The billboard mesh is likewise fixed at `MAX_POINTS * 6` vertices and the
    // shader draws only the live prefix via `render_b.x`.

    state.spec = spec;
    state.layout = layout;
    name.clone_into(&mut state.last_name);
    state.last_target_points = settings.target_points;

    // `push_flame_config` (on the audio path below) re-pushes the bare base
    // chord_degree, so invalidate `drive_flame_audio`'s per-frame cache — a
    // same-base rename would otherwise leave the screen-Y pitch offset stale
    // (see `FlameChordDegreeCache`). Independent of whether audio is running.
    if let Some(degree_cache) = degree_cache.as_mut() {
        degree_cache.0 = None;
    }

    // Audio: instant duck before the new config lands (v4's anti-click mute
    // ahead of the swap), then the whole name-derived param surface. Skipped
    // cleanly when no audio engine is running (headless tests, no cpal device).
    if let Some(mut audio_cmd) = audio_cmd {
        if let Err(_dropped) = audio_cmd.push(AudioCommand::SetFlameParam {
            key: "duck_pulse",
            value: 1.0,
        }) {
            tracing::warn!("audio command ring full; dropping Flame duck_pulse");
        }
        push_flame_config(
            &mut audio_cmd,
            &state.spec.audio,
            settings.chord_energy_scale,
        );
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use bevy::asset::{AssetPlugin, RenderAssetUsages};
    use bevy::ecs::system::RunSystemOnce;
    use bevy::render::storage::ShaderBuffer;
    use bytemuck::{cast_slice, Zeroable};

    use crate::flame::compute::sim_params::{FlameLevelParamsGpu, FlameNodeGpu};
    use crate::flame::levels::MAX_LEVELS;

    /// Insert a `FlameSimParams` for `start_name` whose node buffer holds `nodes`,
    /// returning the buffer handle so a test can assert it is left untouched.
    fn setup(app: &mut App, start_name: &str, nodes: &[FlameNodeGpu]) -> Handle<ShaderBuffer> {
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        app.init_asset::<ShaderBuffer>();

        let spec = build_flame_spec(start_name);
        let layout = LevelLayout::build(4, 100_000.0);
        let handle = {
            let mut buffers = app.world_mut().resource_mut::<Assets<ShaderBuffer>>();
            buffers.add(ShaderBuffer::new(
                cast_slice::<FlameNodeGpu, u8>(nodes),
                RenderAssetUsages::RENDER_WORLD,
            ))
        };

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
            last_name: start_name.into(),
            last_target_points: 100_000.0,
            c_x: 0.0,
            warp_input: Vec2::ZERO,
            complexity: 1.0,
        });
        handle
    }

    /// Read the node buffer's CPU bytes as `f32`s for assertions.
    fn node_floats(app: &App, handle: &Handle<ShaderBuffer>) -> Vec<f32> {
        let buffers = app.world().resource::<Assets<ShaderBuffer>>();
        let data = buffers
            .get(handle)
            .expect("buffer present")
            .data
            .as_ref()
            .expect("cpu data present");
        bytemuck::cast_slice::<u8, f32>(data).to_vec()
    }

    /// Changing the name from "madison" to "xy" rebuilds the spec and re-encodes
    /// the branch table (xy golden: 3 branches), but leaves the node buffer
    /// byte-for-byte untouched — the compute morphs the live shape into the new
    /// attractor rather than reseeding (typed changes now morph like the
    /// carousel).
    #[test]
    fn watch_rebuilds_tables_and_leaves_node_buffer_untouched() {
        let mut app = App::new();
        // A distinctive buffer (node 1 off the origin) so any stray reseed shows.
        let mut initial = vec![FlameNodeGpu::zeroed(); 16];
        initial[0].pos = [3.0, 3.0, 3.0];
        initial[1].pos = [1.0, 2.0, 3.0];
        let handle = setup(&mut app, "madison", &initial);

        app.insert_resource(FlameSettings {
            name: "xy".into(),
            target_points: 100_000.0,
            ..default()
        });

        app.world_mut()
            .run_system_once(watch_flame_name)
            .expect("watcher runs");

        assert_eq!(app.world().resource::<FlameState>().last_name, "xy");
        assert_eq!(
            app.world().resource::<FlameSimParams>().params.branch_count,
            3,
            "xy golden -> 3 branches"
        );
        // Node buffer untouched: same size, node 1 exactly as inserted.
        let nodes = node_floats(&app, &handle);
        assert_eq!(nodes.len(), 16 * 8, "buffer size unchanged (not reseeded)");
        assert_eq!(
            &nodes[8..11],
            &[1.0, 2.0, 3.0],
            "node 1 left exactly as it was"
        );
    }

    /// A name change invalidates `FlameChordDegreeCache` (set to `None`) so the
    /// per-frame audio driver re-asserts base + screen-Y after `push_flame_config`
    /// re-pushes the bare base — the same-base rename desync fix.
    #[test]
    fn watch_invalidates_chord_degree_cache_on_name_change() {
        let mut app = App::new();
        let handle = setup(&mut app, "madison", &vec![FlameNodeGpu::zeroed(); 16]);
        let _ = handle;

        app.insert_resource(FlameSettings {
            name: "ada".into(),
            target_points: 100_000.0,
            ..default()
        });
        // A live cached degree from before the rename.
        app.insert_resource(FlameChordDegreeCache(Some(10.0)));

        app.world_mut()
            .run_system_once(watch_flame_name)
            .expect("watcher runs");

        assert_eq!(
            app.world().resource::<FlameChordDegreeCache>().0,
            None,
            "name change must invalidate the chord-degree cache"
        );
    }

    /// No change to name or point budget -> early return, buffer untouched.
    #[test]
    fn watch_noops_when_unchanged() {
        let mut app = App::new();
        let seed = vec![FlameNodeGpu::zeroed(); 7];
        let handle = setup(&mut app, "madison", &seed);

        app.insert_resource(FlameSettings {
            name: "madison".into(),
            target_points: 100_000.0,
            ..default()
        });

        app.world_mut()
            .run_system_once(watch_flame_name)
            .expect("watcher runs");

        // Buffer untouched: still the 7-node seed we inserted.
        assert_eq!(
            node_floats(&app, &handle).len(),
            7 * 8,
            "unchanged buffer left as-is"
        );
    }
}
