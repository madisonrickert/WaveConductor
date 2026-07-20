//! Render-world compute plugin for the Flame IFS.
//!
//! Per frame: upload the sim uniform + per-level slots, then run ONE compute
//! pass with `level_count` sequential dispatches — dispatch i updates tree
//! level i+1 from level i via dynamic offset i * 256 into the level-params
//! buffer. WebGPU's implicit ordering between dispatches in a pass makes the
//! parent level's writes visible to the child level's reads.
//!
//! # Signal / data flow
//!
//! [`FlameComputePlugin::build`] wires four pieces into the render world:
//!
//! 1. [`ExtractResourcePlugin`] clones [`FlameSimParams`] (the branch table +
//!    per-frame warp uniform, the per-level dispatch slots, the level count, and
//!    the node-buffer handle) from the main world each frame. The resource is
//!    POD + one `Handle`, so the clone allocates nothing.
//! 2. `remove_flame_sim_params_if_absent` ([`ExtractSchedule`]) mirrors removals
//!    the `ExtractResourcePlugin` does not propagate (the established landmine;
//!    see `cymatics/compute/pipeline.rs`).
//! 3. `init_flame_pipeline` ([`RenderStartup`]) builds the bind-group layout,
//!    queues the compute pipeline, and allocates the two persistent uniform
//!    buffers (the 800-byte `SimParams` and the `MAX_LEVELS`-slot per-level
//!    array) **once** — never per frame.
//! 4. `prepare_flame_bind_groups` ([`RenderSystems::PrepareBindGroups`]) uploads
//!    this frame's uniforms and builds (or reuses) the single bind group, cached
//!    in `FlameBindGroupCache` keyed on the node storage buffer's [`BufferId`]
//!    (bounded by construction: one slot, replaced on change, and *cleared* by
//!    the removal companion on sketch exit so the freed node buffer's `Arc` is
//!    not pinned for the rest of the session). `flame_compute` runs the
//!    per-level dispatch loop in the root [`RenderGraph`] schedule before
//!    `camera_driver`.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "usize/u64/u32 casts for GPU buffer sizes and dynamic offsets are \
              intentional and bounds-checked (MAX_LEVELS * 256 fits in u32)"
)]

use std::borrow::Cow;
use std::num::NonZeroU64;

use bevy::core_pipeline::schedule::camera_driver;
use bevy::prelude::*;
use bevy::render::extract_resource::ExtractResourcePlugin;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{
    BindGroup, BindGroupEntry, BindGroupLayout, BindGroupLayoutDescriptor, BindGroupLayoutEntry,
    BindingResource, BindingType, Buffer, BufferBinding, BufferBindingType, BufferDescriptor,
    BufferId, BufferUsages, CachedComputePipelineId, ComputePassDescriptor,
    ComputePipelineDescriptor, PipelineCache, ShaderStages,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderGraph, RenderQueue};
use bevy::render::storage::GpuShaderBuffer;
use bevy::render::{Extract, ExtractSchedule, Render, RenderApp, RenderStartup, RenderSystems};

use super::sim_params::{FlameNodeGpu, FlameSimParams, FlameSimParamsGpu, LEVEL_PARAMS_STRIDE};
use crate::flame::levels::MAX_LEVELS;

/// Compute workgroup width; level dispatches are `ceil(node_count / 256)`.
/// Must match `@workgroup_size(256)` in `simulate.wgsl`.
const WORKGROUP_SIZE: u32 = 256;

/// [`LEVEL_PARAMS_STRIDE`] as `u32` for the per-level dynamic-offset math.
const LEVEL_PARAMS_STRIDE_U32: u32 = LEVEL_PARAMS_STRIDE as u32;

/// `FlameSimParamsGpu` byte size (800) for binding 0's `min_binding_size`.
///
/// `FlameSimParamsGpu` has fields, so it is non-zero-sized; the `panic!` is in a
/// `const` expression and could only fire at compile time, never at runtime.
const SIM_PARAMS_SIZE: NonZeroU64 =
    match NonZeroU64::new(std::mem::size_of::<FlameSimParamsGpu>() as u64) {
        Some(n) => n,
        None => panic!("FlameSimParamsGpu must be non-zero-sized"),
    };

