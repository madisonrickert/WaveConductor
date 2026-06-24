//! Cymatics ping-pong compute: the render-graph node that advances the wave
//! field N sub-steps per frame. The kernel is `assets/shaders/cymatics/
//! simulate.wgsl`.
//!
//! # Signal / data flow
//!
//! [`CymaticsComputePlugin::build`] wires three pieces into the render world:
//!
//! 1. [`ExtractResourcePlugin`] clones [`CymaticsSimParams`] (the per-frame
//!    uniform, the per-iteration phase times, the ping-pong + display texture
//!    handles, and the sub-step count) from the main world each frame.
//! 2. `init_cymatics_pipeline` ([`RenderStartup`]) builds the bind-group
//!    layout, queues the compute pipeline, and allocates the two persistent
//!    uniform buffers (the constant `SimParams` and the `MAX_ITERATIONS`-slot
//!    per-iteration array) **once** — never per frame.
//! 3. `prepare_cymatics_bind_groups` ([`RenderSystems::PrepareBindGroups`])
//!    uploads this frame's uniforms and builds the **two** bind groups — `ab`
//!    (reads A, writes B) and `ba` (reads B, writes A) — caching them across
//!    frames keyed on the ping-pong texture views.
//! 4. `cymatics_compute` runs in the root [`RenderGraph`] schedule before
//!    `camera_driver`, so the field is current before the 2D pass samples it.
//!    It dispatches the kernel `iterations` times, alternating `ab`/`ba` and
//!    binding each sub-step's 256-byte slot via a dynamic offset, then blits
//!    the final texture into the stable `display` texture.
//!
//! # Ping-pong contract
//!
//! `read_tex` is a sampled `texture_2d<f32>` (read via `textureLoad`); `write_tex`
//! is a `texture_storage_2d<rgba32float, write>`. Write-only storage (not
//! `read_write`) keeps us off a downlevel feature on the WebGPU-only target; the
//! A/B alternation is what supplies read-from-one / write-to-the-other. After
//! `iterations` sub-steps the freshest field is in B when the count is odd and
//! A when it is even; the final blit copies whichever into `display` so the
//! renderer always samples one fixed handle regardless of parity.

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
use bevy::render::{Render, RenderApp, RenderStartup, RenderSystems};

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

        render_app
            .add_systems(RenderStartup, init_cymatics_pipeline)
            .add_systems(
                Render,
                prepare_cymatics_bind_groups
                    .in_set(RenderSystems::PrepareBindGroups)
                    .run_if(resource_exists::<CymaticsSimParams>),
            );

        // Run the N-iteration dispatch in the root `RenderGraph` schedule, before
        // `camera_driver` runs the per-camera schedules — so `display` holds the
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
    /// Sub-steps to run this frame.
    iterations: u32,
}

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
fn prepare_cymatics_bind_groups(
    mut commands: Commands<'_, '_>,
    render_device: Res<'_, RenderDevice>,
    render_queue: Res<'_, RenderQueue>,
    pipeline_cache: Res<'_, PipelineCache>,
    sim: Res<'_, CymaticsSimParams>,
    images: Res<'_, RenderAssets<GpuImage>>,
    pipeline: Option<Res<'_, CymaticsPipeline>>,
    mut cached: Local<'_, Option<CachedBindGroups>>,
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

    // Each sub-step's phase time → the leading f32 of its 256-byte slot. The
    // shader reads only `time`, so the slot padding is left untouched; writing
    // the 4-byte field directly avoids materialising a 256-byte scratch.
    for (i, t) in sim.iter_times.iter().enumerate() {
        let offset = i as u64 * ITER_PARAMS_STRIDE;
        render_queue
            .0
            .write_buffer(&pipeline.iter_buffer, offset, bytemuck::bytes_of(t));
    }

    // Rebuild the bind groups only on a texture-view change; reuse otherwise.
    let key = (gpu_a.texture_view.id(), gpu_b.texture_view.id());
    let (ab, ba) = match &*cached {
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
            *cached = Some((key, ab.clone(), ba.clone()));
            (ab, ba)
        }
    };

    // Dispatch dims track the resolution setting; recomputed each frame (cheap,
    // no allocation). Bound-checked in the shader against `resolution`.
    let dispatch_x = sim.resolution.x.div_ceil(WORKGROUP_SIZE);
    let dispatch_y = sim.resolution.y.div_ceil(WORKGROUP_SIZE);
    commands.insert_resource(CymaticsComputeBindGroups {
        ab,
        ba,
        dispatch_x,
        dispatch_y,
        iterations: sim.iterations,
    });
}

/// Render-graph node: dispatches the kernel `iterations` times, alternating
/// bind groups, then blits the final texture into `display`.
///
/// Runs in the root [`RenderGraph`] schedule before `camera_driver`. A clean
/// no-op while the bind groups, pipeline, or sim params are absent (sketch
/// inactive) or the pipeline is still compiling.
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

        // N sub-steps. Even i reads A / writes B (`ab`), odd i reads B / writes A
        // (`ba`); each binds its own IterParams slot via dynamic offset i*256.
        for i in 0..bg.iterations {
            let group = if i % 2 == 0 { &bg.ab } else { &bg.ba };
            let dynamic_offset = [i * ITER_PARAMS_STRIDE_U32];
            pass.set_bind_group(0, group, &dynamic_offset);
            pass.dispatch_workgroups(bg.dispatch_x, bg.dispatch_y, 1);
        }
    }

    // The freshest field is in B after an odd sub-step count, else A (the loop
    // starts ab: i=0 writes B, i=1 writes A, …). Blit it into the stable
    // `display` texture so the renderer samples one fixed handle regardless of
    // parity. Requires COPY_SRC on A/B and COPY_DST on display (set in C5).
    let final_handle = if bg.iterations % 2 == 1 {
        &sim.tex_b
    } else {
        &sim.tex_a
    };
    if let (Some(src), Some(dst)) = (images.get(final_handle), images.get(&sim.display)) {
        render_context.command_encoder().copy_texture_to_texture(
            src.texture.as_image_copy(),
            dst.texture.as_image_copy(),
            Extent3d {
                width: sim.resolution.x,
                height: sim.resolution.y,
                depth_or_array_layers: 1,
            },
        );
    }
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
}
