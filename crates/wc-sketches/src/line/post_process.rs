//! Gravity-smear post-process pipeline for the Line sketch.
//!
//! ## Render schedule
//!
//! [`line_post_process`] is a render system in the Core2d schedule's
//! [`Core2dSystems::EarlyPostProcess`] set: after the main pass (so the scene
//! texture is ready) and before bloom/tonemapping run in `PostProcess`. It
//! reads the camera's view target as input and writes back to the same target's
//! swap texture (Bevy's [`ViewTarget::post_process_write`] rotates between two
//! textures so a system can sample its own input).
//!
//! [`Core2dSystems::EarlyPostProcess`]: bevy::core_pipeline::Core2dSystems::EarlyPostProcess
//!
//! ## Uniforms
//!
//! [`LinePostParams`] is inserted `OnEnter(AppState::Line)` by
//! [`crate::line`] and removed `OnExit`. [`ExtractResourcePlugin`] mirrors it
//! into the render world each frame. [`line_post_process`] takes
//! `Option<Res<LinePostParams>>` and early-returns when the resource is absent,
//! so the pass is a true no-op outside `AppState::Line`. A persistent uniform
//! buffer is allocated once in [`PostProcessPipeline::from_world`]; each frame
//! the node uploads the latest snapshot via `queue.write_buffer` -- no
//! per-frame GPU allocation. Mirrors the [`crate::particles::compute`]
//! sim-params-buffer pattern.
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

use bevy::core_pipeline::{Core2d, Core2dSystems};
use bevy::prelude::*;
use bevy::render::camera::ExtractedCamera;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_resource::{
    binding_types::{sampler, texture_2d, uniform_buffer_sized},
    BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, Buffer, BufferDescriptor,
    BufferUsages, CachedRenderPipelineId, ColorTargetState, ColorWrites, FragmentState, LoadOp,
    MultisampleState, Operations, PipelineCache, PrimitiveState, RenderPassColorAttachment,
    RenderPassDescriptor, RenderPipelineDescriptor, Sampler, SamplerBindingType, SamplerDescriptor,
    ShaderStages, StoreOp, TextureFormat, TextureSampleType, VertexState,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::view::ViewTarget;
use bevy::render::{Extract, ExtractSchedule, RenderApp};
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
    /// Gravity constant `G` used by the smear ray-march. Modulated each
    /// frame in Line by [`crate::line::audio_coupling::drive_audio_and_shader`]
    /// with a triangle-wave envelope x `(groupedUpness + 0.5) x 15000`.
    ///
    /// `update_sim_params` (gated by `sketch_active(Line)`) writes a
    /// placeholder value each frame; `drive_audio_and_shader` overrides it
    /// with the ParticleStats-driven envelope. The entire resource is absent
    /// outside `AppState::Line` (removed by `remove_sim_params` on
    /// `OnExit(Line)` and re-inserted by `insert_line_post_params` on
    /// `OnEnter(Line)`), so the render system no-ops when not in Line.
    pub g_constant: f32,
    /// Per-channel gamma curve applied as the final step of the post-process.
    pub gamma: f32,
    /// Outgoing-trail smear HDR end-tint (`xyz`; `w` pad). The gravity smear
    /// derives its per-step chromatic factor as `pow(this, 1/NUM_STEPS)`, so the
    /// trail compounds toward this tint. Written each in-Line frame by
    /// [`crate::line::systems::sim_params::bake_smear_tints`] from `LineSettings`
    /// (`color × gain`). Default zero is inert: the smear is gated by
    /// `g_constant` (also 0 by default), so an unwritten tint never renders.
    pub smear_outgoing_tint: [f32; 4],
    /// Incoming-trail smear HDR end-tint (`xyz`; `w` pad). See
    /// [`Self::smear_outgoing_tint`].
    pub smear_incoming_tint: [f32; 4],
}

/// Compile-time validated `LinePostParams` size for the uniform bind-group
/// entry — mirrors the `SIM_PARAMS_SIZE` pattern in
/// [`crate::particles::compute`]. Since `LinePostParams` has fields it is
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
///
/// [`LinePostParams`] itself is **not** initialised globally by this plugin --
/// it is inserted `OnEnter(AppState::Line)` and removed `OnExit` in
/// [`crate::line`], so the render system no-ops outside Line (the
/// `Option<Res<LinePostParams>>` gate returns `None`).
pub struct LinePostProcessPlugin;

