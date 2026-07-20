//! Cymatics ping-pong compute: the render-graph node that advances the wave
//! field N sub-steps per frame. The kernel is `assets/shaders/cymatics/
//! simulate.wgsl`.
//!
//! # Signal / data flow
//!
//! [`CymaticsComputePlugin::build`] wires three pieces into the render world:
//!
//! 1. [`ExtractResourcePlugin`] clones [`CymaticsSimParams`] (the per-frame
//!    uniform, the per-iteration phase scalars, the two ping-pong texture
//!    handles, and the sub-step count) from the main world each frame. The
//!    resource is POD, so the clone allocates nothing.
//! 2. `init_cymatics_pipeline` ([`RenderStartup`]) builds the bind-group
//!    layout, queues the compute pipeline, and allocates the two persistent
//!    uniform buffers (the constant `SimParams` and the `MAX_ITERATIONS`-slot
//!    per-iteration array) **once** — never per frame.
//! 3. `prepare_cymatics_bind_groups` ([`RenderSystems::PrepareBindGroups`])
//!    uploads this frame's uniforms and builds the **two** bind groups — `ab`
//!    (reads A, writes B) and `ba` (reads B, writes A) — cached in
//!    `CymaticsBindGroupCache` keyed on the ping-pong texture views' ids
//!    (bounded by construction: one slot, replaced on change, and *cleared* by
//!    the removal companion on sketch exit so the freed A/B textures' `Arc`s
//!    are not pinned for the rest of the session).
//! 4. `cymatics_compute` runs in the root [`RenderGraph`] schedule before
//!    `camera_driver`, so the field is current before the 2D pass samples it.
//!    It dispatches the kernel `iterations` times, alternating `ab`/`ba` and
//!    binding each sub-step's 256-byte slot via a dynamic offset. The render
//!    material samples texture A directly, so no display blit is needed — the
//!    only copy is the odd-N B → A continuity refresh below.
//!
//! # Ping-pong contract
//!
//! `read_tex` is a sampled `texture_2d<f32>` (read via `textureLoad`); `write_tex`
//! is a `texture_storage_2d<rgba32float, write>`. Write-only storage (not
//! `read_write`) keeps us off a downlevel feature on the WebGPU-only target; the
//! A/B alternation is what supplies read-from-one / write-to-the-other. After
//! `iterations` sub-steps the freshest field is in B when the count is odd and
//! A when it is even; the odd-N refresh (below) copies B → A so A always holds
//! the latest field at frame end, and the render material samples A regardless
//! of parity.
//!
//! # Cross-frame continuity (and the render source)
//!
//! A and B are persistent `RENDER_WORLD` textures that survive across frames,
//! and the sub-step loop **always reads A first** (`ab`). So both the next
//! frame's read-A start AND this frame's render-from-A depend on the invariant
//! "texture A holds the latest simulation state at frame end". It holds
//! automatically for even `iterations` (the last write lands in A); for odd
//! `iterations` the last write lands in B, so the node copies B → A. Because the
//! material samples A directly (the byte-identical display blit was removed),
//! this single refresh serves both purposes. `frame_blit_plan` decides whether
//! the refresh is needed from the sub-step count; default `iterations` is even,
//! so the copy is off in the shipping config.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "usize/u64/u32 casts for GPU buffer sizes and dynamic offsets are \
              intentional and bounds-checked (MAX_ITERATIONS * 256 fits in u32)"
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
    BufferUsages, CachedComputePipelineId, ComputePassDescriptor, ComputePipelineDescriptor,
    Extent3d, PipelineCache, ShaderStages, StorageTextureAccess, TextureFormat, TextureSampleType,
    TextureViewDimension, TextureViewId,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderGraph, RenderQueue};
use bevy::render::texture::GpuImage;
use bevy::render::{Extract, ExtractSchedule, Render, RenderApp, RenderStartup, RenderSystems};

use super::sim_params::{
    CymaticsSimParams, IterParamsGpu, SimParamsGpu, ITER_PARAMS_STRIDE, MAX_ITERATIONS,
};

/// Workgroup size; must match `@workgroup_size(8, 8, 1)` in `simulate.wgsl`.
const WORKGROUP_SIZE: u32 = 8;

/// Cache entry for the two ping-pong bind groups: the `(A view, B view)` id
/// pair they were built against, plus the `ab` and `ba` bind groups. Held in a
/// [`Local`] by `prepare_cymatics_bind_groups`; rebuilt when the key changes.
type CachedBindGroups = ((TextureViewId, TextureViewId), BindGroup, BindGroup);

/// `SimParamsGpu` byte size for binding 0's `min_binding_size`.
///
/// `SimParamsGpu` has fields, so it is non-zero-sized; the `panic!` is in a
/// `const` expression and so could only fire at compile time, never at runtime.
const SIM_PARAMS_SIZE: NonZeroU64 = match NonZeroU64::new(std::mem::size_of::<SimParamsGpu>() as u64)
{
    Some(n) => n,
    None => panic!("SimParamsGpu must be non-zero-sized"),
};