/// WGSL `LevelParams` byte size (four `u32` = 16) for binding 2's
/// `min_binding_size` and each per-level `BufferBinding`'s size. Only the
/// leading 16 bytes of each 256-byte slot are read by the shader.
const LEVEL_PARAMS_SIZE: NonZeroU64 = match NonZeroU64::new(16) {
    Some(n) => n,
    None => panic!("LEVEL_PARAMS_SIZE must be non-zero"),
};

/// `FlameNodeGpu` byte size (32) for binding 1's node storage `min_binding_size`.
///
/// `FlameNodeGpu` has fields, so it is non-zero-sized; the `panic!` is in a
/// `const` expression and could only fire at compile time, never at runtime.
/// Setting it (rather than `None`) makes wgpu reject a too-small bound node
/// buffer at bind-group creation instead of at DX12 dispatch time.
const NODE_SIZE: NonZeroU64 = match NonZeroU64::new(std::mem::size_of::<FlameNodeGpu>() as u64) {
    Some(n) => n,
    None => panic!("FlameNodeGpu must be non-zero-sized"),
};

// Every per-level dynamic offset must address within `u32`; the deepest slot's
// offset is `(MAX_LEVELS - 1) * 256`, far below `u32::MAX`.
const _: () = assert!((MAX_LEVELS as u64) * LEVEL_PARAMS_STRIDE <= u32::MAX as u64);

/// Registers extraction, the removal companion, pipeline init, per-frame
/// prepare, and the per-level dispatch node for the Flame IFS.
///
/// `Plugin` singleton — add exactly once (done by `SketchesPlugin`). Inert until
/// [`FlameSimParams`] exists in the world (the sketch inserts it on entry), so it
/// costs nothing on other sketches.
pub struct FlameComputePlugin;

