//! Compute pipeline for Line particle simulation.
//!
//! Architecture mirrors Bevy 0.18's `compute_shader_game_of_life` example:
//!
//! - [`LineComputePlugin`] extracts sim params into the render world and
//!   inserts a render-graph node that dispatches the compute shader each frame.
//! - [`LineSimParams`] is extracted from the main world via
//!   [`ExtractResourcePlugin`] and carries the per-frame uniform + the
//!   `ShaderStorageBuffer` handle for the particle array.
//! - [`LinePipeline`] is initialized in [`RenderStartup`] and caches the
//!   `BindGroupLayoutDescriptor` + `CachedComputePipelineId`.
//! - `prepare_bind_group` runs in [`RenderSystems::PrepareBindGroups`] and
//!   builds the per-frame [`LineComputeBindGroup`].
//! - `LineComputeNode` (private) dispatches the compute pass; it lives in the
//!   main render graph and edges before [`bevy::render::graph::CameraDriverLabel`].
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

use bevy::prelude::*;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_graph::{self, RenderGraph, RenderLabel};
use bevy::render::render_resource::{
    BindGroupEntry, BindGroupLayout, BindGroupLayoutDescriptor, BindGroupLayoutEntry, BindingType,
    Buffer, BufferBindingType, BufferDescriptor, BufferUsages, CachedComputePipelineId,
    ComputePassDescriptor, ComputePipelineDescriptor, PipelineCache, ShaderStages,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue};
use bevy::render::storage::{GpuShaderStorageBuffer, ShaderStorageBuffer};
use bevy::render::{Render, RenderApp, RenderStartup, RenderSystems};

use super::particle::SimParams;

/// Workgroup size must match `@workgroup_size(64)` in `simulate.wgsl`.
const WORKGROUP_SIZE: u32 = 64;

/// Compile-time validated `SimParams` size for the uniform bind-group entry.
///
/// `SimParams` is non-zero-sized by definition (it has fields). The `panic!`
/// branch is inside a `const` expression, so any future change that made it
/// zero-sized would fail at compile time rather than at runtime.
const SIM_PARAMS_SIZE: NonZeroU64 = match NonZeroU64::new(
    // const usize→u64 cast — u64::try_from(usize) isn't const-stable.
    // size_of fits in u64 on all supported targets.
    std::mem::size_of::<SimParams>() as u64,
) {
    Some(n) => n,
    None => panic!("SimParams must be non-zero-sized"),
};

/// Render-graph label for the Line compute node.
///
/// The node lives in the main (non-sub) render graph and runs before the
/// camera driver so the buffer is updated before any 2D pass reads it.
#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
pub struct LineComputeLabel;

/// Plugin that wires the compute pipeline into the render world.
pub struct LineComputePlugin;