impl Plugin for LinePostProcessPlugin {
    fn build(&self, app: &mut App) {
        // The resource is NOT init'd here -- it is inserted per-sketch in
        // line/mod.rs so the render system no-ops outside AppState::Line.
        app.add_plugins(ExtractResourcePlugin::<LinePostParams>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        // Run in EarlyPostProcess: after the main pass (scene texture holds the
        // rendered particles + rings) and before `PostProcess`, where bloom and
        // tonemapping read the smeared HDR result.
        render_app.add_systems(
            Core2d,
            line_post_process.in_set(Core2dSystems::EarlyPostProcess),
        );

        // Explicitly remove the render-world copy when the main-world resource
        // is gone. `ExtractResourcePlugin` propagates inserts and updates but
        // NOT removals (verified against bevy_render 0.19 extract_resource.rs —
        // the None branch is a complete no-op). Without this, `LinePostParams`
        // lingers in the render world after `OnExit(AppState::Line)`, causing
        // `line_post_process` to keep running Line's gravity smear on Dots and
        // Home frames with stale uniform values.
        render_app.add_systems(ExtractSchedule, remove_line_post_params_if_absent);
    }

    fn finish(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.init_resource::<PostProcessPipeline>();
    }
}

/// Cached render-pipeline state for the post-process. Initialised once in
/// [`LinePostProcessPlugin::finish`] (which runs after the render sub-app is
/// fully set up so `AssetServer`, `PipelineCache`, and `RenderDevice` are all
/// available).
#[derive(Resource)]
pub struct PostProcessPipeline {
    /// Bind-group layout descriptor retained so [`line_post_process`]
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
    /// `queue.write_buffer` in [`line_post_process`] — avoids a GPU
    /// buffer allocation every frame that `create_buffer_with_data` would
    /// incur. Mirrors `ParticlePipeline::sim_params_buffer` in
    /// [`crate::particles::compute`].
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
                    immediate_size: 0,
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
                        // The pipeline writes into the camera's view target,
                        // which is `Rgba16Float` while internal-HDR rendering
                        // is on (see `spawn_camera` in the binary crate).
                        // wgpu validates pipeline target formats against the
                        // bound attachment at draw time, so this MUST match
                        // the view target's HDR format.
                        //
                        // We previously used `TextureFormat::bevy_default()`
                        // here, which returns `Rgba8UnormSrgb` and clipped
                        // the gravity ray-march accumulator at 1.0. The
                        // float target now preserves the over-bright values
                        // for bloom + AgX tonemap to handle downstream.
                        targets: vec![Some(ColorTargetState {
                            format: TextureFormat::Rgba16Float,
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

/// Removes the render-world [`LinePostParams`] when the main world no longer
/// has it — i.e. after `OnExit(AppState::Line)` fires.
///
/// [`ExtractResourcePlugin`] propagates inserts and updates each
/// [`ExtractSchedule`] tick but does **not** issue `remove_resource` when the
/// main-world source is absent (verified against `bevy_render` 0.19
/// `extract_resource.rs`: the `None` arm is a no-op). Without this explicit
/// removal the stale render-world copy keeps [`line_post_process`] running
/// Line's gravity smear on Dots/Home frames with stale `g_constant`/`gamma`
/// values.
///
/// The two [`ExtractSchedule`] systems — this one and the
/// [`ExtractResourcePlugin`]'s own insert/update system — guard on mutually
/// exclusive conditions (`main_resource.is_none()` vs `is_some()`), so there
/// is no ordering conflict between them.
fn remove_line_post_params_if_absent(
    mut commands: Commands<'_, '_>,
    main_resource: Extract<'_, '_, Option<Res<'_, LinePostParams>>>,
    render_resource: Option<Res<'_, LinePostParams>>,
) {
    if main_resource.is_none() && render_resource.is_some() {
        commands.remove_resource::<LinePostParams>();
    }
}

/// Render system that draws the gravity-smear post-process pass.
///
/// Added to the [`Core2d`] schedule in [`Core2dSystems::EarlyPostProcess`] (so
/// the scene texture holds the rendered particles + rings, and the smear runs in
/// HDR before bloom/tonemapping). Uploads [`LinePostParams`] into the persistent
/// uniform buffer via `queue.write_buffer`, builds a fresh bind group, then
/// issues a 3-vertex fullscreen-triangle draw.
///
/// We read HDR-ness from [`ExtractedCamera::hdr`] rather than querying the `Hdr`
/// marker: as of Bevy 0.19 `Hdr` is no longer extracted to the render world, so
/// a `&'static Hdr` `ViewQuery` would silently never match and this pass would
/// stop running. The pipeline targets `Rgba16Float`, so it must only run against
/// an HDR camera's intermediate; the body early-returns for any non-HDR camera.
/// The main Line `Camera2d` is the only Core2d camera, so this matches just it.
/// (The hand-mesh overlay is a `Camera3d` on the Core3d graph — see
/// `crate::line::hand_mesh` — so it never reaches this Core2d system regardless
/// of its HDR setting.) The gate is kept defensively: an earlier Plan 11.6
/// design added a second, non-HDR `Camera2d`; without it wgpu panicked on the
/// `Rgba8UnormSrgb` ↔ `Rgba16Float` attachment mismatch.
///
/// [`Core2d`]: bevy::core_pipeline::Core2d
/// [`Core2dSystems`]: bevy::core_pipeline::Core2dSystems
pub fn line_post_process(
    view: ViewQuery<'_, '_, (&'static ViewTarget, &'static ExtractedCamera)>,
    post_params: Option<Res<'_, LinePostParams>>,
    pipeline_res: Option<Res<'_, PostProcessPipeline>>,
    pipeline_cache: Res<'_, PipelineCache>,
    render_queue: Res<'_, RenderQueue>,
    mut render_context: RenderContext<'_, '_>,
) {
    let (view_target, camera) = view.into_inner();
    // Skip non-HDR Core2d cameras: this pass targets `Rgba16Float` and would
    // mismatch an `Rgba8UnormSrgb` attachment (see the doc note above).
    if !camera.hdr {
        return;
    }
    let Some(pipeline_res) = pipeline_res else {
        return;
    };
    let Some(pipeline) = pipeline_cache.get_render_pipeline(pipeline_res.pipeline_id) else {
        return;
    };
    let Some(post_params) = post_params else {
        return;
    };

    // Upload current LinePostParams into the persistent uniform buffer.
    // `write_buffer` is a staged copy — no allocation after init.
    render_queue.0.write_buffer(
        &pipeline_res.post_params_buffer,
        0,
        bytemuck::bytes_of(&*post_params),
    );

    let post_process_write = view_target.post_process_write();
    let source = post_process_write.source;
    let destination = post_process_write.destination;

    let layout = pipeline_cache.get_bind_group_layout(&pipeline_res.bind_group_layout_descriptor);

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
            multiview_mask: None,
        });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, &bind_group, &[]);
    pass.draw(0..3, 0..1);
}