impl Plugin for FlameComputePlugin {
    fn build(&self, app: &mut App) {
        // Mirror FlameSimParams into the render world each frame.
        app.add_plugins(ExtractResourcePlugin::<FlameSimParams>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        // ExtractResourcePlugin does NOT propagate removals — manual companion
        // (the established landmine; see cymatics/compute/pipeline.rs).
        render_app.init_resource::<FlameBindGroupCache>();
        render_app.add_systems(ExtractSchedule, remove_flame_sim_params_if_absent);

        render_app
            .add_systems(RenderStartup, init_flame_pipeline)
            .add_systems(
                Render,
                prepare_flame_bind_groups
                    .in_set(RenderSystems::PrepareBindGroups)
                    .run_if(resource_exists::<FlameSimParams>),
            );

        // Run the per-level dispatch in the root `RenderGraph` schedule, before
        // `camera_driver` runs the per-camera schedules — so the node buffer is
        // current before the 2D pass reads it. (Bevy 0.19 render graph is
        // systems-based; see the migration guide's "Render Graph as Systems".)
        render_app.add_systems(RenderGraph, flame_compute.before(camera_driver));
    }
}

/// Cached compute pipeline state. Initialised once in [`RenderStartup`].
#[derive(Resource)]
struct FlamePipeline {
    /// Retained so `prepare_flame_bind_groups` can fetch the [`BindGroupLayout`]
    /// from the [`PipelineCache`] without storing it twice.
    bind_group_layout_descriptor: BindGroupLayoutDescriptor,
    /// Handle into Bevy's [`PipelineCache`].
    pipeline_id: CachedComputePipelineId,
    /// Persistent `UNIFORM | COPY_DST` buffer for the 800-byte
    /// [`FlameSimParamsGpu`]; refilled each frame via `write_buffer` (no realloc).
    sim_params_buffer: Buffer,
    /// Persistent `UNIFORM | COPY_DST` buffer of `MAX_LEVELS` × 256-byte slots;
    /// level `i` binds slot `i` via dynamic offset `i * 256`.
    level_buffer: Buffer,
}

/// Per-frame bind group + per-level dispatch dims, consumed by [`flame_compute`].
/// Removed by `remove_flame_sim_params_if_absent` (private, so a code span) on
/// sketch exit — the held [`BindGroup`] owns an `Arc` reference to the node
/// buffer, so letting it linger would pin the freed buffer's VRAM for the
/// session.
#[derive(Resource)]
struct FlameComputeBindGroups {
    /// Bind group: sim uniform (0), node storage buffer (1), level uniform (2,
    /// dynamic offset). Reused across frames; rebuilt on node-buffer change.
    bind_group: BindGroup,
    /// Per-level `(dynamic offset, workgroup count)`; slot `i` is tree level
    /// `i + 1`. Only the first `level_count` entries are meaningful.
    dispatch: [(u32, u32); MAX_LEVELS],
    /// Levels to dispatch this frame, clamped to `MAX_LEVELS` (the `level_buffer`
    /// slot count) in `prepare_flame_bind_groups`. `0` freezes the fractal.
    level_count: u32,
}

/// One-slot bind-group cache keyed on the node storage buffer's [`BufferId`].
///
/// A render-world `Resource` (not a system `Local`) deliberately: the prepare
/// system stops running once its `run_if(resource_exists::<FlameSimParams>)`
/// gate goes false on sketch exit, so a `Local` slot could never release the
/// old bind group — pinning the freed node buffer's `Arc`. As a resource,
/// `remove_flame_sim_params_if_absent` (private, so a code span) clears it on
/// the same exit seam.
#[derive(Resource, Default)]
struct FlameBindGroupCache(Option<(BufferId, BindGroup)>);

/// Initialises [`FlamePipeline`] in the render-world startup schedule.
///
/// Runs in [`RenderStartup`] (not `FromWorld`) because it needs [`AssetServer`],
/// [`PipelineCache`], and [`RenderDevice`] as system params.
fn init_flame_pipeline(
    mut commands: Commands<'_, '_>,
    asset_server: Res<'_, AssetServer>,
    pipeline_cache: Res<'_, PipelineCache>,
    render_device: Res<'_, RenderDevice>,
) {
    // The dynamic-offset stride must be a multiple of the device's
    // min_uniform_buffer_offset_alignment. WebGPU caps that limit at 256
    // (== LEVEL_PARAMS_STRIDE), so every offset i*256 is aligned on conformant
    // devices. Surface — not silently truncate — the spec-violating case.
    let align = u64::from(render_device.limits().min_uniform_buffer_offset_alignment);
    if align > LEVEL_PARAMS_STRIDE {
        error!(
            "min_uniform_buffer_offset_alignment ({align}) exceeds LEVEL_PARAMS_STRIDE \
             ({LEVEL_PARAMS_STRIDE}); per-level uniform offsets are misaligned. Raise \
             FlameLevelParamsGpu / LEVEL_PARAMS_STRIDE to {align} to match this device."
        );
    }

    let bind_group_layout_descriptor = BindGroupLayoutDescriptor::new(
        "flame_compute_bgl",
        &[
            // binding 0 — SimParams uniform (branch table + warp; constant per
            // frame, no dynamic offset).
            BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::COMPUTE,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: Some(SIM_PARAMS_SIZE),
                },
                count: None,
            },
            // binding 1 — node storage buffer, read_write. Each dispatch reads
            // parent slots and writes its level's slots.
            BindGroupLayoutEntry {
                binding: 1,
                visibility: ShaderStages::COMPUTE,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: Some(NODE_SIZE),
                },
                count: None,
            },
            // binding 2 — per-level LevelParams uniform, bound with a 256-byte
            // dynamic offset (one 16-byte head per dispatched level).
            BindGroupLayoutEntry {
                binding: 2,
                visibility: ShaderStages::COMPUTE,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: Some(LEVEL_PARAMS_SIZE),
                },
                count: None,
            },
        ],
    );

    let shader = asset_server.load::<bevy::shader::Shader>("shaders/flame/simulate.wgsl");

    let pipeline_id = pipeline_cache.queue_compute_pipeline(ComputePipelineDescriptor {
        label: Some(Cow::from("flame_compute_pipeline")),
        layout: vec![bind_group_layout_descriptor.clone()],
        shader,
        entry_point: Some(Cow::from("main")),
        ..default()
    });

    // Allocate both uniform buffers once; each frame `prepare_flame_bind_groups`
    // refills them via `queue.write_buffer` — no per-frame GPU allocation.
    let sim_params_buffer = render_device.create_buffer(&BufferDescriptor {
        label: Some("flame_sim_params_uniform"),
        size: std::mem::size_of::<FlameSimParamsGpu>() as u64,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let level_buffer = render_device.create_buffer(&BufferDescriptor {
        label: Some("flame_level_params_uniform"),
        size: LEVEL_PARAMS_STRIDE * MAX_LEVELS as u64,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    commands.insert_resource(FlamePipeline {
        bind_group_layout_descriptor,
        pipeline_id,
        sim_params_buffer,
        level_buffer,
    });
}

/// Uploads this frame's uniforms and builds (or reuses) the compute bind group,
/// running in [`RenderSystems::PrepareBindGroups`].
///
/// ## No per-frame allocation
///
/// Both uniform buffers are uploaded via `queue.write_buffer` (a staged copy, no
/// allocation). The bind group is cached and reused every frame — rebuilt only
/// when the node storage buffer's [`BufferId`] changes, which now happens only on
/// sketch re-entry (`spawn_flame` allocates a fresh buffer). Name changes no
/// longer touch it: the buffer is seeded once and the compute morphs it in place.
/// The compute runs every active frame, including the multi-hour soak, so
/// rebuilding the bind group per frame would be a steady-state allocation. The
/// cache keys on the node buffer's [`BufferId`]: when it changes the entry is
/// replaced, dropping the old bind group (releasing its reference to the freed
/// buffer) so no stale buffer is retained across a re-entry.
///
/// The cache lives in [`FlameBindGroupCache`], a render-world `Resource`,
/// rather than a system `Local`: this system stops running once its
/// `run_if(resource_exists::<FlameSimParams>)` gate goes false on sketch exit,
/// so a `Local` slot could never release the old bind group — pinning the
/// freed node buffer's VRAM for the rest of the session. As a resource,
/// `remove_flame_sim_params_if_absent` clears the slot on the same exit seam.
fn prepare_flame_bind_groups(
    mut commands: Commands<'_, '_>,
    render_device: Res<'_, RenderDevice>,
    render_queue: Res<'_, RenderQueue>,
    pipeline_cache: Res<'_, PipelineCache>,
    sim: Res<'_, FlameSimParams>,
    buffers: Res<'_, RenderAssets<GpuShaderBuffer>>,
    pipeline: Option<Res<'_, FlamePipeline>>,
    mut cached: ResMut<'_, FlameBindGroupCache>,
) {
    let Some(pipeline) = pipeline else {
        return;
    };
    let Some(gpu_nodes) = buffers.get(&sim.nodes) else {
        return;
    };

    // Constant-per-frame SimParams → its persistent buffer (staged, no alloc).
    render_queue.0.write_buffer(
        &pipeline.sim_params_buffer,
        0,
        bytemuck::bytes_of(&sim.params),
    );

    // Clamp the effective level count to the fixed buffer capacity. The per-level
    // uniform has exactly `MAX_LEVELS` slots; a larger value would index a dynamic
    // offset past the buffer at submit. The dispatched count below is clamped to
    // the same value.
    let level_count = sim.level_count.min(MAX_LEVELS as u32);

    // Each dispatched level's four `u32` fields → the leading 16 bytes of its
    // 256-byte slot (offsets 0, 4, 8, 12, matching WGSL `LevelParams`). The
    // shader reads only those four fields, so the slot padding is left untouched;
    // writing the 16-byte head directly avoids materialising a 256-byte scratch.
    for i in 0..level_count {
        let slot = &sim.levels[i as usize];
        // [level_start, node_count, parent_start, parent_count] — laid out
        // exactly like WGSL `LevelParams`.
        let head: [u32; 4] = [
            slot.level_start,
            slot.node_count,
            slot.parent_start,
            slot.parent_count,
        ];
        let offset = u64::from(i) * LEVEL_PARAMS_STRIDE;
        render_queue
            .0
            .write_buffer(&pipeline.level_buffer, offset, bytemuck::bytes_of(&head));
    }

    // Reuse the bind group while the node storage buffer is unchanged; rebuild +
    // replace (releasing the old buffer reference) when the buffer is swapped,
    // which now only happens on sketch re-entry (a fresh `spawn_flame` alloc).
    let buffer_id = gpu_nodes.buffer.id();
    let bind_group = match &cached.0 {
        Some((id, bg)) if *id == buffer_id => bg.clone(),
        _ => {
            let layout: BindGroupLayout =
                pipeline_cache.get_bind_group_layout(&pipeline.bind_group_layout_descriptor);
            let bg = render_device.create_bind_group(
                "flame_compute_bind_group",
                &layout,
                &[
                    BindGroupEntry {
                        binding: 0,
                        resource: pipeline.sim_params_buffer.as_entire_binding(),
                    },
                    BindGroupEntry {
                        binding: 1,
                        resource: gpu_nodes.buffer.as_entire_binding(),
                    },
                    BindGroupEntry {
                        binding: 2,
                        // Base offset 0; the per-level 256-byte dynamic offset is
                        // applied at `set_bind_group`. Size is one 16-byte head so
                        // each level binds exactly its own slot.
                        resource: BindingResource::Buffer(BufferBinding {
                            buffer: &pipeline.level_buffer,
                            offset: 0,
                            size: Some(LEVEL_PARAMS_SIZE),
                        }),
                    },
                ],
            );
            cached.0 = Some((buffer_id, bg.clone()));
            bg
        }
    };

    // Per-level dispatch dims: `(dynamic offset, ceil(node_count / 256))`. Levels
    // beyond `level_count` stay `(0, 0)` and are never dispatched.
    let mut dispatch = [(0_u32, 0_u32); MAX_LEVELS];
    for i in 0..level_count {
        let workgroups = sim.levels[i as usize].node_count.div_ceil(WORKGROUP_SIZE);
        dispatch[i as usize] = (i * LEVEL_PARAMS_STRIDE_U32, workgroups);
    }

    commands.insert_resource(FlameComputeBindGroups {
        bind_group,
        dispatch,
        level_count,
    });
}

/// Render-graph node: runs one compute pass with `level_count` sequential
/// dispatches, binding each level's 256-byte slot via a dynamic offset.
///
/// Runs in the root [`RenderGraph`] schedule before `camera_driver`. A clean
/// no-op while the bind groups, pipeline, or sim params are absent (sketch
/// inactive), the pipeline is still compiling, or `level_count == 0` (Idle
/// freeze / no work).
///
/// Gates directly on [`FlameSimParams`] (mirroring `particle_compute` /
/// `cymatics_compute`). [`remove_flame_sim_params_if_absent`] removes both
/// that resource and [`FlameComputeBindGroups`] on `OnExit`; the `Option`
/// guards here keep the dispatch a no-op for the one extract cycle before
/// those removals land (and while the pipeline is still compiling).
fn flame_compute(
    bind_groups: Option<Res<'_, FlameComputeBindGroups>>,
    pipeline_res: Option<Res<'_, FlamePipeline>>,
    sim: Option<Res<'_, FlameSimParams>>,
    pipeline_cache: Res<'_, PipelineCache>,
    mut render_context: RenderContext<'_, '_>,
) {
    // No extracted params → the sketch has exited; do not dispatch the lingering
    // bind group (mirrors `particle_compute` / `cymatics_compute`).
    let (Some(bg), Some(pipeline_res), Some(_sim)) = (bind_groups, pipeline_res, sim) else {
        return;
    };
    // Idle freeze / nothing to do: skip the pass entirely (no encoder work).
    if bg.level_count == 0 {
        return;
    }
    let Some(compute_pipeline) = pipeline_cache.get_compute_pipeline(pipeline_res.pipeline_id)
    else {
        return;
    };

    let mut pass = render_context
        .command_encoder()
        .begin_compute_pass(&ComputePassDescriptor {
            label: Some("flame_compute_pass"),
            timestamp_writes: None,
        });
    pass.set_pipeline(compute_pipeline);

    // `level_count` is clamped to `MAX_LEVELS` in prepare; the loop's max dynamic
    // offset is `(MAX_LEVELS - 1) * 256`, inside the buffer.
    debug_assert!(
        bg.level_count <= MAX_LEVELS as u32,
        "level_count must be clamped to MAX_LEVELS before the dispatch loop; \
         the level_buffer has exactly MAX_LEVELS slots and a larger count would \
         index a dynamic offset past the buffer at submit"
    );

    // One dispatch per tree level. WebGPU's implicit ordering between dispatches
    // in a pass makes the parent level's writes visible to the child's reads.
    for i in 0..bg.level_count {
        let (offset, workgroups) = bg.dispatch[i as usize];
        pass.set_bind_group(0, &bg.bind_group, &[offset]);
        pass.dispatch_workgroups(workgroups, 1, 1);
    }
}

/// Removes [`FlameSimParams`] from the render world when the main-world source
/// is absent.
///
/// [`ExtractResourcePlugin`] propagates inserts and updates from the main world
/// to the render world each frame, but it does NOT propagate removals: when
/// `OnExit(AppState::Flame)` removes the main-world [`FlameSimParams`], the
/// render-world copy silently persists, keeping `flame_compute`'s
/// `run_if`/`Option` gate true and dispatching the frozen fractal every frame.
/// This system — added to the render sub-app's [`ExtractSchedule`] alongside the
/// `ExtractResourcePlugin` — fills that gap, mirroring the identical fix in
/// `cymatics` and `particles`.
///
/// Besides the extracted [`FlameSimParams`] copy, this also drops the
/// per-frame [`FlameComputeBindGroups`] and clears the [`FlameBindGroupCache`]
/// slot: both hold a [`BindGroup`] whose `Arc` references pin the sketch's
/// freed node buffer in VRAM, and neither is entity-owned nor re-run once
/// `prepare_flame_bind_groups`'s `run_if` gate goes false — without this, the
/// buffer would be retained for the rest of the session (AGENTS.md
/// GPU-release mechanism 2/3).
fn remove_flame_sim_params_if_absent(
    mut commands: Commands<'_, '_>,
    main_resource: Extract<'_, '_, Option<Res<'_, FlameSimParams>>>,
    render_resource: Option<Res<'_, FlameSimParams>>,
    bind_groups: Option<Res<'_, FlameComputeBindGroups>>,
    mut cache: ResMut<'_, FlameBindGroupCache>,
) {
    if main_resource.is_some() {
        return;
    }
    if render_resource.is_some() {
        commands.remove_resource::<FlameSimParams>();
    }
    if bind_groups.is_some() {
        commands.remove_resource::<FlameComputeBindGroups>();
    }
    if cache.0.is_some() {
        cache.0 = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build-smoke: `FlameComputePlugin` adds cleanly under `MinimalPlugins`
    /// (no `RenderApp`) without panicking. `build` early-returns when
    /// `get_sub_app_mut(RenderApp)` is `None`, so registering it outside a full
    /// render context must be a no-op. Mirrors `cymatics_compute_plugin_builds`.
    #[test]
    fn flame_compute_plugin_builds() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(FlameComputePlugin);
        app.update();
    }

    /// The constants the dynamic-offset path depends on: the u32 stride mirrors
    /// the u64 one, the binding head fits within a slot, and the whole per-level
    /// buffer addresses within u32 (so no offset overflows).
    #[test]
    fn dynamic_offset_constants_are_consistent() {
        assert_eq!(u64::from(LEVEL_PARAMS_STRIDE_U32), LEVEL_PARAMS_STRIDE);
        assert!(LEVEL_PARAMS_SIZE.get() <= LEVEL_PARAMS_STRIDE);
        let last_offset = (MAX_LEVELS as u64 - 1) * LEVEL_PARAMS_STRIDE;
        assert!(u32::try_from(last_offset).is_ok());
    }

    /// Binding 0's `min_binding_size` is the exact `FlameSimParamsGpu` size, so
    /// the layout matches the 800-byte WGSL `SimParams` uniform.
    #[test]
    fn sim_params_min_binding_size_is_800() {
        assert_eq!(SIM_PARAMS_SIZE.get(), 800);
    }

    /// Per-level dispatch math rounds node counts up so the last partial
    /// workgroup is still launched (and bound-checked in the shader).
    #[test]
    fn dispatch_workgroups_round_up() {
        // 5 nodes / 256 = 1 workgroup covering the partial tile.
        assert_eq!(5_u32.div_ceil(WORKGROUP_SIZE), 1);
        // 256 nodes / 256 = exactly 1 workgroup.
        assert_eq!(256_u32.div_ceil(WORKGROUP_SIZE), 1);
        // 257 nodes / 256 = 2 workgroups (last covers 1 node).
        assert_eq!(257_u32.div_ceil(WORKGROUP_SIZE), 2);
    }

    /// The removal companion clears every render-world compute resource when
    /// the main-world source is absent, and leaves them alone while it is
    /// present — this is the seam that releases the node buffer's VRAM on
    /// sketch exit.
    ///
    /// [`FlameComputeBindGroups`] and a populated [`FlameBindGroupCache`] slot
    /// hold wgpu handles that cannot be constructed headless, so this test
    /// exercises the extracted-params removal and verifies the system runs
    /// cleanly with the (empty) cache; the bind-group/cache clears share the
    /// same `main_resource.is_none()` branch asserted here.
    #[test]
    #[allow(clippy::expect_used, reason = "test assertions")]
    fn removal_companion_clears_render_world_on_exit() {
        use bevy::ecs::system::RunSystemOnce;
        use bevy::render::MainWorld;
        use bytemuck::Zeroable;

        let render_params = || FlameSimParams {
            params: FlameSimParamsGpu::zeroed(),
            levels: [super::super::sim_params::FlameLevelParamsGpu::zeroed(); MAX_LEVELS],
            level_count: 0,
            nodes: Handle::default(),
        };

        // Main-world source absent: the render copy must be removed.
        let mut render_world = World::new();
        render_world.insert_resource(MainWorld::default());
        render_world.insert_resource(render_params());
        render_world.init_resource::<FlameBindGroupCache>();
        render_world
            .run_system_once(remove_flame_sim_params_if_absent)
            .expect("companion runs");
        assert!(
            render_world.get_resource::<FlameSimParams>().is_none(),
            "extracted params must be dropped once the main source is gone"
        );
        assert!(
            render_world.resource::<FlameBindGroupCache>().0.is_none(),
            "cache slot stays empty after the clear branch"
        );

        // Main-world source present: everything is left in place.
        let mut render_world = World::new();
        let mut main_world = MainWorld::default();
        main_world.insert_resource(render_params());
        render_world.insert_resource(main_world);
        render_world.insert_resource(render_params());
        render_world.init_resource::<FlameBindGroupCache>();
        render_world
            .run_system_once(remove_flame_sim_params_if_absent)
            .expect("companion runs");
        assert!(
            render_world.get_resource::<FlameSimParams>().is_some(),
            "live sketch must keep its extracted params"
        );
    }
}
