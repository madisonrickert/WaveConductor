//! Render-graph node + pipeline cache for the dual-Kawase backdrop blur.
//!
//! ## Source strategy
//!
//! [`backdrop_blur`] is a render system in the `Core2d` schedule. It reads the
//! primary camera's [`ViewTarget`] via a [`ViewQuery`] and calls
//! [`ViewTarget::post_process_write`] to get a ping-pong write token whose
//! `source` field is the post-tonemap LDR colour attachment for the current
//! frame — no separate extraction or one-frame lag.
//!
//! The `ViewTarget` is left in a post-process-written state after the node runs.
//! Downstream nodes (egui, upscaling) that also use the `ViewTarget` will see
//! the blurred-then-restored image as their source. To avoid corrupting the
//! `ViewTarget`, this node reads from `source` and writes only to the
//! [`BackdropBlurTexture`] (an independent texture). The `ViewTarget` is not
//! written back to — the `post_process_write()` token is dropped without calling
//! any write-back methods.
//!
//! ## Kawase chain
//!
//! Six render passes in sequence (3 down + 3 up). Each pass uses a 1.0×
//! texel offset:
//!
//! 1. `source (full-res)`  → `scratch.half`       — downsample
//! 2. `scratch.half`       → `scratch.quarter`    — downsample
//! 3. `scratch.quarter`    → `scratch.eighth`     — downsample
//! 4. `scratch.eighth`     → `scratch.quarter`    — upsample
//! 5. `scratch.quarter`    → `scratch.half`       — upsample
//! 6. `scratch.half`       → `BackdropBlurTexture` — upsample (final output)
//!
//! ## Run conditions
//!
//! The system returns early (no passes) when any of:
//! - [`BackdropBlurEnabled`]`.0 == false`
//! - [`ExtractedUiOpacity`]`.0 < 0.01` (chrome is fully faded)
//! - [`BackdropBlurTexture`] or [`BackdropBlurScratch`] not yet allocated
//! - Pipelines still compiling (first few frames)
//!
//! [`ViewTarget`]: bevy::render::view::ViewTarget
//! [`ViewQuery`]: bevy::render::renderer::ViewQuery

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "u32/u64 ↔ usize/f32 casts for GPU buffer sizes and texel computations are intentional"
)]

use bevy::prelude::*;
use bevy::render::render_resource::{
    BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntry, BindingType,
    BufferBindingType, BufferUsages, CachedRenderPipelineId, ColorTargetState, ColorWrites,
    FragmentState, LoadOp, MultisampleState, Operations, PipelineCache, PrimitiveState,
    RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor, SamplerBindingType,
    ShaderStages, ShaderType, StoreOp, TextureFormat, TextureSampleType, TextureView,
    TextureViewDimension, VertexState,
};
use bevy::render::renderer::{RenderContext, RenderQueue, ViewQuery};
use bevy::render::view::ViewTarget;
use bevy::render::{Extract, ExtractSchedule, Render, RenderSystems};

use super::{BackdropBlurEnabled, BackdropBlurScratch, BackdropBlurTexture};

/// Asset path for the Kawase downsample WGSL shader, relative to `assets/`.
const DOWNSAMPLE_SHADER: &str = "shaders/backdrop_blur/downsample.wgsl";

/// Asset path for the Kawase upsample WGSL shader, relative to `assets/`.
const UPSAMPLE_SHADER: &str = "shaders/backdrop_blur/upsample.wgsl";

/// Uniform data uploaded to the GPU for each Kawase blur pass.
///
/// Matches the `Uniforms` struct declared in both
/// `assets/shaders/backdrop_blur/downsample.wgsl` and
/// `assets/shaders/backdrop_blur/upsample.wgsl`.
///
/// `texel_size` is `vec2<f32>(1.0 / width, 1.0 / height)` in UV space for
/// the *input* texture of that pass. `_pad` satisfies `vec2<f32>` alignment
/// so the struct occupies exactly 16 bytes (one `vec4`).
#[repr(C)]
#[derive(Copy, Clone, ShaderType, Default)]
pub(super) struct BlurUniforms {
    /// Reciprocal of the input texture dimensions in each axis (`1/w`, `1/h`).
    /// Used by the shader to compute per-tap UV offsets.
    pub texel_size: Vec2,
    /// Explicit padding to reach a 16-byte aligned struct size.
    pub _pad: Vec2,
}

