//! Egui paint callback that samples [`super::BackdropBlurTexture`] and
//! draws a textured quad with a corner-radius SDF mask.
//!
//! ## Compositing order
//!
//! The callback is constructed by [`super::super::frame::backdrop_blur_frame`]
//! and pushed into the egui paint list before the panel's translucent tint
//! rect. Back-to-front order: blurred backdrop → translucent tint → content.
//!
//! ## What happens when the texture is not ready
//!
//! If [`super::BackdropBlurTexture`] has not been allocated yet (first frame)
//! or [`CompositePipeline`] is still compiling, [`BackdropBlurPaintCallback::render`]
//! returns silently. The caller's tint rect is still drawn, so the panel
//! degrades to a solid translucent fill rather than showing nothing.
//!
//! ## Bind-group layout
//!
//! Three bindings at `@group(0)`, matching `assets/shaders/backdrop_blur/composite.wgsl`:
//!
//! | binding | type                        | visibility |
//! |--------|-----------------------------|------------|
//! | 0      | filterable 2D texture       | FRAGMENT   |
//! | 1      | filtering sampler           | FRAGMENT   |
//! | 2      | uniform buffer (`CompositeUniforms`) | FRAGMENT |
//!
//! A fresh per-frame `CompositeUniforms` buffer is uploaded for each panel
//! rect (32 bytes; acceptable once-per-visible-panel cost).

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "u32/f32 casts for pixel-coordinate and UV conversions are intentional"
)]

use bevy::prelude::*;
use bevy_egui::egui;

use bevy::render::render_resource::{
    BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntry, BindingType,
    BufferBindingType, BufferUsages, CachedRenderPipelineId, ColorTargetState, ColorWrites,
    FragmentState, MultisampleState, PipelineCache, PrimitiveState, RenderPipelineDescriptor,
    SamplerBindingType, ShaderStages, ShaderType, TextureFormat, TextureSampleType,
    TextureViewDimension, VertexState,
};
use bevy::render::renderer::{RenderDevice, RenderQueue};
use bevy::render::sync_world::RenderEntity;
use bevy_egui::render::{EguiBevyPaintCallbackImpl, EguiPipelineKey};

/// Asset path for the composite WGSL shader, relative to `assets/`.
const COMPOSITE_SHADER: &str = "shaders/backdrop_blur/composite.wgsl";

/// Cached render-pipeline state for the blur-composite paint callback.
///
/// Lives in the [`RenderApp`]; created once via [`FromWorld`] in
/// [`BackdropBlurPlugin::finish`](super::BackdropBlurPlugin). The bind-group
/// layout descriptor and queued pipeline ID are reused every frame.
///
/// The live [`BindGroupLayout`] object is retrieved at bind-group creation
/// time via [`PipelineCache::get_bind_group_layout`], matching the pattern
/// from [`super::node::BackdropBlurPipeline`].
///
/// [`RenderApp`]: bevy::render::RenderApp
/// [`BindGroupLayout`]: bevy::render::render_resource::BindGroupLayout
#[derive(Resource)]
pub struct CompositePipeline {
    /// Bind-group layout descriptor shared across all callback invocations
    /// in a frame. Retrieved from the pipeline cache at draw time.
    pub bind_group_layout_descriptor: BindGroupLayoutDescriptor,
    /// Queued pipeline ID. Retrieve the compiled pipeline via
    /// [`PipelineCache::get_render_pipeline`] inside the render body.
    pub pipeline: CachedRenderPipelineId,
    /// Handle kept alive so the shader asset is not evicted while the
    /// pipeline is in use.
    pub shader: Handle<Shader>,
}

/// Uniform data uploaded per-panel for the composite draw call.
///
/// Matches the `Uniforms` struct in `assets/shaders/backdrop_blur/composite.wgsl`.
///
/// - `uv_rect`: UV min/max (xy = min, zw = max) of the panel inside the full
///   viewport, in normalised [0, 1] coordinates.
/// - `half_extent`: half-width / half-height of the panel in *physical pixels*,
///   used by the corner-radius SDF.
/// - `corner_radius`: corner radius in physical pixels.
/// - `_pad`: explicit padding to reach a 32-byte aligned struct.
#[repr(C)]
#[derive(Copy, Clone, ShaderType, Default)]
pub(crate) struct CompositeUniforms {
    /// UV bounding rect: `xy` = top-left, `zw` = bottom-right, all in [0, 1].
    pub uv_rect: Vec4,
    /// Half the panel's physical-pixel dimensions (`width/2`, `height/2`).
    pub half_extent: Vec2,
    /// Corner radius in physical pixels (points × `pixels_per_point`).
    pub corner_radius: f32,
    /// Explicit struct padding; must stay `0.0`.
    pub _pad: f32,
}

