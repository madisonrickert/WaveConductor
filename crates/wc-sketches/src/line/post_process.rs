//! Gravity-smear post-process pipeline for the Line sketch.
//!
//! ## Render graph
//!
//! [`LinePostProcessNode`] is inserted into the Core2d sub-graph immediately
//! after [`Node2d::MainTransparentPass`] and before [`Node2d::EndMainPass`].
//! It reads the camera's view target as input and writes back to the same
//! target's swap texture (Bevy's [`ViewTarget::post_process_write`] rotates
//! between two textures so a node can sample its own input).
//!
//! ## Uniforms
//!
//! [`LinePostParams`] is populated each frame on the main thread by
//! [`crate::line::systems::update_sim_params`] (which also writes the compute
//! sim params). [`ExtractResourcePlugin`] mirrors it into the render world.
//! A persistent uniform buffer is allocated once in
//! [`PostProcessPipeline::from_world`]; each frame the node uploads the
//! latest snapshot via `queue.write_buffer` — no per-frame GPU allocation.
//! Mirrors the [`crate::line::compute`] sim-params-buffer pattern.
//!
//! ## Shader
//!
//! `assets/shaders/line/gravity.wgsl` is the WGSL port of v4's
//! `src/sketches/line/shaders/gravity/fragment.glsl`. A separate
//! fullscreen-triangle vertex stage lives in the same file.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "u32 ↔ usize casts for GPU buffer sizes are intentional"
)]

use std::num::NonZeroU64;

use bevy::core_pipeline::core_2d::graph::{Core2d, Node2d};
use bevy::ecs::query::QueryItem;
use bevy::image::BevyDefault as _;
use bevy::prelude::*;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_graph::{
    NodeRunError, RenderGraphContext, RenderGraphExt, RenderLabel, ViewNode, ViewNodeRunner,
};
use bevy::render::render_resource::{
    binding_types::{sampler, texture_2d, uniform_buffer_sized},
    BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, Buffer, BufferDescriptor,
    BufferUsages, CachedRenderPipelineId, ColorTargetState, ColorWrites, FragmentState, LoadOp,
    MultisampleState, Operations, PipelineCache, PrimitiveState, RenderPassColorAttachment,
    RenderPassDescriptor, RenderPipelineDescriptor, Sampler, SamplerBindingType, SamplerDescriptor,
    ShaderStages, StoreOp, TextureFormat, TextureSampleType, VertexState,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue};
use bevy::render::view::ViewTarget;
use bevy::render::RenderApp;
use bevy::shader::Shader;
use bytemuck::{Pod, Zeroable};

/// Uniform layout for the post-process shader. Mirrors `struct PostParams`
/// in `assets/shaders/line/gravity.wgsl`.
///
/// All fields are tightly packed `f32`s in `#[repr(C)]` order. The WGSL
/// declares the matching fields in the same order so wgpu's std140-equivalent
/// layout aligns without padding (`vec2<f32>` pairs sit on 8-byte boundaries,
/// scalars pack after).
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable, Resource, ExtractResource)]
pub struct LinePostParams {
    /// Render-target resolution in pixels (width, height).
    pub i_resolution: [f32; 2],
    /// Cursor position in window pixel coordinates (top-left origin, +y down).
    pub i_mouse: [f32; 2],
    /// Scale factor for the per-step mouse-pull contribution. v4 default 1/15.
    pub i_mouse_factor: f32,
    /// Elapsed wall-clock seconds since app start.
    pub i_global_time: f32,
    /// Gravity constant `G` used by the smear ray-march. Plan 9 will modulate
    /// this with audio + a triangle wave; Phase C ships a constant placeholder.
    ///
    /// Default is `0.0` so the post-process is visually no-op outside
    /// `AppState::Line`. `update_sim_params` (gated by `sketch_active(Line)`)
    /// writes a non-zero value each frame in Line; `remove_sim_params` resets
    /// to default on `OnExit(Line)` so the next state doesn't inherit the
    /// last in-Line value.
    pub g_constant: f32,
    /// Per-channel gamma curve applied as the final step of the post-process.
    pub gamma: f32,
}