/// Cached render-pipeline state for the dual-Kawase backdrop blur.
///
/// Lives in the [`RenderApp`]; created once via [`FromWorld`] in
/// [`BackdropBlurPlugin::finish`]. The bind-group layout descriptor and queued
/// pipeline IDs are reused every frame — no per-frame GPU allocation.
///
/// The live [`BindGroupLayout`] object is retrieved at bind-group creation
/// time via [`PipelineCache::get_bind_group_layout`].
///
/// [`RenderApp`]: bevy::render::RenderApp
/// [`BindGroupLayout`]: bevy::render::render_resource::BindGroupLayout
#[derive(Resource)]
pub struct BackdropBlurPipeline {
    /// Bind-group layout descriptor shared by both the downsample and upsample
    /// pipelines.
    ///
    /// Describes three bindings at `@group(0)`:
    /// - `binding 0`: filterable 2-D texture.
    /// - `binding 1`: filtering sampler.
    /// - `binding 2`: uniform buffer (`BlurUniforms`).
    pub bind_group_layout_descriptor: BindGroupLayoutDescriptor,

    /// Queued pipeline ID for the Kawase downsample pass.
    ///
    /// Retrieve the compiled pipeline via
    /// [`PipelineCache::get_render_pipeline`] inside the node's `run` body.
    pub downsample: CachedRenderPipelineId,

    /// Queued pipeline ID for the Kawase upsample pass.
    ///
    /// Retrieve the compiled pipeline via
    /// [`PipelineCache::get_render_pipeline`] inside the node's `run` body.
    pub upsample: CachedRenderPipelineId,

    /// Handle to the downsample shader asset. Held here to keep the
    /// [`Shader`] asset alive while the pipeline is in use.
    pub downsample_shader: Handle<Shader>,

    /// Handle to the upsample shader asset. Held here to keep the
    /// [`Shader`] asset alive while the pipeline is in use.
    pub upsample_shader: Handle<Shader>,
}

