//! Shared compute pipeline for particle simulation.
//!
//! Architecture mirrors Bevy 0.18's `compute_shader_game_of_life` example:
//!
//! - [`ParticleComputePlugin`] extracts sim params into the render world and
//!   registers a render system that dispatches the compute shader each frame.
//! - [`ParticleSimParams`] is extracted from the main world via
//!   [`ExtractResourcePlugin`] and carries the per-frame uniform + the
//!   `ShaderBuffer` handle for the particle array.
//! - [`ParticlePipeline`] is initialized in [`RenderStartup`] and caches the
//!   `BindGroupLayoutDescriptor` + `CachedComputePipelineId`.
//! - `prepare_bind_group` runs in [`RenderSystems::PrepareBindGroups`] and
//!   builds the per-frame [`ParticleComputeBindGroup`].
//! - `particle_compute` (private) dispatches the compute pass; it runs in the
//!   root `RenderGraph` schedule, ordered before `camera_driver`.
//!
//! # Bind group layout (matches `simulate.wgsl`)
//!
//! - `@binding(0)`: `SimParams` uniform.
//! - `@binding(1)`: Particle storage buffer, `read_write`.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "u32 ↔ usize casts for GPU buffer sizes are intentional and bounds-checked"
)]

use std::borrow::Cow;
use std::num::NonZeroU64;

use bevy::core_pipeline::schedule::camera_driver;
use bevy::prelude::*;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{
    BindGroupEntry, BindGroupLayout, BindGroupLayoutDescriptor, BindGroupLayoutEntry, BindingType,
    Buffer, BufferBindingType, BufferDescriptor, BufferUsages, CachedComputePipelineId,
    ComputePassDescriptor, ComputePipelineDescriptor, PipelineCache, ShaderStages,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderGraph, RenderQueue};
use bevy::render::storage::{GpuShaderBuffer, ShaderBuffer};
use bevy::render::{Render, RenderApp, RenderStartup, RenderSystems};

use super::particle::SimParams;

/// Workgroup size must match `@workgroup_size(64)` in `simulate.wgsl`.
const WORKGROUP_SIZE: u32 = 64;

/// Compile-time validated `SimParams` size for the uniform bind-group entry.
///
/// `SimParams` is non-zero-sized by definition (it has fields). The `panic!`
/// branch is inside a `const` expression, so any future change that made it
/// zero-sized would fail at compile time rather than at runtime.
#[allow(
    clippy::cast_possible_truncation,
    reason = "size_of::<SimParams>() fits in u64 on all supported targets; \
              u64::try_from(usize) isn't const-stable in 1.89"
)]
const SIM_PARAMS_SIZE: NonZeroU64 = match NonZeroU64::new(std::mem::size_of::<SimParams>() as u64) {
    Some(n) => n,
    None => panic!("SimParams must be non-zero-sized"),
};

/// Plugin that wires the shared particle compute pipeline into the render world.
pub struct ParticleComputePlugin;