/// `IterParamsGpu` byte size (== [`ITER_PARAMS_STRIDE`]) for binding 3's
/// `min_binding_size` and each per-iteration `BufferBinding`'s size.
const ITER_PARAMS_SIZE: NonZeroU64 =
    match NonZeroU64::new(std::mem::size_of::<IterParamsGpu>() as u64) {
        Some(n) => n,
        None => panic!("IterParamsGpu must be non-zero-sized"),
    };

/// [`ITER_PARAMS_STRIDE`] as `u32` for the per-iteration dynamic-offset math.
const ITER_PARAMS_STRIDE_U32: u32 = ITER_PARAMS_STRIDE as u32;

// The full per-iteration buffer (and so every dynamic offset) must address
// within `u32`; MAX_ITERATIONS * 256 = 30720, far below u32::MAX.
const _: () = assert!(ITER_PARAMS_STRIDE == ITER_PARAMS_SIZE.get());
const _: () = assert!((MAX_ITERATIONS as u64) * ITER_PARAMS_STRIDE <= u32::MAX as u64);

/// Registers the Cymatics compute pipeline + render-graph node.
///
/// `Plugin` singleton — add exactly once (done by `SketchesPlugin`). Inert until
/// [`CymaticsSimParams`] exists in the world (the sketch inserts it on entry),
/// so it costs nothing on other sketches.
pub struct CymaticsComputePlugin;