impl Plugin for LineComputePlugin {
    fn build(&self, app: &mut App) {
        // Extract LineSimParams from the main world into the render world each frame.
        app.add_plugins(ExtractResourcePlugin::<LineSimParams>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app
            .add_systems(RenderStartup, init_line_pipeline)
            .add_systems(
                Render,
                prepare_bind_group
                    .in_set(RenderSystems::PrepareBindGroups)
                    .run_if(resource_exists::<LineSimParams>),
            );

        // Add the compute node to the main render graph (not a sub-graph).
        // Edge to CameraDriverLabel ensures the compute pass completes before
        // the camera driver begins issuing 2D draw calls.
        let mut render_graph = render_app.world_mut().resource_mut::<RenderGraph>();
        render_graph.add_node(LineComputeLabel, LineComputeNode);
        render_graph.add_node_edge(LineComputeLabel, bevy::render::graph::CameraDriverLabel);
    }
}

/// Extracted each frame from the main world into the render world.
///
/// Carries the per-frame simulation parameters and the GPU buffer handle.
#[derive(Resource, Clone, ExtractResource)]
pub struct LineSimParams {
    /// Per-frame uniforms (dt, drag, attractor position, etc.).
    pub params: SimParams,
    /// Handle to the particle storage buffer (shared with `LineMaterial`).
    pub particles_handle: Handle<ShaderStorageBuffer>,
    /// Number of particles — determines dispatch size.
    pub particle_count: u32,
}

/// Cached compute pipeline state. Initialized once in [`RenderStartup`].
#[derive(Resource)]
pub struct LinePipeline {
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
/// this module) and consumed by `LineComputeNode` during graph execution.
#[derive(Resource)]
pub struct LineComputeBindGroup {
    /// Bind group with `SimParams` uniform (binding 0) and particle buffer (binding 1).
    pub bind_group: bevy::render::render_resource::BindGroup,
    /// Workgroup count: `ceil(particle_count / WORKGROUP_SIZE)`.
    pub dispatch_size: u32,
}

/// Initializes [`LinePipeline`] in the render world startup schedule.
///
/// This runs once when the render world is first set up. Runs in
/// [`RenderStartup`] rather than via `FromWorld` because it needs
/// `AssetServer`, `PipelineCache`, and `RenderDevice` as system params.
fn init_line_pipeline(
    mut commands: Commands<'_, '_>,
    asset_server: Res<'_, AssetServer>,
    pipeline_cache: Res<'_, PipelineCache>,
    render_device: Res<'_, RenderDevice>,
) {
    // Build the bind group layout descriptor manually with raw entries so we
    // don't depend on encase::ShaderType for SimParams (we use bytemuck instead).
    // SIM_PARAMS_SIZE is validated at compile time, so no runtime branch.
    let bind_group_layout_descriptor = BindGroupLayoutDescriptor::new(
        "line_compute_bgl",
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

    let shader = asset_server.load::<bevy::shader::Shader>("shaders/line/simulate.wgsl");

    let pipeline_id = pipeline_cache.queue_compute_pipeline(ComputePipelineDescriptor {
        label: Some(Cow::from("line_compute_pipeline")),
        layout: vec![bind_group_layout_descriptor.clone()],
        shader,
        entry_point: Some(Cow::from("main")),
        ..default()
    });

    // Allocate the SimParams uniform buffer once. Each frame `prepare_bind_group`
    // uploads new data via `queue.write_buffer` — no per-frame allocation.
    let sim_params_buffer = render_device.create_buffer(&BufferDescriptor {
        label: Some("line_sim_params_uniform"),
        size: std::mem::size_of::<super::particle::SimParams>() as u64,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    commands.insert_resource(LinePipeline {
        bind_group_layout_descriptor,
        pipeline_id,
        sim_params_buffer,
    });
}

/// Builds the per-frame bind group for the compute dispatch.
///
/// Uploads [`SimParams`] into the persistent uniform buffer on
/// [`LinePipeline`] via `queue.write_buffer` — no per-frame GPU allocation.
/// Retrieves the GPU particle buffer via `RenderAssets<GpuShaderStorageBuffer>`.
fn prepare_bind_group(
    mut commands: Commands<'_, '_>,
    render_device: Res<'_, RenderDevice>,
    render_queue: Res<'_, RenderQueue>,
    pipeline_cache: Res<'_, PipelineCache>,
    sim: Res<'_, LineSimParams>,
    buffers: Res<'_, RenderAssets<GpuShaderStorageBuffer>>,
    pipeline: Option<Res<'_, LinePipeline>>,
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
        "line_compute_bind_group",
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
    commands.insert_resource(LineComputeBindGroup {
        bind_group,
        dispatch_size,
    });
}

/// Render-graph node that dispatches the Line compute shader each frame.
#[derive(Default)]
struct LineComputeNode;

impl render_graph::Node for LineComputeNode {
    fn run(
        &self,
        _graph: &mut render_graph::RenderGraphContext<'_>,
        render_context: &mut RenderContext<'_>,
        world: &World,
    ) -> Result<(), render_graph::NodeRunError> {
        let Some(bg) = world.get_resource::<LineComputeBindGroup>() else {
            tracing::trace!(
                node = "LineComputeNode",
                "no bind group — sketch inactive or buffer not ready"
            );
            return Ok(());
        };
        let Some(pipeline_res) = world.get_resource::<LinePipeline>() else {
            tracing::trace!(node = "LineComputeNode", "no LinePipeline resource");
            return Ok(());
        };
        let pipeline_cache = world.resource::<PipelineCache>();
        let Some(compute_pipeline) = pipeline_cache.get_compute_pipeline(pipeline_res.pipeline_id)
        else {
            tracing::trace!(node = "LineComputeNode", "pipeline still compiling");
            return Ok(());
        };

        let mut pass =
            render_context
                .command_encoder()
                .begin_compute_pass(&ComputePassDescriptor {
                    label: Some("line_compute_pass"),
                    timestamp_writes: None,
                });
        pass.set_pipeline(compute_pipeline);
        pass.set_bind_group(0, &bg.bind_group, &[]);
        pass.dispatch_workgroups(bg.dispatch_size, 1, 1);
        Ok(())
    }
}