/// Compile-time validated `LinePostParams` size for the uniform bind-group
/// entry — mirrors the `SIM_PARAMS_SIZE` pattern in
/// [`crate::line::compute`]. Since `LinePostParams` has fields it is
/// non-zero-sized; the `panic!` branch is unreachable at runtime because it
/// lives inside a `const` block.
const POST_PARAMS_SIZE: NonZeroU64 =
    match NonZeroU64::new(std::mem::size_of::<LinePostParams>() as u64) {
        Some(n) => n,
        None => panic!("LinePostParams must be non-zero-sized"),
    };

/// Plugin that registers the gravity-smear post-process node and its uniform
/// resource. Adds [`ExtractResourcePlugin<LinePostParams>`] so the render
/// world sees the latest uniforms each frame.
pub struct LinePostProcessPlugin;

impl Plugin for LinePostProcessPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LinePostParams>();
        app.add_plugins(ExtractResourcePlugin::<LinePostParams>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app
            .add_render_graph_node::<ViewNodeRunner<LinePostProcessNode>>(
                Core2d,
                LinePostProcessLabel,
            )
            .add_render_graph_edges(
                Core2d,
                (
                    Node2d::MainTransparentPass,
                    LinePostProcessLabel,
                    Node2d::EndMainPass,
                ),
            );
    }

    fn finish(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.init_resource::<PostProcessPipeline>();
    }
}

/// Render-graph label for the gravity-smear post-process node.
#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
pub struct LinePostProcessLabel;

/// Cached render-pipeline state for the post-process. Initialised once in
/// [`LinePostProcessPlugin::finish`] (which runs after the render sub-app is
/// fully set up so `AssetServer`, `PipelineCache`, and `RenderDevice` are all
/// available).
#[derive(Resource)]
pub struct PostProcessPipeline {
    /// Bind-group layout descriptor retained so [`LinePostProcessNode::run`]
    /// can fetch the cached `BindGroupLayout` from the
    /// [`PipelineCache`] without storing the layout object separately.
    pub bind_group_layout_descriptor: BindGroupLayoutDescriptor,
    /// Filtering sampler used to read the scene texture.
    pub sampler: Sampler,
    /// Handle into Bevy's `PipelineCache` for the gravity-smear render pipeline.
    pub pipeline_id: CachedRenderPipelineId,
    /// Persistent uniform buffer for [`LinePostParams`].
    ///
    /// Allocated once with `UNIFORM | COPY_DST` and updated each frame via
    /// `queue.write_buffer` in [`LinePostProcessNode::run`] — avoids a GPU
    /// buffer allocation every frame that `create_buffer_with_data` would
    /// incur. Mirrors `LinePipeline::sim_params_buffer` in
    /// [`crate::line::compute`].
    pub post_params_buffer: Buffer,
}