impl Plugin for CymaticsComputePlugin {
    fn build(&self, app: &mut App) {
        // Mirror CymaticsSimParams into the render world each frame.
        app.add_plugins(ExtractResourcePlugin::<CymaticsSimParams>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        // Explicitly remove the render-world copy when the main-world resource
        // is gone. `ExtractResourcePlugin` propagates inserts and updates but
        // NOT removals (verified against bevy_render 0.19 extract_resource.rs —
        // the None branch is a complete no-op). Without this, `CymaticsSimParams`
        // lingers in the render world after `OnExit(AppState::Cymatics)`, causing
        // `cymatics_compute`'s `run_if(resource_exists::<CymaticsSimParams>)` to
        // stay true and keep dispatching the N-sub-step compute pass on whatever
        // sketch is now showing — wasting GPU and thermal budget.
        render_app.init_resource::<CymaticsBindGroupCache>();
        render_app.add_systems(ExtractSchedule, remove_cymatics_sim_params_if_absent);

        render_app
            .add_systems(RenderStartup, init_cymatics_pipeline)
            .add_systems(
                Render,
                prepare_cymatics_bind_groups
                    .in_set(RenderSystems::PrepareBindGroups)
                    .run_if(resource_exists::<CymaticsSimParams>),
            );

        // Run the N-iteration dispatch in the root `RenderGraph` schedule, before
        // `camera_driver` runs the per-camera schedules — so texture A holds the
        // current field before the 2D pass samples it. (Bevy 0.19 render graph is
        // systems-based; see the migration guide's "Render Graph as Systems".)
        render_app.add_systems(RenderGraph, cymatics_compute.before(camera_driver));
    }
}

/// Cached compute pipeline state. Initialised once in [`RenderStartup`].
#[derive(Resource)]
struct CymaticsPipeline {
    /// Retained so `prepare_cymatics_bind_groups` can fetch the
    /// [`BindGroupLayout`] from the [`PipelineCache`] without storing it twice.
    bind_group_layout_descriptor: BindGroupLayoutDescriptor,
    /// Handle into Bevy's [`PipelineCache`].
    pipeline_id: CachedComputePipelineId,
    /// Persistent `UNIFORM | COPY_DST` buffer for the constant-per-frame
    /// [`SimParamsGpu`]; refilled each frame via `write_buffer` (no realloc).
    sim_params_buffer: Buffer,
    /// Persistent `UNIFORM | COPY_DST` buffer of `MAX_ITERATIONS` × 256-byte
    /// slots; iteration `i` binds slot `i` via dynamic offset `i * 256`.
    iter_buffer: Buffer,
}

/// Per-frame bind groups + dispatch dims, consumed by [`cymatics_compute`].
/// Removed by `remove_cymatics_sim_params_if_absent` (private, so a code
/// span) on sketch exit — the held `ab`/`ba` [`BindGroup`]s each own an `Arc`
/// reference to the ping-pong A/B textures, so letting them linger would pin
/// the freed textures' VRAM for the session.
#[derive(Resource)]
struct CymaticsComputeBindGroups {
    /// Reads A, writes B — used on even sub-steps.
    ab: BindGroup,
    /// Reads B, writes A — used on odd sub-steps.
    ba: BindGroup,
    /// `ceil(resolution.x / WORKGROUP_SIZE)`.
    dispatch_x: u32,
    /// `ceil(resolution.y / WORKGROUP_SIZE)`.
    dispatch_y: u32,
    /// Sub-steps to run this frame, clamped to `MAX_ITERATIONS` (the
    /// `iter_buffer` slot count) in `prepare_cymatics_bind_groups`.
    iterations: u32,
}

/// One-slot bind-group cache keyed on the `(A view, B view)` id pair.
///
/// A render-world `Resource` (not a system `Local`) deliberately: the prepare
/// system stops running once its `run_if(resource_exists::<CymaticsSimParams>)`
/// gate goes false on sketch exit, so a `Local` slot could never release the
/// old `ab`/`ba` bind groups — pinning the freed A/B textures' `Arc`s. As a
/// resource, `remove_cymatics_sim_params_if_absent` (private, so a code span)
/// clears it on the same exit seam.
#[derive(Resource, Default)]
struct CymaticsBindGroupCache(Option<CachedBindGroups>);

/// Initialises [`CymaticsPipeline`] in the render-world startup schedule.
///
/// Runs in [`RenderStartup`] (not `FromWorld`) because it needs [`AssetServer`],
/// [`PipelineCache`], and [`RenderDevice`] as system params. The pipeline
/// *compile* is where `rgba32float` write-only storage support is actually
/// exercised — if the device rejected the format the queued pipeline would fail
/// at `PipelineCache` time, surfaced via [`cymatics_compute`]'s missing-pipeline
/// no-op (and Bevy's pipeline-error logging), not silently swallowed.
fn init_cymatics_pipeline(
    mut commands: Commands<'_, '_>,
    asset_server: Res<'_, AssetServer>,
    pipeline_cache: Res<'_, PipelineCache>,
    render_device: Res<'_, RenderDevice>,
) {
    // The dynamic-offset stride must be a multiple of the device's
    // min_uniform_buffer_offset_alignment. WebGPU caps that limit at 256
    // (== ITER_PARAMS_STRIDE), so every offset i*256 is aligned on conformant
    // devices. Surface — not silently truncate — the spec-violating case.
    let align = u64::from(render_device.limits().min_uniform_buffer_offset_alignment);
    if align > ITER_PARAMS_STRIDE {
        error!(
            "min_uniform_buffer_offset_alignment ({align}) exceeds ITER_PARAMS_STRIDE \
             ({ITER_PARAMS_STRIDE}); per-iteration uniform offsets are misaligned. Raise \
             IterParamsGpu / ITER_PARAMS_STRIDE to {align} to match this device."
        );
    }

    let bind_group_layout_descriptor = BindGroupLayoutDescriptor::new(
        "cymatics_compute_bgl",
        &[
            // binding 0 — SimParams uniform (constant per frame, no dynamic offset).
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
            // binding 1 — read texture. Read only via `textureLoad` (no
            // filtering), so `filterable: false` — this keeps the compute read
            // path off the `float32-filterable` feature requirement.
            BindGroupLayoutEntry {
                binding: 1,
                visibility: ShaderStages::COMPUTE,
                ty: BindingType::Texture {
                    sample_type: TextureSampleType::Float { filterable: false },
                    view_dimension: TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            // binding 2 — write texture. `rgba32float`, write-only storage. NOT
            // read_write (that needs a downlevel feature we avoid); the ping-pong
            // supplies read-from-A / write-to-B instead.
            BindGroupLayoutEntry {
                binding: 2,
                visibility: ShaderStages::COMPUTE,
                ty: BindingType::StorageTexture {
                    access: StorageTextureAccess::WriteOnly,
                    format: TextureFormat::Rgba32Float,
                    view_dimension: TextureViewDimension::D2,
                },
                count: None,
            },
            // binding 3 — per-iteration phase uniform, bound with a 256-byte
            // dynamic offset (one IterParamsGpu slot per sub-step).
            BindGroupLayoutEntry {
                binding: 3,
                visibility: ShaderStages::COMPUTE,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: Some(ITER_PARAMS_SIZE),
                },
                count: None,
            },
        ],
    );

    let shader = asset_server.load::<bevy::shader::Shader>("shaders/cymatics/simulate.wgsl");

    let pipeline_id = pipeline_cache.queue_compute_pipeline(ComputePipelineDescriptor {
        label: Some(Cow::from("cymatics_compute_pipeline")),
        layout: vec![bind_group_layout_descriptor.clone()],
        shader,
        entry_point: Some(Cow::from("main")),
        ..default()
    });

    // Allocate both uniform buffers once; each frame `prepare_cymatics_bind_groups`
    // refills them via `queue.write_buffer` — no per-frame GPU allocation.
    let sim_params_buffer = render_device.create_buffer(&BufferDescriptor {
        label: Some("cymatics_sim_params_uniform"),
        size: std::mem::size_of::<SimParamsGpu>() as u64,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let iter_buffer = render_device.create_buffer(&BufferDescriptor {
        label: Some("cymatics_iter_params_uniform"),
        size: ITER_PARAMS_STRIDE * MAX_ITERATIONS as u64,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    commands.insert_resource(CymaticsPipeline {
        bind_group_layout_descriptor,
        pipeline_id,
        sim_params_buffer,
        iter_buffer,
    });
}

/// Uploads this frame's uniforms and builds (or reuses) the two ping-pong bind
/// groups, running in [`RenderSystems::PrepareBindGroups`].
///
/// ## No per-frame allocation
///
/// Both uniform buffers are uploaded via `queue.write_buffer` (a staged copy,
/// no allocation). The two bind groups are cached and reused every frame —
/// rebuilt only when a ping-pong texture view changes (sketch re-entry / resize
/// reallocates the `GpuImage`). The compute runs every active frame, including
/// the multi-hour idle soak, so rebuilding the bind groups per frame would be a
/// steady-state allocation. The cache keys on the pair of [`TextureViewId`]s
/// (mirroring `hand_mesh::bone_composite`): when a view id changes the entry is
/// replaced, dropping the old bind groups (releasing their references to the
/// freed texture) so no stale view is retained across a re-entry.
///
/// The cache lives in [`CymaticsBindGroupCache`], a render-world `Resource`,
/// rather than a system `Local`: this system stops running once its
/// `run_if(resource_exists::<CymaticsSimParams>)` gate goes false on sketch
/// exit, so a `Local` slot could never release the old bind groups — pinning
/// the freed A/B textures' VRAM for the rest of the session. As a resource,
/// `remove_cymatics_sim_params_if_absent` clears the slot on the same exit
/// seam.
fn prepare_cymatics_bind_groups(
    mut commands: Commands<'_, '_>,
    render_device: Res<'_, RenderDevice>,
    render_queue: Res<'_, RenderQueue>,
    pipeline_cache: Res<'_, PipelineCache>,
    sim: Res<'_, CymaticsSimParams>,
    images: Res<'_, RenderAssets<GpuImage>>,
    pipeline: Option<Res<'_, CymaticsPipeline>>,
    mut cached: ResMut<'_, CymaticsBindGroupCache>,
) {
    let Some(pipeline) = pipeline else {
        return;
    };
    let (Some(gpu_a), Some(gpu_b)) = (images.get(&sim.tex_a), images.get(&sim.tex_b)) else {
        return;
    };

    // Constant-per-frame SimParams → its persistent buffer (staged, no alloc).
    render_queue.0.write_buffer(
        &pipeline.sim_params_buffer,
        0,
        bytemuck::bytes_of(&sim.params),
    );

    // Each sub-step's `(time, wave_signal, wave_signal2)` → the leading three
    // f32s of its 256-byte slot (offsets 0, 4, 8, matching `IterParamsGpu`). The
    // shader reads only those three fields, so the slot padding is left
    // untouched; writing the 12-byte head directly avoids materialising a
    // 256-byte scratch.
    //
    // The two clocks share the per-sub-step increment `phase_dt` but are carried
    // separately so each stays bounded over a multi-hour soak (see
    // `CymaticsSimParams`):
    //   - `phase = phase_base + i·phase_dt` is the oscillator phase, wrapped mod
    //     TAU upstream; the active source value is `source_amplitude·sin(phase)`,
    //     hoisted out of the per-cell shader (uniform across the dispatch).
    //     Wrapping keeps `sin`'s argument small and precise.
    //   - `ramp_t = ramp_base + i·phase_dt` is the bounded alive-bloom clock fed
    //     to the shader's `IterParams.time` (its `(time-500)/500` ramp); it needs
    //     elapsed time, not phase, so it is NOT wrapped (just capped upstream).
    //
    // The source mode is branched ONCE outside the per-slot loop:
    //   - `ping_mode == 0` (active): both centres get the SAME shared oscillator
    //     value (written into both signal lanes), so the simulation is
    //     byte-identical to the pre-raindrop single-source path.
    //   - `ping_mode == 1` (screensaver): each centre gets its own raindrop Hann
    //     envelope, evaluated per sub-step at `ping_base[c] + i` so the ring
    //     expansion is locked to sub-steps (fps-independent).
    //
    // The slot count is clamped to `MAX_ITERATIONS` — the `iter_buffer`'s exact
    // slot count — so a malformed sub-step count can never `write_buffer` past
    // the buffer end; the dispatched count below is clamped to the same value.
    // `u16` holds MAX_ITERATIONS (120) and gives a lossless, lint-clean index → f32.
    let slot_count = u16::try_from(sim.iterations.min(MAX_ITERATIONS as u32)).unwrap_or(0);
    let screensaver = sim.ping_mode == 1;
    for i in 0..slot_count {
        let step = f32::from(i);
        let ramp_t = sim.ramp_base + step * sim.phase_dt;
        // [time, wave_signal, wave_signal2] — laid out exactly like IterParamsGpu's head.
        let head: [f32; 3] = if screensaver {
            // Each centre's independent raindrop envelope at this sub-step's tick.
            let h1 = ping_envelope(sim.ping_base[0] + step, sim.ping_duration, sim.ping_amp[0]);
            let h2 = ping_envelope(sim.ping_base[1] + step, sim.ping_duration, sim.ping_amp[1]);
            [ramp_t, h1, h2]
        } else {
            // Shared continuous oscillator at both centres (byte-identical path).
            let phase = sim.phase_base + step * sim.phase_dt;
            let s = sim.source_amplitude * phase.sin();
            [ramp_t, s, s]
        };
        let offset = u64::from(i) * ITER_PARAMS_STRIDE;
        render_queue
            .0
            .write_buffer(&pipeline.iter_buffer, offset, bytemuck::bytes_of(&head));
    }

    // Rebuild the bind groups only on a texture-view change; reuse otherwise.
    let key = (gpu_a.texture_view.id(), gpu_b.texture_view.id());
    let (ab, ba) = match &cached.0 {
        Some((cached_key, ab, ba)) if *cached_key == key => (ab.clone(), ba.clone()),
        _ => {
            let layout: BindGroupLayout =
                pipeline_cache.get_bind_group_layout(&pipeline.bind_group_layout_descriptor);
            let make = |read: &GpuImage, write: &GpuImage| {
                render_device.create_bind_group(
                    "cymatics_compute_bind_group",
                    &layout,
                    &[
                        BindGroupEntry {
                            binding: 0,
                            resource: pipeline.sim_params_buffer.as_entire_binding(),
                        },
                        BindGroupEntry {
                            binding: 1,
                            resource: BindingResource::TextureView(&read.texture_view),
                        },
                        BindGroupEntry {
                            binding: 2,
                            resource: BindingResource::TextureView(&write.texture_view),
                        },
                        BindGroupEntry {
                            binding: 3,
                            // Base offset 0; the per-sub-step 256-byte dynamic
                            // offset is applied at `set_bind_group`. Size is one
                            // slot so each sub-step binds exactly its own slot.
                            resource: BindingResource::Buffer(BufferBinding {
                                buffer: &pipeline.iter_buffer,
                                offset: 0,
                                size: Some(ITER_PARAMS_SIZE),
                            }),
                        },
                    ],
                )
            };
            let ab = make(gpu_a, gpu_b);
            let ba = make(gpu_b, gpu_a);
            cached.0 = Some((key, ab.clone(), ba.clone()));
            (ab, ba)
        }
    };

    // Dispatch dims track the resolution setting; recomputed each frame (cheap,
    // no allocation). Bound-checked in the shader against `resolution`.
    let dispatch_x = sim.resolution.x.div_ceil(WORKGROUP_SIZE);
    let dispatch_y = sim.resolution.y.div_ceil(WORKGROUP_SIZE);

    // Clamp the effective sub-step count to the fixed buffer capacity. The
    // per-iteration uniform has exactly `MAX_ITERATIONS` slots; a larger value
    // (a malformed Dev setting) would index past the buffer via the loop's
    // dynamic offsets at submit. Matches the `.take(MAX_ITERATIONS)` write bound
    // above so the dispatched count never exceeds the slots actually uploaded.
    let iterations = sim.iterations.min(MAX_ITERATIONS as u32);
    commands.insert_resource(CymaticsComputeBindGroups {
        ab,
        ba,
        dispatch_x,
        dispatch_y,
        iterations,
    });
}

/// Render-graph node: dispatches the kernel `iterations` times, alternating
/// bind groups, then (for odd `iterations`) copies B → A so A holds the latest
/// field at frame end. The render material samples A directly — no display blit.
///
/// Runs in the root [`RenderGraph`] schedule before `camera_driver`. A clean
/// no-op while the bind groups, pipeline, or sim params are absent (sketch
/// inactive) or the pipeline is still compiling.
///
/// Gates directly on [`CymaticsSimParams`] (mirroring `particle_compute` /
/// `flame_compute`). [`remove_cymatics_sim_params_if_absent`] removes both
/// that resource and [`CymaticsComputeBindGroups`] on `OnExit`; the `Option`
/// guards here keep the dispatch a no-op for the one extract cycle before
/// those removals land (and while the pipeline is still compiling).
fn cymatics_compute(
    bind_groups: Option<Res<'_, CymaticsComputeBindGroups>>,
    pipeline_res: Option<Res<'_, CymaticsPipeline>>,
    pipeline_cache: Res<'_, PipelineCache>,
    sim: Option<Res<'_, CymaticsSimParams>>,
    images: Res<'_, RenderAssets<GpuImage>>,
    mut render_context: RenderContext<'_, '_>,
) {
    let (Some(bg), Some(pipeline_res), Some(sim)) = (bind_groups, pipeline_res, sim) else {
        return;
    };
    let Some(compute_pipeline) = pipeline_cache.get_compute_pipeline(pipeline_res.pipeline_id)
    else {
        return;
    };

    // Scope the compute pass so it (and its borrow of the encoder) is dropped
    // before the blit reuses the encoder below.
    {
        let mut pass =
            render_context
                .command_encoder()
                .begin_compute_pass(&ComputePassDescriptor {
                    label: Some("cymatics_compute_pass"),
                    timestamp_writes: None,
                });
        pass.set_pipeline(compute_pipeline);

        // `iterations` is clamped to `MAX_ITERATIONS` in prepare; the loop's max
        // dynamic offset is `(MAX_ITERATIONS - 1) * 256`, inside the buffer.
        debug_assert!(
            bg.iterations <= MAX_ITERATIONS as u32,
            "iterations must be clamped to MAX_ITERATIONS before the dispatch loop; \
             the iter_buffer has exactly MAX_ITERATIONS slots and a larger count \
             would index a dynamic offset past the buffer at submit"
        );

        // N sub-steps. Even i reads A / writes B (`ab`), odd i reads B / writes A
        // (`ba`); each binds its own IterParams slot via dynamic offset i*256.
        for i in 0..bg.iterations {
            let group = if i % 2 == 0 { &bg.ab } else { &bg.ba };
            let dynamic_offset = [i * ITER_PARAMS_STRIDE_U32];
            pass.set_bind_group(0, group, &dynamic_offset);
            pass.dispatch_workgroups(bg.dispatch_x, bg.dispatch_y, 1);
        }
    }

    // Odd sub-step count: the last write landed in B, but both the next frame's
    // loop and this frame's render-from-A read A first. Copy B → A so A holds
    // the latest state at frame end. Requires COPY_DST on A (set in C5). Even
    // counts (the default) already leave the latest state in A — no copy, no
    // overhead in the shipping config. See `frame_blit_plan` for the parity
    // reasoning.
    if frame_blit_plan(bg.iterations) {
        if let (Some(src), Some(dst)) = (images.get(&sim.tex_b), images.get(&sim.tex_a)) {
            let extent = Extent3d {
                width: sim.resolution.x,
                height: sim.resolution.y,
                depth_or_array_layers: 1,
            };
            render_context.command_encoder().copy_texture_to_texture(
                src.texture.as_image_copy(),
                dst.texture.as_image_copy(),
                extent,
            );
        }
    }
}

/// Removes [`CymaticsSimParams`] from the render world when the main-world
/// source is absent.
///
/// [`ExtractResourcePlugin`] propagates inserts and updates from the main world
/// to the render world each frame, but it does NOT propagate removals: when
/// `OnExit(AppState::Cymatics)` removes the main-world [`CymaticsSimParams`],
/// the render-world copy silently persists. This system — added to the render
/// sub-app's [`ExtractSchedule`] alongside the `ExtractResourcePlugin` — fills
/// that gap. It mirrors the identical fix in `dots::post_process` and
/// `line::post_process`.
///
/// When the render-world copy is absent the `cymatics_compute` node's
/// `run_if(resource_exists::<CymaticsSimParams>)` gate becomes false, stopping
/// all N-sub-step dispatches. The `Handle<Image>` clones of A/B held inside the
/// resource are also dropped, releasing the asset reference counts.
///
/// Besides the extracted [`CymaticsSimParams`] copy, this also drops the
/// per-frame [`CymaticsComputeBindGroups`] and clears the
/// [`CymaticsBindGroupCache`] slot: both hold `ab`/`ba` [`BindGroup`]s whose
/// `Arc` references pin the sketch's freed A/B ping-pong textures (~12.5 MiB)
/// in VRAM, and neither is entity-owned nor re-run once
/// `prepare_cymatics_bind_groups`'s `run_if` gate goes false — without this,
/// the textures would be retained until sketch re-entry (AGENTS.md
/// GPU-release mechanism 2/3). This mirrors the fix landed for
/// `radiance::compute::pipeline`; `hand_mesh::bone_composite`'s equivalent
/// cache is a separate, still-open item.
fn remove_cymatics_sim_params_if_absent(
    mut commands: Commands<'_, '_>,
    main_resource: Extract<'_, '_, Option<Res<'_, CymaticsSimParams>>>,
    render_resource: Option<Res<'_, CymaticsSimParams>>,
    bind_groups: Option<Res<'_, CymaticsComputeBindGroups>>,
    mut cache: ResMut<'_, CymaticsBindGroupCache>,
) {
    if main_resource.is_some() {
        return;
    }
    if render_resource.is_some() {
        commands.remove_resource::<CymaticsSimParams>();
    }
    if bind_groups.is_some() {
        commands.remove_resource::<CymaticsComputeBindGroups>();
    }
    if cache.0.is_some() {
        cache.0 = None;
    }
}

/// Returns whether the odd-N B → A continuity refresh is needed for an
/// `iterations`-sub-step frame.
///
/// The loop starts `ab` (i=0 writes B, i=1 writes A, …), so the last write lands
/// in B for odd counts and A for even counts (including 0, where no sub-step ran
/// and A still holds the prior state). The persistent texture A must hold the
/// latest state at frame end — both the next frame's loop and this frame's
/// render-from-A read A first. So we copy B → A exactly when the final write
/// landed in B (odd `iterations`); for even counts A is already current.
///
/// Pure so the cross-frame continuity contract is unit-testable without a GPU;
/// [`cymatics_compute`] uses it directly, so the test guards the real path.
fn frame_blit_plan(iterations: u32) -> bool {
    iterations % 2 == 1
}

/// Single-ring raindrop envelope: one raised-cosine (Hann) lobe of peak height
/// `strength` over `[0, duration)` sub-step ticks, zero everywhere else.
///
/// `strength · sin²(π·tick/duration)` is `0` at `tick = 0` and `tick = duration`
/// and peaks at `tick = duration/2`, so one fire seeds a single smooth
/// up-and-back source displacement that launches exactly one outgoing ring; the
/// medium then rings down via the existing `velocity_decay` / `height_decay`.
/// Outside the window — and for a non-positive `duration` — the source is quiet.
///
/// Pure so the envelope shape is unit-testable without a GPU; the prepare loop
/// calls it per sub-step in screensaver mode, so the test guards the real path.
fn ping_envelope(tick: f32, duration: f32, strength: f32) -> f32 {
    if duration <= 0.0 || tick < 0.0 || tick >= duration {
        return 0.0;
    }
    let s = (std::f32::consts::PI * tick / duration).sin();
    strength * s * s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build-smoke: `CymaticsComputePlugin` adds cleanly under `MinimalPlugins`
    /// (no `RenderApp`) without panicking. `build` early-returns when
    /// `get_sub_app_mut(RenderApp)` is `None`, so registering it outside a full
    /// render context must be a no-op. Mirrors the `particle_compute_plugin_builds`
    /// and `hand_mesh_composite_plugin_builds` smoke tests.
    #[test]
    fn cymatics_compute_plugin_builds() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(CymaticsComputePlugin);
        app.update();
    }

    /// The constants the dynamic-offset path depends on: the u32 stride mirrors
    /// the u64 one, the buffer size matches the slot stride, and the whole
    /// per-iteration buffer addresses within u32 (so no offset overflows).
    #[test]
    fn dynamic_offset_constants_are_consistent() {
        assert_eq!(u64::from(ITER_PARAMS_STRIDE_U32), ITER_PARAMS_STRIDE);
        assert_eq!(ITER_PARAMS_SIZE.get(), ITER_PARAMS_STRIDE);
        let last_offset = (MAX_ITERATIONS as u64 - 1) * ITER_PARAMS_STRIDE;
        assert!(u32::try_from(last_offset).is_ok());
    }

    /// Cross-frame ping-pong continuity: the odd-N refresh must restore the
    /// "texture A holds the latest state at frame end" invariant for every
    /// parity. The loop reads A first, so odd counts (final write in B) need the
    /// B → A copy; even counts (and 0) already leave the latest in A. This is the
    /// regression guard for the odd-N divergence the even-default visual capture
    /// would miss — and now also the source the render material samples.
    #[test]
    fn frame_blit_plan_restores_continuity() {
        // Odd: freshest in B, refresh A from B. N=1 is the degenerate case where
        // only `ab` ever runs, so A would otherwise never be written.
        assert!(frame_blit_plan(1));
        assert!(frame_blit_plan(3));
        // Even: freshest already in A, no extra copy. 20 is the shipping default.
        assert!(!frame_blit_plan(2));
        assert!(!frame_blit_plan(20));
        // Zero sub-steps: no dispatch ran, A still holds the prior state. No
        // copy — handled like any even count.
        assert!(!frame_blit_plan(0));
    }

    /// `ping_envelope` is a single Hann lobe: `0` at both window edges (tick `0`
    /// and tick `D`), peak `strength` at the centre (`D/2`), monotone up then
    /// down, never negative, and silent outside `[0, D)` and for a non-positive
    /// duration. This is the raindrop's one-ring source displacement shape.
    #[test]
    fn ping_envelope_is_a_single_hann_lobe() {
        let d = 30.0_f32;
        let strength = 4.0_f32;
        // Zero at both window edges.
        assert!(
            ping_envelope(0.0, d, strength).abs() < 1e-6,
            "envelope must be 0 at tick 0"
        );
        assert!(
            ping_envelope(d, d, strength).abs() < 1e-6,
            "envelope must be 0 at tick D (window closed)"
        );
        // Peak at the centre.
        assert!(
            (ping_envelope(d / 2.0, d, strength) - strength).abs() < 1e-4,
            "envelope must peak at `strength` at D/2"
        );
        // Monotone rise then fall around the peak.
        assert!(ping_envelope(d * 0.25, d, strength) < ping_envelope(d * 0.5, d, strength));
        assert!(ping_envelope(d * 0.75, d, strength) < ping_envelope(d * 0.5, d, strength));
        // Silent outside the window and for a degenerate duration (the guard
        // returns a literal 0.0, so this is an exact-zero check via epsilon).
        assert!(
            ping_envelope(-1.0, d, strength).abs() < f32::EPSILON,
            "negative tick is silent"
        );
        assert!(
            ping_envelope(d + 1.0, d, strength).abs() < f32::EPSILON,
            "past the window is silent"
        );
        assert!(
            ping_envelope(5.0, 0.0, strength).abs() < f32::EPSILON,
            "non-positive duration is silent"
        );
        // A Hann lobe is non-negative across the whole window.
        for k in 0..=30u16 {
            let t = f32::from(k);
            assert!(
                ping_envelope(t, d, strength) >= 0.0,
                "envelope went negative"
            );
        }
    }

    /// Dispatch math covers a non-multiple-of-8 resolution by rounding up, so
    /// the last partial tile is still launched (and bound-checked in the shader).
    #[test]
    fn dispatch_dims_round_up() {
        assert_eq!(1920_u32.div_ceil(WORKGROUP_SIZE), 240);
        assert_eq!(1080_u32.div_ceil(WORKGROUP_SIZE), 135);
        // Non-multiple: 1023 cells / 8 = 128 tiles (last tile covers 7 cells).
        assert_eq!(1023_u32.div_ceil(WORKGROUP_SIZE), 128);
        assert_eq!(1_u32.div_ceil(WORKGROUP_SIZE), 1);
    }

    /// The removal companion clears every render-world compute resource when
    /// the main-world source is absent, and leaves them alone while it is
    /// present — this is the seam that releases the ping-pong A/B textures'
    /// VRAM on sketch exit.
    ///
    /// [`CymaticsComputeBindGroups`] and a populated [`CymaticsBindGroupCache`]
    /// slot hold wgpu handles that cannot be constructed headless, so this
    /// test exercises the extracted-params removal and verifies the system
    /// runs cleanly with the (empty) cache; the bind-group/cache clears share
    /// the same `main_resource.is_none()` branch asserted here.
    #[test]
    #[allow(clippy::expect_used, reason = "test assertions")]
    fn removal_companion_clears_render_world_on_exit() {
        use bevy::ecs::system::RunSystemOnce;
        use bevy::render::MainWorld;

        let render_params = || CymaticsSimParams {
            params: SimParamsGpu::default(),
            phase_base: 0.0,
            ramp_base: 0.0,
            phase_dt: 0.0,
            source_amplitude: 2.0,
            iterations: 20,
            ping_mode: 0,
            ping_base: [0.0, 0.0],
            ping_amp: [0.0, 0.0],
            ping_duration: 0.0,
            tex_a: Handle::default(),
            tex_b: Handle::default(),
            resolution: UVec2::new(512, 512),
        };

        // Main-world source absent: the render copy must be removed.
        let mut render_world = World::new();
        render_world.insert_resource(MainWorld::default());
        render_world.insert_resource(render_params());
        render_world.init_resource::<CymaticsBindGroupCache>();
        render_world
            .run_system_once(remove_cymatics_sim_params_if_absent)
            .expect("companion runs");
        assert!(
            render_world.get_resource::<CymaticsSimParams>().is_none(),
            "extracted params must be dropped once the main source is gone"
        );
        assert!(
            render_world
                .resource::<CymaticsBindGroupCache>()
                .0
                .is_none(),
            "cache slot stays empty after the clear branch"
        );

        // Main-world source present: everything is left in place.
        let mut render_world = World::new();
        let mut main_world = MainWorld::default();
        main_world.insert_resource(render_params());
        render_world.insert_resource(main_world);
        render_world.insert_resource(render_params());
        render_world.init_resource::<CymaticsBindGroupCache>();
        render_world
            .run_system_once(remove_cymatics_sim_params_if_absent)
            .expect("companion runs");
        assert!(
            render_world.get_resource::<CymaticsSimParams>().is_some(),
            "live sketch must keep its extracted params"
        );
    }
}