impl Plugin for ParticleComputePlugin {
    fn build(&self, app: &mut App) {
        // Extract ParticleSimParams from the main world into the render world each frame.
        app.add_plugins(ExtractResourcePlugin::<ParticleSimParams>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app
            .add_systems(RenderStartup, init_particle_pipeline)
            .add_systems(
                Render,
                prepare_bind_group
                    .in_set(RenderSystems::PrepareBindGroups)
                    .run_if(resource_exists::<ParticleSimParams>),
            );

        // Run the compute dispatch in the root `RenderGraph` schedule, before
        // `camera_driver` runs the per-camera schedules — so the particle buffer
        // is updated before any 2D pass reads it. (Bevy 0.19 replaced the
        // trait-based render graph with systems; see the migration guide's
        // "Render Graph as Systems".)
        render_app.add_systems(RenderGraph, particle_compute.before(camera_driver));
    }
}

/// Extracted each frame from the main world into the render world.
///
/// Carries the per-frame simulation parameters and the GPU buffer handle.
#[derive(Resource, Clone, ExtractResource)]
pub struct ParticleSimParams {
    /// Per-frame uniforms (dt, drag, attractor position, etc.).
    pub params: SimParams,
    /// Handle to the particle storage buffer (shared with `ParticleMaterial`).
    pub particles_handle: Handle<ShaderBuffer>,
    /// Number of particles — determines dispatch size.
    pub particle_count: u32,
}

/// Cached compute pipeline state. Initialized once in [`RenderStartup`].
#[derive(Resource)]
pub struct ParticlePipeline {
    /// Descriptor retained so `prepare_bind_group` can retrieve the
    /// `BindGroupLayout` from the `PipelineCache` without storing the layout
    /// object separately.
    pub bind_group_layout_descriptor: BindGroupLayoutDescriptor,
    /// Handle into Bevy's `PipelineCache`.
    pub pipeline_id: CachedComputePipelineId,
    /// Persistent uniform buffer for `SimParams`.
    ///
    /// Allocated once at pipeline init with `UNIFORM | COPY_DST` and updated
    /// each frame via `queue.write_buffer` — avoids a GPU buffer allocation
    /// every frame that `create_buffer_with_data` would incur.
    pub sim_params_buffer: Buffer,
}

/// Per-frame bind group built by the `prepare_bind_group` system (private to
/// this module) and consumed by `particle_compute` during the render schedule.
#[derive(Resource)]
pub struct ParticleComputeBindGroup {
    /// Bind group with `SimParams` uniform (binding 0) and particle buffer (binding 1).
    pub bind_group: bevy::render::render_resource::BindGroup,
    /// Workgroup count: `ceil(particle_count / WORKGROUP_SIZE)`.
    pub dispatch_size: u32,
}

/// Initializes [`ParticlePipeline`] in the render world startup schedule.
///
/// This runs once when the render world is first set up. Runs in
/// [`RenderStartup`] rather than via `FromWorld` because it needs
/// `AssetServer`, `PipelineCache`, and `RenderDevice` as system params.
fn init_particle_pipeline(
    mut commands: Commands<'_, '_>,
    asset_server: Res<'_, AssetServer>,
    pipeline_cache: Res<'_, PipelineCache>,
    render_device: Res<'_, RenderDevice>,
) {
    // Build the bind group layout descriptor manually with raw entries so we
    // don't depend on encase::ShaderType for SimParams (we use bytemuck instead).
    // SIM_PARAMS_SIZE is validated at compile time, so no runtime branch.
    let bind_group_layout_descriptor = BindGroupLayoutDescriptor::new(
        "particle_compute_bgl",
        &[
            // binding 0 — SimParams uniform buffer
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
            // binding 1 — Particle storage buffer, read_write
            BindGroupLayoutEntry {
                binding: 1,
                visibility: ShaderStages::COMPUTE,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    );

    let shader = asset_server.load::<bevy::shader::Shader>("shaders/particles/simulate.wgsl");

    let pipeline_id = pipeline_cache.queue_compute_pipeline(ComputePipelineDescriptor {
        label: Some(Cow::from("particle_compute_pipeline")),
        layout: vec![bind_group_layout_descriptor.clone()],
        shader,
        entry_point: Some(Cow::from("main")),
        ..default()
    });

    // Allocate the SimParams uniform buffer once. Each frame `prepare_bind_group`
    // uploads new data via `queue.write_buffer` — no per-frame allocation.
    let sim_params_buffer = render_device.create_buffer(&BufferDescriptor {
        label: Some("particle_sim_params_uniform"),
        size: std::mem::size_of::<super::particle::SimParams>() as u64,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    commands.insert_resource(ParticlePipeline {
        bind_group_layout_descriptor,
        pipeline_id,
        sim_params_buffer,
    });
}

/// Builds the per-frame bind group for the compute dispatch.
///
/// Uploads [`SimParams`] into the persistent uniform buffer on
/// [`ParticlePipeline`] via `queue.write_buffer` — no per-frame GPU allocation.
/// Retrieves the GPU particle buffer via `RenderAssets<GpuShaderBuffer>`.
fn prepare_bind_group(
    mut commands: Commands<'_, '_>,
    render_device: Res<'_, RenderDevice>,
    render_queue: Res<'_, RenderQueue>,
    pipeline_cache: Res<'_, PipelineCache>,
    sim: Res<'_, ParticleSimParams>,
    buffers: Res<'_, RenderAssets<GpuShaderBuffer>>,
    pipeline: Option<Res<'_, ParticlePipeline>>,
) {
    let Some(pipeline) = pipeline else {
        return;
    };
    let Some(particle_buffer) = buffers.get(&sim.particles_handle) else {
        return;
    };

    // Upload current SimParams into the persistent uniform buffer.
    // `write_buffer` is a staged copy — no allocation after init.
    render_queue.0.write_buffer(
        &pipeline.sim_params_buffer,
        0,
        bytemuck::bytes_of(&sim.params),
    );

    // Retrieve the cached BindGroupLayout from the PipelineCache.
    let layout: BindGroupLayout =
        pipeline_cache.get_bind_group_layout(&pipeline.bind_group_layout_descriptor);

    let bind_group = render_device.create_bind_group(
        "particle_compute_bind_group",
        &layout,
        &[
            BindGroupEntry {
                binding: 0,
                resource: pipeline.sim_params_buffer.as_entire_binding(),
            },
            BindGroupEntry {
                binding: 1,
                resource: particle_buffer.buffer.as_entire_binding(),
            },
        ],
    );

    let dispatch_size = sim.particle_count.div_ceil(WORKGROUP_SIZE);
    commands.insert_resource(ParticleComputeBindGroup {
        bind_group,
        dispatch_size,
    });
}

/// Render system that dispatches the particle compute shader each frame.
///
/// Runs in the root [`RenderGraph`] schedule before `camera_driver`, so the
/// particle storage buffer is updated before any 2D pass reads it. A no-op when
/// the bind group or pipeline isn't ready (sketch inactive / still compiling).
fn particle_compute(
    bind_group: Option<Res<'_, ParticleComputeBindGroup>>,
    pipeline_res: Option<Res<'_, ParticlePipeline>>,
    pipeline_cache: Res<'_, PipelineCache>,
    mut render_context: RenderContext<'_, '_>,
) {
    let Some(bg) = bind_group else {
        return;
    };
    let Some(pipeline_res) = pipeline_res else {
        return;
    };
    let Some(compute_pipeline) = pipeline_cache.get_compute_pipeline(pipeline_res.pipeline_id)
    else {
        return;
    };

    let mut pass = render_context
        .command_encoder()
        .begin_compute_pass(&ComputePassDescriptor {
            label: Some("particle_compute_pass"),
            timestamp_writes: None,
        });
    pass.set_pipeline(compute_pipeline);
    pass.set_bind_group(0, &bg.bind_group, &[]);
    pass.dispatch_workgroups(bg.dispatch_size, 1, 1);
}