impl FromWorld for CompositePipeline {
    fn from_world(world: &mut World) -> Self {
        // Build the bind-group layout entries for the three WGSL bindings at
        // @group(0). Uses the same raw-entry style as BackdropBlurPipeline so
        // the two resources are consistent and auditable side-by-side.
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
                // `composite.wgsl` reads `uniforms` in both `vs_main` (for
                // UV/half-extent) and `fs_main` (for corner-radius SDF), so
                // the layout must expose the binding to both stages.
                visibility: ShaderStages::VERTEX_FRAGMENT,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: Some(CompositeUniforms::min_size()),
                },
                count: None,
            },
        ];

        let bind_group_layout_descriptor =
            BindGroupLayoutDescriptor::new("backdrop_blur_composite_layout", &entries);

        // Load the composite shader. The handle is held on the resource to
        // prevent the asset from being evicted while the pipeline is active.
        let shader: Handle<Shader> = world.resource::<AssetServer>().load(COMPOSITE_SHADER);

        // Queue the pipeline. Compilation is deferred; `render` checks
        // `get_render_pipeline` before issuing draw calls.
        let pipeline =
            world
                .resource_mut::<PipelineCache>()
                .queue_render_pipeline(RenderPipelineDescriptor {
                    label: Some("backdrop_blur_composite".into()),
                    layout: vec![bind_group_layout_descriptor.clone()],
                    immediate_size: 0,
                    vertex: VertexState {
                        shader: shader.clone(),
                        shader_defs: vec![],
                        // Quad triangulation is handled in the vertex shader by
                        // indexing into a const array of 6 corner positions.
                        entry_point: Some("vs_main".into()),
                        buffers: vec![],
                    },
                    fragment: Some(FragmentState {
                        shader: shader.clone(),
                        shader_defs: vec![],
                        entry_point: Some("fs_main".into()),
                        targets: vec![Some(ColorTargetState {
                            // The composite pipeline writes back into the
                            // camera's view target, which is `Rgba16Float`
                            // while internal-HDR rendering is on (see
                            // `spawn_camera` in the binary crate). The format
                            // here MUST match the view target — wgpu validates
                            // pipeline target formats against the bound
                            // attachment at draw time and rejects mismatches.
                            //
                            // egui's own pass also renders into this same HDR
                            // target. Because backdrop blur runs *after*
                            // tonemapping in the Core2d graph, the values we
                            // sample are already mapped into the SDR range
                            // (clamped softly to ~[0, 1] by AgX) but still
                            // stored as float — the blend below works the same
                            // way it did in 8-bit sRGB, just with more headroom.
                            //
                            // Using ALPHA_BLENDING means coverage from the
                            // corner-radius SDF masks the edges of the panel.
                            format: TextureFormat::Rgba16Float,
                            blend: Some(bevy::render::render_resource::BlendState::ALPHA_BLENDING),
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
            pipeline,
            shader,
        }
    }
}

/// Egui paint callback that composites the blurred backdrop under a panel rect.
///
/// Constructed by [`super::super::frame::backdrop_blur_frame`] and inserted
/// into the egui paint list via
/// [`bevy_egui::render::EguiBevyPaintCallback::new_paint_callback`].
///
/// Each callback carries the panel-specific geometry: the egui-point rect and
/// corner radius. These are converted to physical pixels inside [`render`]
/// using `info.pixels_per_point`.
///
/// [`render`]: BackdropBlurPaintCallback::render
pub struct BackdropBlurPaintCallback {
    /// Corner radius of the panel in egui points. Converted to physical pixels
    /// at render time via `info.pixels_per_point`.
    pub corner_radius: f32,
    /// Panel bounding rect in egui points. Used to compute UVs into the blur
    /// texture and to derive the SDF half-extent.
    pub rect: egui::Rect,
}

impl EguiBevyPaintCallbackImpl for BackdropBlurPaintCallback {
    /// No per-frame update needed. The blur texture is produced by
    /// [`super::node::backdrop_blur`] in a separate render system that
    /// runs before the egui pass.
    fn update(
        &self,
        _info: egui::PaintCallbackInfo,
        _render_entity: RenderEntity,
        _pipeline_key: EguiPipelineKey,
        _world: &mut World,
    ) {
    }

    /// Draw the blurred backdrop quad.
    ///
    /// Steps:
    /// 1. Resolve `CompositePipeline` and `BackdropBlurTexture` from the
    ///    world; bail silently if either is missing.
    /// 2. Convert the egui-point rect to physical-pixel UVs using
    ///    `info.pixels_per_point` and `info.screen_size_px`.
    /// 3. Upload a 32-byte `CompositeUniforms` buffer with the UV rect,
    ///    half-extent, and corner radius.
    /// 4. Build a transient bind group and issue `draw(0..6, 0..1)`.
    ///    The vertex shader triangulates the quad from a const array indexed
    ///    by `@builtin(vertex_index)`, so no vertex buffer is needed.
    fn render<'pass>(
        &self,
        info: egui::PaintCallbackInfo,
        render_pass: &mut bevy::render::render_phase::TrackedRenderPass<'pass>,
        _render_entity: RenderEntity,
        _pipeline_key: EguiPipelineKey,
        world: &'pass World,
    ) {
        // --- Resource lookups — bail silently on any miss ---

        let Some(pipeline_data) = world.get_resource::<CompositePipeline>() else {
            return;
        };
        let pipeline_cache = world.resource::<PipelineCache>();
        let Some(pipeline) = pipeline_cache.get_render_pipeline(pipeline_data.pipeline) else {
            // Pipeline still compiling on the first few frames; not an error.
            return;
        };
        let Some(blur_texture) = world.get_resource::<super::BackdropBlurTexture>() else {
            return;
        };

        // --- Geometry conversion ---

        // `screen_size_px` is [width, height] of the egui render target in
        // physical pixels. We use it to normalise the panel rect into UVs.
        let screen_w = info.screen_size_px[0] as f32;
        let screen_h = info.screen_size_px[1] as f32;
        if screen_w <= 0.0 || screen_h <= 0.0 {
            return;
        }

        let ppp = info.pixels_per_point;
        // Convert panel rect (points) → physical pixels → [0,1] UVs.
        let uv_min = Vec2::new(
            self.rect.min.x * ppp / screen_w,
            self.rect.min.y * ppp / screen_h,
        );
        let uv_max = Vec2::new(
            self.rect.max.x * ppp / screen_w,
            self.rect.max.y * ppp / screen_h,
        );

        // Half-extent in physical pixels, used by the SDF in the shader.
        let half_extent = Vec2::new(
            (self.rect.width() * ppp) * 0.5,
            (self.rect.height() * ppp) * 0.5,
        );

        // --- Uniform buffer upload ---

        let uniforms = CompositeUniforms {
            uv_rect: Vec4::new(uv_min.x, uv_min.y, uv_max.x, uv_max.y),
            half_extent,
            corner_radius: self.corner_radius * ppp,
            _pad: 0.0,
        };

        // Borrow device and queue from world resources. Both are `Arc`-backed
        // handles, so cloning is cheap.
        let device = world.resource::<RenderDevice>();
        let queue = world.resource::<RenderQueue>();

        let buffer = device.create_buffer(&bevy::render::render_resource::BufferDescriptor {
            label: Some("backdrop_blur_composite_uniforms"),
            size: CompositeUniforms::min_size().get(),
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        {
            use bevy::render::render_resource::encase;
            let mut staging = encase::UniformBuffer::new(Vec::<u8>::with_capacity(
                CompositeUniforms::min_size().get() as usize,
            ));
            // `write` only fails if the staging buffer is too small. We sized it
            // via `CompositeUniforms::min_size()`, so a failure is an invariant
            // violation and a panic is correct.
            #[allow(clippy::expect_used)]
            staging
                .write(&uniforms)
                .expect("CompositeUniforms: write to staging buffer");
            queue.write_buffer(&buffer, 0, staging.as_ref());
        }

        // --- Bind group ---

        let layout =
            pipeline_cache.get_bind_group_layout(&pipeline_data.bind_group_layout_descriptor);
        let bind_group = device.create_bind_group(
            Some("backdrop_blur_composite_bind_group"),
            &layout,
            &BindGroupEntries::sequential((
                &blur_texture.view,
                &blur_texture.sampler,
                buffer.as_entire_binding(),
            )),
        );

        // `set_bind_group` requires `&'pass BindGroup` but `bind_group` is
        // stack-local. We extend its lifetime via `Box::leak`. The memory is
        // small (one `BindGroup` ≈ a pointer per panel per frame) and the GPU
        // resource itself is reference-counted internally by wgpu, so leaking
        // the Rust wrapper is safe. The wgpu device reclaims GPU resources on
        // the next frame when the device drops its last reference.
        //
        // Alternative approaches (storing in a world component via `update`,
        // pre-allocating in `CompositePipeline`) were considered but add
        // complexity not yet warranted for 1–3 simultaneous panels.
        let bind_group: &'pass _ = Box::leak(Box::new(bind_group));

        // --- Draw ---

        render_pass.set_render_pipeline(pipeline);
        render_pass.set_bind_group(0, bind_group, &[]);
        // The vertex shader generates a 2-triangle quad from a const array of 6
        // clip-space corners indexed by `@builtin(vertex_index)`.
        render_pass.draw(0..6, 0..1);
    }
}
