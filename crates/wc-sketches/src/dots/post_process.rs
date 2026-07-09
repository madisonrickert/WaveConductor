//! Explode post-process pipeline for the Dots (Fabric) sketch.
//!
//! ## Render schedule
//!
//! [`dots_post_process`] is a render system in the Core2d schedule's
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
//! [`DotsPostParams`] is inserted `OnEnter(AppState::Dots)` by
//! [`crate::dots`] and removed `OnExit`. [`ExtractResourcePlugin`] mirrors it
//! into the render world each frame. A persistent uniform buffer is allocated
//! once in [`DotsPostProcessPipeline::from_world`]; each frame the node uploads
//! the latest snapshot via `queue.write_buffer` — no per-frame GPU allocation.
//!
//! ## Shader
//!
//! `assets/shaders/dots/explode.wgsl` is the WGSL port of v4's
//! `src/sketches/dots/shaders/explode/fragment.glsl`. A fullscreen-triangle
//! vertex stage lives in the same file.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "u32 ↔ usize casts for GPU buffer sizes are intentional"
)]

use std::num::NonZeroU64;

use bevy::core_pipeline::{Core2d, Core2dSystems};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::camera::ExtractedCamera;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_resource::{
    binding_types::{sampler, texture_2d, uniform_buffer_sized},
    BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, Buffer,
    BufferDescriptor, BufferUsages, CachedRenderPipelineId, ColorTargetState, ColorWrites,
    FragmentState, LoadOp, MultisampleState, Operations, PipelineCache, PrimitiveState,
    RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor, Sampler,
    SamplerBindingType, SamplerDescriptor, ShaderStages, StoreOp, TextureFormat, TextureSampleType,
    TextureViewId, VertexState,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::view::ViewTarget;
use bevy::render::{Extract, ExtractSchedule, RenderApp};
use bevy::shader::Shader;
use bytemuck::{Pod, Zeroable};

/// Uniform layout for the explode post-process shader. Mirrors `struct PostParams`
/// in `assets/shaders/dots/explode.wgsl`.
///
/// Field order is `i_resolution`, `i_mouse`, `shrink_factor`, `gamma` — must
/// match the WGSL struct declaration exactly so wgpu's layout aligns without
/// padding. All fields are `f32`; two `vec2<f32>` each consume 8 bytes.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable, Resource, ExtractResource)]
pub struct DotsPostParams {
    /// Render-target resolution in pixels (width, height).
    pub i_resolution: [f32; 2],
    /// Cursor position in normalised UV coordinates `[0,1]^2` (matches v4's
    /// shader convention). Task 2 will drive this from the live cursor; for
    /// Task 1 it is initialised to `[0.5, 0.5]` (screen centre).
    pub i_mouse: [f32; 2],
    /// Per-iteration shrink factor. v4 default = 0.98 — each channel sample
    /// contracts the UV by this factor relative to the previous iteration.
    pub shrink_factor: f32,
    /// Per-channel gamma curve applied as the final output step.
    /// v4 default = 1.0 (identity).
    pub gamma: f32,
}

/// Compile-time validated `DotsPostParams` size for the uniform bind-group
/// entry. Mirrors the `POST_PARAMS_SIZE` pattern in
/// [`crate::line::post_process`].
pub const POST_PARAMS_SIZE: NonZeroU64 =
    match NonZeroU64::new(std::mem::size_of::<DotsPostParams>() as u64) {
        Some(n) => n,
        None => panic!("DotsPostParams must be non-zero-sized"),
    };

/// Plugin that registers the explode post-process render node and its uniform
/// resource. Adds [`ExtractResourcePlugin<DotsPostParams>`] so the render
/// world sees the latest uniforms each frame.
///
/// `DotsPostParams` itself is **not** initialised globally by this plugin —
/// it is inserted `OnEnter(AppState::Dots)` and removed `OnExit` in
/// [`crate::dots`], so the render system no-ops outside Dots.
pub struct DotsPostProcessPlugin;

impl Plugin for DotsPostProcessPlugin {
    fn build(&self, app: &mut App) {
        // Register the extract plugin so the render world gets DotsPostParams.
        // The resource is *not* init'd here — it is inserted per-sketch in dots/mod.rs.
        app.add_plugins(ExtractResourcePlugin::<DotsPostParams>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        // Run in EarlyPostProcess: after the main pass (scene texture holds the
        // rendered dots grid) and before `PostProcess`, where bloom and
        // tonemapping read the exploded HDR result.
        render_app.add_systems(
            Core2d,
            dots_post_process.in_set(Core2dSystems::EarlyPostProcess),
        );

        // Explicitly remove the render-world copy when the main-world resource
        // is gone. `ExtractResourcePlugin` propagates inserts and updates but
        // NOT removals (verified against bevy_render 0.19 extract_resource.rs —
        // the None branch is a complete no-op). Without this, `DotsPostParams`
        // lingers in the render world after `OnExit(AppState::Dots)`, causing
        // `dots_post_process` to keep running the explode pass on other sketches
        // with stale uniform values.
        render_app.add_systems(ExtractSchedule, remove_dots_post_params_if_absent);
    }

    fn finish(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.init_resource::<DotsPostProcessPipeline>();
    }
}

/// Cached render-pipeline state for the explode post-process. Initialised once
/// in [`DotsPostProcessPlugin::finish`] (after the render sub-app is fully set
/// up so `AssetServer`, `PipelineCache`, and `RenderDevice` are all available).
#[derive(Resource)]
pub struct DotsPostProcessPipeline {
    /// Bind-group layout descriptor retained so [`dots_post_process`]
    /// can fetch the cached `BindGroupLayout` from the [`PipelineCache`].
    pub bind_group_layout_descriptor: BindGroupLayoutDescriptor,
    /// Filtering sampler used to read the scene texture.
    pub sampler: Sampler,
    /// Handle into Bevy's `PipelineCache` for the explode render pipeline.
    pub pipeline_id: CachedRenderPipelineId,
    /// Persistent uniform buffer for [`DotsPostParams`].
    ///
    /// Allocated once with `UNIFORM | COPY_DST` and updated each frame via
    /// `queue.write_buffer` in [`dots_post_process`] — avoids a GPU buffer
    /// allocation every frame. Mirrors `PostProcessPipeline::post_params_buffer`
    /// in [`crate::line::post_process`].
    pub post_params_buffer: Buffer,
}

impl FromWorld for DotsPostProcessPipeline {
    fn from_world(world: &mut World) -> Self {
        let render_device = world.resource::<RenderDevice>();

        // Bind-group layout: uniform | texture_2d | sampler (mirrors Line).
        let entries = BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                uniform_buffer_sized(false, Some(POST_PARAMS_SIZE)),
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
            ),
        );
        let bind_group_layout_descriptor =
            BindGroupLayoutDescriptor::new("dots_explode_post_layout", &entries);

        let sampler = render_device.create_sampler(&SamplerDescriptor::default());

        // Persistent uniform buffer — no per-frame allocation.
        let post_params_buffer = render_device.create_buffer(&BufferDescriptor {
            label: Some("dots_post_params"),
            size: std::mem::size_of::<DotsPostParams>() as u64,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let shader: Handle<Shader> = world
            .resource::<AssetServer>()
            .load("shaders/dots/explode.wgsl");

        let pipeline_id =
            world
                .resource_mut::<PipelineCache>()
                .queue_render_pipeline(RenderPipelineDescriptor {
                    label: Some("dots_explode_pipeline".into()),
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
                        // which is `Rgba16Float` while internal-HDR rendering is
                        // on. The format must match the view target's HDR format
                        // (wgpu validates at draw time). Mirrors Line's pipeline.
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

/// Removes the render-world [`DotsPostParams`] when the main world no longer
/// has it — i.e. after `OnExit(AppState::Dots)` fires.
///
/// [`ExtractResourcePlugin`] propagates inserts and updates each
/// [`ExtractSchedule`] tick but does **not** issue `remove_resource` when the
/// main-world source is absent (verified against `bevy_render` 0.19
/// `extract_resource.rs`: the `None` arm is a no-op). Without this explicit
/// removal the stale render-world copy keeps [`dots_post_process`] running
/// Dots' explode pass on other sketches with stale uniform values.
///
/// The two [`ExtractSchedule`] systems — this one and the
/// [`ExtractResourcePlugin`]'s own insert/update system — guard on mutually
/// exclusive conditions (`main_resource.is_none()` vs `is_some()`), so there
/// is no ordering conflict between them.
fn remove_dots_post_params_if_absent(
    mut commands: Commands<'_, '_>,
    main_resource: Extract<'_, '_, Option<Res<'_, DotsPostParams>>>,
    render_resource: Option<Res<'_, DotsPostParams>>,
) {
    if main_resource.is_none() && render_resource.is_some() {
        commands.remove_resource::<DotsPostParams>();
    }
}

/// Render system that draws the explode post-process pass.
///
/// Added to the [`Core2d`] schedule in [`Core2dSystems::EarlyPostProcess`] (so
/// the scene texture holds the rendered dots grid, and the explode runs in HDR
/// before bloom/tonemapping). Uploads [`DotsPostParams`] into the persistent
/// uniform buffer via `queue.write_buffer`, fetches a bind group cached by the
/// ping-pong source view (no per-frame allocation), then issues a 3-vertex
/// fullscreen-triangle draw.
///
/// Gates on [`ExtractedCamera::hdr`] (not the `Hdr` marker component, which is
/// no longer extracted to the render world as of Bevy 0.19). The pipeline
/// targets `Rgba16Float`, so it must only run against an HDR camera's
/// intermediate. Also early-returns when `DotsPostParams` is absent from the
/// render world — which is the case outside `AppState::Dots` since the resource
/// is inserted only `OnEnter(Dots)`.
///
/// [`Core2d`]: bevy::core_pipeline::Core2d
/// [`Core2dSystems`]: bevy::core_pipeline::Core2dSystems
pub fn dots_post_process(
    view: ViewQuery<'_, '_, (&'static ViewTarget, &'static ExtractedCamera)>,
    post_params: Option<Res<'_, DotsPostParams>>,
    pipeline_res: Option<Res<'_, DotsPostProcessPipeline>>,
    pipeline_cache: Res<'_, PipelineCache>,
    render_queue: Res<'_, RenderQueue>,
    mut bind_group_cache: Local<'_, (Option<UVec2>, HashMap<TextureViewId, BindGroup>)>,
    mut render_context: RenderContext<'_, '_>,
) {
    let (view_target, camera) = view.into_inner();
    // Skip non-HDR Core2d cameras: this pass targets `Rgba16Float` and would
    // mismatch an `Rgba8UnormSrgb` attachment.
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
        // DotsPostParams absent → not in Dots state; no-op.
        return;
    };

    // Upload current DotsPostParams into the persistent uniform buffer.
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

    // Reuse the bind group for this source view if we have built it before.
    // `post_process_write` cycles `source` between two stable views, so after the
    // first two frames every frame is a cache hit — no per-frame
    // `create_bind_group` on the render hot path (the project's
    // no-hot-path-allocation rule). The other two entries (persistent uniform
    // buffer + sampler) never change, and `write_buffer` updates the uniform
    // contents without invalidating the binding.
    //
    // A resize reallocates the view targets, minting new `TextureViewId`s. We
    // clear the map on that transition, dropping the bind groups that still
    // referenced the old (now freed) full-screen HDR targets. Without this the
    // map would grow by two entries per resize for the life of the process —
    // each pinning an `Rgba16Float` screen-sized texture. Steady state holds
    // exactly two entries. Same shape as `hand_mesh::bone_composite`.
    let target_size = camera.physical_target_size;
    if bind_group_cache.0 != target_size {
        bind_group_cache.1.clear();
        bind_group_cache.0 = target_size;
    }
    let bind_group = bind_group_cache.1.entry(source.id()).or_insert_with(|| {
        render_context.render_device().create_bind_group(
            "dots_explode_post_bind_group",
            &layout,
            &BindGroupEntries::sequential((
                pipeline_res.post_params_buffer.as_entire_binding(),
                source,
                &pipeline_res.sampler,
            )),
        )
    });

    let mut pass = render_context
        .command_encoder()
        .begin_render_pass(&RenderPassDescriptor {
            label: Some("dots_explode_post_pass"),
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
    pass.set_bind_group(0, &*bind_group, &[]);
    pass.draw(0..3, 0..1);
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions — panicking on unexpected None is the correct behaviour"
)]
mod tests {
    use super::*;

    /// Verifies that `DotsPostParams` is non-zero-sized and that `POST_PARAMS_SIZE`
    /// matches its actual byte size. Catches accidental field removal.
    #[test]
    fn post_params_size_matches_const() {
        let actual = std::mem::size_of::<DotsPostParams>() as u64;
        assert_eq!(
            actual,
            POST_PARAMS_SIZE.get(),
            "POST_PARAMS_SIZE must match size_of::<DotsPostParams>()"
        );
        assert!(actual > 0, "DotsPostParams must be non-zero-sized");
    }

    /// Field-order smoke test: the struct must be exactly 6 × f32 = 24 bytes.
    /// `i_resolution: [f32;2]` + `i_mouse: [f32;2]` + `shrink_factor: f32` +
    /// `gamma: f32` = 6 × 4 = 24 bytes (no implicit padding between same-aligned
    /// fields in #[repr(C)]).
    #[test]
    fn post_params_layout_is_six_f32s() {
        assert_eq!(
            std::mem::size_of::<DotsPostParams>(),
            6 * std::mem::size_of::<f32>(),
            "DotsPostParams must be exactly 6 × f32 (24 bytes)"
        );
    }

    /// Default value smoke test: confirms the zero-init (Pod/Zeroable) default
    /// does not carry stale state.
    #[test]
    #[allow(clippy::float_cmp, reason = "comparing literal zero")]
    fn post_params_default_is_zeroed() {
        let p = DotsPostParams::default();
        assert_eq!(p.i_resolution, [0.0, 0.0]);
        assert_eq!(p.i_mouse, [0.0, 0.0]);
        assert_eq!(p.shrink_factor, 0.0);
        assert_eq!(p.gamma, 0.0);
    }
}