impl FromWorld for PostProcessPipeline {
    fn from_world(world: &mut World) -> Self {
        let render_device = world.resource::<RenderDevice>();

        // Build the bind-group layout descriptor. `BindGroupLayoutEntries`
        // derefs to `[BindGroupLayoutEntry]`, so we copy out an owned vec
        // for the descriptor (the descriptor itself is `Clone` and gets
        // passed to `RenderPipelineDescriptor::layout`).
        let entries = BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                uniform_buffer_sized(false, Some(POST_PARAMS_SIZE)),
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
            ),
        );
        let bind_group_layout_descriptor =
            BindGroupLayoutDescriptor::new("line_post_layout", &entries);

        let sampler = render_device.create_sampler(&SamplerDescriptor::default());

        // Allocate the post-params uniform buffer once. Each frame the node
        // uploads new data via `queue.write_buffer` — no per-frame allocation.
        let post_params_buffer = render_device.create_buffer(&BufferDescriptor {
            label: Some("line_post_params"),
            size: std::mem::size_of::<LinePostParams>() as u64,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let shader: Handle<Shader> = world
            .resource::<AssetServer>()
            .load("shaders/line/gravity.wgsl");

        let pipeline_id =
            world
                .resource_mut::<PipelineCache>()
                .queue_render_pipeline(RenderPipelineDescriptor {
                    label: Some("line_post_process_pipeline".into()),
                    layout: vec![bind_group_layout_descriptor.clone()],
                    push_constant_ranges: vec![],
                    vertex: VertexState {
                        shader: shader.clone(),
                        shader_defs: vec![],
                        entry_point: Some("vertex".into()),
                        buffers: vec![],
                    },
                    fragment: Some(FragmentState {
                        shader,
                        shader_defs: vec![],
                        entry_point: Some("fragment".into()),
                        targets: vec![Some(ColorTargetState {
                            format: TextureFormat::bevy_default(),
                            blend: None,
                            write_mask: ColorWrites::ALL,
                        })],
                    }),
                    primitive: PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: MultisampleState::default(),
                    zero_initialize_workgroup_memory: false,
                });

        Self {
            bind_group_layout_descriptor,
            sampler,
            pipeline_id,
            post_params_buffer,
        }
    }
}

/// Render-graph node that draws the gravity-smear post-process pass.
///
/// Runs after [`Node2d::MainTransparentPass`] (so the scene texture contains
/// the rendered particles + attractor rings) and before [`Node2d::EndMainPass`].
/// Uploads [`LinePostParams`] into the persistent uniform buffer on
/// [`PostProcessPipeline`] via `queue.write_buffer`, builds a fresh bind
/// group, then issues a 3-vertex fullscreen-triangle draw.
#[derive(Default)]
pub struct LinePostProcessNode;

impl ViewNode for LinePostProcessNode {
    type ViewQuery = &'static ViewTarget;

    fn run<'w>(
        &self,
        _graph: &mut RenderGraphContext<'_>,
        render_context: &mut RenderContext<'w>,
        view_target: QueryItem<'w, '_, Self::ViewQuery>,
        world: &'w World,
    ) -> Result<(), NodeRunError> {
        let pipeline_cache = world.resource::<PipelineCache>();
        let Some(pipeline_res) = world.get_resource::<PostProcessPipeline>() else {
            tracing::trace!(
                node = "LinePostProcessNode",
                "no PostProcessPipeline resource"
            );
            return Ok(());
        };
        let Some(pipeline) = pipeline_cache.get_render_pipeline(pipeline_res.pipeline_id) else {
            tracing::trace!(node = "LinePostProcessNode", "pipeline still compiling");
            return Ok(());
        };
        let Some(post_params) = world.get_resource::<LinePostParams>() else {
            tracing::trace!(node = "LinePostProcessNode", "no LinePostParams resource");
            return Ok(());
        };

        // Upload current LinePostParams into the persistent uniform buffer.
        // `write_buffer` is a staged copy — no allocation after init.
        // `RenderQueue` is fetched from the render world (Bevy 0.18 does not
        // expose a `render_queue()` accessor on `RenderContext`).
        let render_queue = world.resource::<RenderQueue>();
        render_queue.0.write_buffer(
            &pipeline_res.post_params_buffer,
            0,
            bytemuck::bytes_of(post_params),
        );

        let post_process_write = view_target.post_process_write();
        let source = post_process_write.source;
        let destination = post_process_write.destination;

        let layout =
            pipeline_cache.get_bind_group_layout(&pipeline_res.bind_group_layout_descriptor);

        let bind_group = render_context.render_device().create_bind_group(
            "line_post_bind_group",
            &layout,
            &BindGroupEntries::sequential((
                pipeline_res.post_params_buffer.as_entire_binding(),
                source,
                &pipeline_res.sampler,
            )),
        );

        let mut pass = render_context
            .command_encoder()
            .begin_render_pass(&RenderPassDescriptor {
                label: Some("line_post_pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: destination,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Load,
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}