impl FromWorld for BackdropBlurPipeline {
    fn from_world(world: &mut World) -> Self {
        // Build the bind-group layout entries for the three WGSL bindings at
        // @group(0). We use raw `BindGroupLayoutEntry` structs to match the
        // existing wc-core convention and avoid pulling in unneeded macros.
        let entries = [
            BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::FRAGMENT,
                ty: BindingType::Texture {
                    sample_type: TextureSampleType::Float { filterable: true },
                    view_dimension: TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            BindGroupLayoutEntry {
                binding: 1,
                visibility: ShaderStages::FRAGMENT,
                ty: BindingType::Sampler(SamplerBindingType::Filtering),
                count: None,
            },
            BindGroupLayoutEntry {
                binding: 2,
                visibility: ShaderStages::FRAGMENT,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: Some(BlurUniforms::min_size()),
                },
                count: None,
            },
        ];

        let bind_group_layout_descriptor =
            BindGroupLayoutDescriptor::new("backdrop_blur_layout", &entries);

        // Load the shader assets. The handles are stored on the resource to
        // keep the Shader assets alive in the AssetServer while the pipelines
        // are in use.
        let downsample_shader: Handle<Shader> =
            world.resource::<AssetServer>().load(DOWNSAMPLE_SHADER);
        let upsample_shader: Handle<Shader> = world.resource::<AssetServer>().load(UPSAMPLE_SHADER);

        // Queue both pipelines. `queue_render_pipeline` returns immediately
        // with a `CachedRenderPipelineId`; the actual GPU compilation is
        // deferred. The node checks `get_render_pipeline(id)` before issuing
        // draw calls.
        let make_descriptor = |label: &'static str, shader: Handle<Shader>| {
            RenderPipelineDescriptor {
                label: Some(label.into()),
                layout: vec![bind_group_layout_descriptor.clone()],
                immediate_size: 0,
                vertex: VertexState {
                    shader: shader.clone(),
                    shader_defs: vec![],
                    // Fullscreen triangle is index-driven; no vertex buffers.
                    entry_point: Some("vs_main".into()),
                    buffers: vec![],
                },
                fragment: Some(FragmentState {
                    shader,
                    shader_defs: vec![],
                    entry_point: Some("fs_main".into()),
                    // `Rgba16Float` matches both the scratch textures created
                    // in `ensure_blur_texture` and the camera's HDR view
                    // target. The Kawase chain runs entirely in linear HDR
                    // (no gamma/sRGB conversion in the WGSL — the shaders are
                    // straight weighted averages), so the upsample output is
                    // already in the right colour space for the composite
                    // pipeline downstream.
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
            }
        };

        let downsample =
            world
                .resource_mut::<PipelineCache>()
                .queue_render_pipeline(make_descriptor(
                    "backdrop_blur_downsample",
                    downsample_shader.clone(),
                ));

        let upsample =
            world
                .resource_mut::<PipelineCache>()
                .queue_render_pipeline(make_descriptor(
                    "backdrop_blur_upsample",
                    upsample_shader.clone(),
                ));

        Self {
            bind_group_layout_descriptor,
            downsample,
            upsample,
            downsample_shader,
            upsample_shader,
        }
    }
}

/// Run-count proxy resource used by integration tests to verify the node
/// logic executed at least once.
///
/// Incremented by [`prepare_blur_run_count`] in [`RenderSystems::Prepare`]
/// rather than inside the node's `run` body (the node cannot safely mutate
/// world resources during its run). Tests assert `BlurNodeRunCount::0 >= 1`
/// after calling `App::update()` when blur is enabled.
#[derive(Resource, Default)]
pub struct BlurNodeRunCount(pub u32);

/// Snapshot of main-world [`UiOpacity::current`] extracted into the render
/// world each frame via [`extract_ui_opacity`].
///
/// The blur node reads this to decide whether to skip the Kawase passes when
/// the overlay chrome is fully faded out (opacity < 1 %).
///
/// [`UiOpacity::current`]: crate::ui::auto_fade::UiOpacity::current
#[derive(Resource, Default)]
pub struct ExtractedUiOpacity(pub f32);

/// Render system that runs the dual-Kawase backdrop-blur chain.
///
/// Added to the `Core2d` schedule (after the `PostProcess` set, before
/// `bevy_egui`'s pass). Reads the primary camera's [`ViewTarget`] via a
/// [`ViewQuery`], samples `view_target.post_process_write().source` (the
/// post-tonemap LDR colour) and writes the blurred result into
/// [`BackdropBlurTexture`].
///
/// A no-op (returns early) when [`BackdropBlurEnabled`] is `false`, when
/// [`ExtractedUiOpacity`] is below 1 %, or when any required resource is absent.
///
/// # Panics
///
/// Panics only on an internal invariant violation: `BlurUniforms` is sized via
/// `min_size()`, so the per-pass staging-buffer write cannot fail.
///
/// [`ViewQuery`]: bevy::render::renderer::ViewQuery
#[allow(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    reason = "six-pass Kawase chain (lines) + Bevy render-system params (arguments)"
)]
pub fn backdrop_blur(
    view: ViewQuery<'_, '_, &'static ViewTarget>,
    enabled: Option<Res<'_, BackdropBlurEnabled>>,
    opacity: Option<Res<'_, ExtractedUiOpacity>>,
    blur_texture: Option<Res<'_, BackdropBlurTexture>>,
    scratch: Option<Res<'_, BackdropBlurScratch>>,
    pipeline_res: Option<Res<'_, BackdropBlurPipeline>>,
    pipeline_cache: Res<'_, PipelineCache>,
    render_queue: Res<'_, RenderQueue>,
    mut render_context: RenderContext<'_, '_>,
) {
    // --- Run conditions ---

    if !enabled.is_some_and(|e| e.0) {
        return;
    }
    if opacity.map_or(0.0, |o| o.0) < 0.01 {
        return;
    }

    let (Some(blur_texture), Some(scratch), Some(pipeline_res)) =
        (blur_texture, scratch, pipeline_res)
    else {
        return;
    };

    let Some(down_pipeline) = pipeline_cache.get_render_pipeline(pipeline_res.downsample) else {
        return;
    };
    let Some(up_pipeline) = pipeline_cache.get_render_pipeline(pipeline_res.upsample) else {
        return;
    };

    // --- Source ---
    //
    // `post_process_write` gives access to the post-tonemap colour via a
    // ping-pong token. We only read `source`; we do NOT write back via the
    // token (the blur output goes to `BackdropBlurTexture`, a separate
    // texture). The token is dropped unused — safe, no write-back is called.
    let view_target = view.into_inner();
    let post_process = view_target.post_process_write();
    let source_view: &TextureView = post_process.source;

    // Source full resolution (derived from scratch.half_extent * 2).
    let full_res = UVec2::new(scratch.half_extent.x * 2, scratch.half_extent.y * 2);

    // --- Helpers ---
    let layout = pipeline_cache.get_bind_group_layout(&pipeline_res.bind_group_layout_descriptor);

    // `RenderDevice` is cloned (cheap Arc handle) so the per-pass closures can
    // build buffers/bind groups while `render_context` is borrowed mutably for
    // `command_encoder()`. Reborrowing it as `&mut` lets each `encode_pass`
    // call auto-reborrow instead of moving it.
    let device = render_context.render_device().clone();
    let render_context = &mut render_context;

    let make_uniform_buffer = |texel: Vec2| -> bevy::render::render_resource::Buffer {
        use bevy::render::render_resource::encase;

        let uniforms = BlurUniforms {
            texel_size: texel,
            _pad: Vec2::ZERO,
        };
        let buf = device.create_buffer(&bevy::render::render_resource::BufferDescriptor {
            label: Some("backdrop_blur_uniforms"),
            size: BlurUniforms::min_size().get(),
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut staging = encase::UniformBuffer::new(Vec::<u8>::with_capacity(
            BlurUniforms::min_size().get() as usize,
        ));
        // `write` fails only if the buffer is too small; we sized it via
        // `BlurUniforms::min_size()`, so this is an invariant violation and a
        // panic is correct.
        #[allow(clippy::expect_used)]
        staging
            .write(&uniforms)
            .expect("BlurUniforms: write to staging buffer");
        render_queue.write_buffer(&buf, 0, staging.as_ref());
        buf
    };

    // Encode one Kawase pass. Builds a transient bind group, begins a render
    // pass, draws a fullscreen triangle, and drops the pass encoder.
    // `device` is captured by ref; `layout`, `blur_texture.sampler` are
    // borrowed from outer scope. `render_context` is passed in mutably so
    // the closure does not need to capture it (avoids the simultaneous
    // borrow conflict with `make_uniform_buffer`).
    let encode_pass = |render_context: &mut RenderContext<'_, '_>,
                       input_view: &TextureView,
                       output_view: &TextureView,
                       pipeline: &bevy::render::render_resource::RenderPipeline,
                       input_size: UVec2,
                       pass_label: &'static str| {
        let texel = Vec2::new(
            1.0 / input_size.x.max(1) as f32,
            1.0 / input_size.y.max(1) as f32,
        );
        let uniform_buf = make_uniform_buffer(texel);
        let bind_group = device.create_bind_group(
            Some(pass_label),
            &layout,
            &BindGroupEntries::sequential((
                input_view,
                &blur_texture.sampler,
                uniform_buf.as_entire_binding(),
            )),
        );
        let mut pass = render_context
            .command_encoder()
            .begin_render_pass(&RenderPassDescriptor {
                label: Some(pass_label),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: output_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        // Clear to transparent black (`wgpu::Color::default()`).
                        #[allow(clippy::default_trait_access)]
                        load: LoadOp::Clear(Default::default()),
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
    };

    // --- Six-pass Kawase chain (3 down + 3 up) ---
    //
    // Downsample: source(full) → half → quarter → eighth
    encode_pass(
        render_context,
        source_view,
        &scratch.half_view,
        down_pipeline,
        full_res,
        "backdrop_blur_down_half",
    );
    encode_pass(
        render_context,
        &scratch.half_view,
        &scratch.quarter_view,
        down_pipeline,
        scratch.half_extent,
        "backdrop_blur_down_quarter",
    );
    encode_pass(
        render_context,
        &scratch.quarter_view,
        &scratch.eighth_view,
        down_pipeline,
        scratch.quarter_extent,
        "backdrop_blur_down_eighth",
    );

    // Upsample: eighth → quarter → half → BackdropBlurTexture
    encode_pass(
        render_context,
        &scratch.eighth_view,
        &scratch.quarter_view,
        up_pipeline,
        scratch.eighth_extent,
        "backdrop_blur_up_quarter",
    );
    encode_pass(
        render_context,
        &scratch.quarter_view,
        &scratch.half_view,
        up_pipeline,
        scratch.quarter_extent,
        "backdrop_blur_up_half",
    );
    encode_pass(
        render_context,
        &scratch.half_view,
        &blur_texture.view,
        up_pipeline,
        scratch.half_extent,
        "backdrop_blur_up_final",
    );
}

/// Extract `UiOpacity::current` from the main world into [`ExtractedUiOpacity`]
/// in the render world each frame.
///
/// Registered in [`ExtractSchedule`] by [`BackdropBlurPlugin::setup_render_app`].
pub fn extract_ui_opacity(
    mut commands: Commands<'_, '_>,
    opacity: Extract<'_, '_, Res<'_, crate::ui::auto_fade::UiOpacity>>,
) {
    commands.insert_resource(ExtractedUiOpacity(opacity.current));
}

/// Increment [`BlurNodeRunCount`] each frame the blur passes would execute.
///
/// This system acts as a proxy for the node's run — tests assert against the
/// counter rather than depending on actual GPU draw calls, which require a
/// real render device and GPU adapter.
///
/// Registered in [`RenderSystems::Prepare`] by [`BackdropBlurPlugin::setup_render_app`].
pub fn prepare_blur_run_count(
    enabled: Res<'_, BackdropBlurEnabled>,
    opacity: Res<'_, ExtractedUiOpacity>,
    mut counter: ResMut<'_, BlurNodeRunCount>,
) {
    if enabled.0 && opacity.0 >= 0.01 {
        counter.0 = counter.0.wrapping_add(1);
    }
}

/// Add the blur system to the `Core2d` schedule and register extraction +
/// prepare systems.
///
/// Called from [`BackdropBlurPlugin::setup_render_app`]. Ordered after
/// [`Core2dSystems::PostProcess`] (so it reads the post-tonemap LDR colour) and
/// before `bevy_egui`'s pass (so panels composite over the fresh blur with no
/// one-frame lag). The blur writes only to [`BackdropBlurTexture`]; the view
/// target is left untouched.
///
/// [`Core2dSystems::PostProcess`]: bevy::core_pipeline::Core2dSystems::PostProcess
pub(super) fn setup_render_graph(render_app: &mut bevy::app::SubApp) {
    use bevy::core_pipeline::{Core2d, Core2dSystems};

    render_app.add_systems(
        Core2d,
        backdrop_blur
            .after(Core2dSystems::PostProcess)
            .before(bevy_egui::render::egui_pass),
    );
}

/// Register the extraction and prepare systems in the given render sub-app.
///
/// Called from [`BackdropBlurPlugin::setup_render_app`].
pub(super) fn setup_render_systems(render_app: &mut bevy::app::SubApp) {
    render_app
        .init_resource::<BlurNodeRunCount>()
        .init_resource::<ExtractedUiOpacity>()
        .add_systems(ExtractSchedule, extract_ui_opacity)
        .add_systems(
            Render,
            prepare_blur_run_count.in_set(RenderSystems::Prepare),
        );
}
