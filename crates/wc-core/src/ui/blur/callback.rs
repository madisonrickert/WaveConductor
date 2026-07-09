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
//! Each panel owns one persistent 32-byte `CompositeUniforms` buffer, created
//! on first paint and rewritten in place every frame via `Queue::write_buffer`.
//! Buffers and bind groups live in `CompositeSlots`, keyed by the widget's
//! stable `egui::Id`, and are evicted after `SLOT_EVICT_FRAMES` frames without
//! a paint. Nothing is allocated on the render hot path.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "u32/f32 casts for pixel-coordinate and UV conversions are intentional"
)]

use bevy::prelude::*;
use bevy_egui::egui;

use bevy::render::render_resource::{
    BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntry, BindingType,
    Buffer, BufferBindingType, BufferDescriptor, BufferUsages, CachedRenderPipelineId,
    ColorTargetState, ColorWrites, FragmentState, MultisampleState, PipelineCache, PrimitiveState,
    RenderPipelineDescriptor, SamplerBindingType, ShaderStages, ShaderType, TextureFormat,
    TextureSampleType, TextureViewDimension, TextureViewId, VertexState,
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

/// GPU resources owned by one frosted widget's composite draw.
///
/// The bind group holds `Arc` references to the blur texture view, the
/// sampler, and `buffer`. Keeping the `BindGroup` alive here — rather than
/// leaking it — is what bounds the app's GPU memory. The bind group is rebuilt
/// only when `blur_view` changes (i.e. on a window resize); the buffer's
/// *contents* are rewritten every frame via `Queue::write_buffer`, which does
/// not invalidate the binding.
pub(crate) struct CompositeGpu {
    /// Per-widget `CompositeUniforms` buffer (32 bytes).
    buffer: Buffer,
    /// Bind group over (blur texture view, sampler, `buffer`).
    bind_group: BindGroup,
    /// Id of the blur texture view this bind group was built against. When the
    /// blur texture is reallocated (resize), the id changes and the bind group
    /// must be rebuilt or it would sample a freed texture.
    blur_view: TextureViewId,
}

/// Render-world storage for every frosted widget's [`CompositeGpu`].
///
/// Populated by [`BackdropBlurPaintCallback::update`], read by
/// [`BackdropBlurPaintCallback::render`], advanced and pruned once per frame by
/// [`tick_composite_slots`].
#[derive(Resource, Default)]
pub(crate) struct CompositeSlots(pub(crate) super::slots::SlotBook<CompositeGpu>);

/// Advance the composite slot book one frame and evict stale widgets.
///
/// Registered in `Render` under `RenderSystems::PrepareResources`, which runs
/// before the render graph — and therefore before `bevy_egui`'s
/// `prepare_egui_pass` node invokes any paint callback's `update`.
pub(crate) fn tick_composite_slots(mut slots: ResMut<'_, CompositeSlots>) {
    slots.0.tick();
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

/// Convert a panel rect in egui points into the [`CompositeUniforms`] the
/// composite shader expects.
///
/// `screen_size_px` is the egui render target size in *physical pixels*;
/// `rect` and `corner_radius_points` are in *egui points*. Both are scaled by
/// `pixels_per_point` where the shader needs physical pixels.
///
/// Returns `None` when either screen dimension is zero, which happens on the
/// first frame before the window reports a size. Callers bail silently.
pub(crate) fn composite_uniforms(
    screen_size_px: [u32; 2],
    pixels_per_point: f32,
    rect: egui::Rect,
    corner_radius_points: f32,
) -> Option<CompositeUniforms> {
    // `screen_size_px` is [width, height] of the egui render target in
    // physical pixels. We use it to normalise the panel rect into UVs.
    let screen_w = screen_size_px[0] as f32;
    let screen_h = screen_size_px[1] as f32;
    if screen_w <= 0.0 || screen_h <= 0.0 {
        return None;
    }

    let ppp = pixels_per_point;
    // Convert panel rect (points) → physical pixels → [0,1] UVs.
    let uv_min = Vec2::new(rect.min.x * ppp / screen_w, rect.min.y * ppp / screen_h);
    let uv_max = Vec2::new(rect.max.x * ppp / screen_w, rect.max.y * ppp / screen_h);

    // Half-extent in physical pixels, used by the SDF in the shader.
    let half_extent = Vec2::new((rect.width() * ppp) * 0.5, (rect.height() * ppp) * 0.5);

    Some(CompositeUniforms {
        uv_rect: Vec4::new(uv_min.x, uv_min.y, uv_max.x, uv_max.y),
        half_extent,
        corner_radius: corner_radius_points * ppp,
        _pad: 0.0,
    })
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
    /// Stable per-widget egui id, used to key this widget's GPU slot in
    /// `SlotBook`. Must be the same value on every frame the widget is
    /// painted, and distinct from every other frosted widget's id. Both
    /// construction sites pass `response.id`, which egui derives from the
    /// containing `Ui` and the widget's allocation order. This invariant is
    /// positional rather than structural: each frosted widget currently lives
    /// in its own `egui::Area` with a fixed string id and is the first
    /// allocation in that `Ui`, which is what makes `response.id` stable and
    /// unique — two blurred widgets sharing one `Ui` would depend on a fixed
    /// allocation order.
    pub id: egui::Id,
    /// Corner radius of the panel in egui points. Converted to physical pixels
    /// at render time via `info.pixels_per_point`.
    pub corner_radius: f32,
    /// Panel bounding rect in egui points. Used to compute UVs into the blur
    /// texture and to derive the SDF half-extent.
    pub rect: egui::Rect,
}

impl EguiBevyPaintCallbackImpl for BackdropBlurPaintCallback {
    /// Create or refresh this widget's `CompositeGpu` slot.
    ///
    /// A plain code span, not an intra-doc link: `update` is a public
    /// trait-impl method while `CompositeGpu` is `pub(crate)`, so a link trips
    /// `rustdoc::private_intra_doc_links`, which CI denies.
    ///
    /// `bevy_egui` calls `update` for every paint callback (from the
    /// `prepare_egui_pass` render-graph node) before it calls `render` for any
    /// of them, so writing here and reading in `render` is sound. We create the
    /// uniform buffer and bind group **once per widget**, not once per frame:
    /// the buffer contents are rewritten with `write_buffer`, and the bind
    /// group is rebuilt only when the blur texture view is reallocated.
    ///
    /// Bails silently on any missing resource, mirroring `render`.
    fn update(
        &self,
        info: egui::PaintCallbackInfo,
        _render_entity: RenderEntity,
        _pipeline_key: EguiPipelineKey,
        world: &mut World,
    ) {
        let Some(uniforms) = composite_uniforms(
            info.screen_size_px,
            info.pixels_per_point,
            self.rect,
            self.corner_radius,
        ) else {
            return;
        };

        // Bail before `resource_scope` panics on a missing resource. In headless
        // tests without a RenderApp the plugin never inits this.
        if world.get_resource::<CompositeSlots>().is_none() {
            return;
        }

        let id = self.id;
        world.resource_scope(|world: &mut World, mut slots: Mut<'_, CompositeSlots>| {
            let Some(pipeline_data) = world.get_resource::<CompositePipeline>() else {
                return;
            };
            let Some(blur_texture) = world.get_resource::<super::BackdropBlurTexture>() else {
                return;
            };
            let pipeline_cache = world.resource::<PipelineCache>();
            let device = world.resource::<RenderDevice>();
            let queue = world.resource::<RenderQueue>();

            let blur_view = blur_texture.view.id();

            // Rebuild only when absent or when the blur texture was reallocated.
            let stale = slots
                .0
                .get(id)
                .is_none_or(|gpu: &CompositeGpu| gpu.blur_view != blur_view);
            if stale {
                let buffer = device.create_buffer(&BufferDescriptor {
                    label: Some("backdrop_blur_composite_uniforms"),
                    size: CompositeUniforms::min_size().get(),
                    usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let layout = pipeline_cache
                    .get_bind_group_layout(&pipeline_data.bind_group_layout_descriptor);
                let bind_group = device.create_bind_group(
                    Some("backdrop_blur_composite_bind_group"),
                    &layout,
                    &BindGroupEntries::sequential((
                        &blur_texture.view,
                        &blur_texture.sampler,
                        buffer.as_entire_binding(),
                    )),
                );
                slots.0.insert(
                    id,
                    CompositeGpu {
                        buffer,
                        bind_group,
                        blur_view,
                    },
                );
            }

            // Rewrite the uniform contents every frame through the reusable
            // staging buffer. `clear()` retains capacity, so steady state does
            // not allocate (the project's no-hot-path-allocation rule).
            let Some((scratch, gpu)) = slots.0.scratch_and_touch(id) else {
                return;
            };
            {
                use bevy::render::render_resource::encase;
                scratch.clear();
                let mut staging = encase::UniformBuffer::new(std::mem::take(scratch));
                // `write` only fails if the staging buffer is too small. `encase`
                // grows a `Vec` backing store as needed, so a failure here is an
                // invariant violation and a panic is correct.
                #[allow(clippy::expect_used)]
                staging
                    .write(&uniforms)
                    .expect("CompositeUniforms: write to staging buffer");
                queue.write_buffer(&gpu.buffer, 0, staging.as_ref());
                *scratch = staging.into_inner();
            }
        });
    }

    /// Draw the blurred backdrop quad.
    ///
    /// All GPU resource creation happened in [`Self::update`]. This method only
    /// looks up the pipeline and this widget's slot, then issues the draw. The
    /// `&'pass BindGroup` that `set_bind_group` requires is borrowed straight
    /// out of `world: &'pass World`, which is why no `Box::leak` is needed.
    ///
    /// The vertex shader triangulates the quad from a const array indexed by
    /// `@builtin(vertex_index)`, so no vertex buffer is bound.
    fn render<'pass>(
        &self,
        _info: egui::PaintCallbackInfo,
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
        let Some(slots) = world.get_resource::<CompositeSlots>() else {
            return;
        };
        // Absent when `update` bailed this frame (e.g. blur texture not yet
        // allocated). The caller's tint rect still paints, so the panel
        // degrades to a solid translucent fill.
        let Some(gpu) = slots.0.get(self.id) else {
            return;
        };

        // --- Draw ---

        render_pass.set_render_pipeline(pipeline);
        render_pass.set_bind_group(0, &gpu.bind_group, &[]);
        render_pass.draw(0..6, 0..1);
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions; expect_used is denied workspace-wide for non-test code"
)]
#[allow(
    clippy::used_underscore_binding,
    reason = "`_pad` is shader struct padding; asserting it stays 0.0 is the point"
)]
mod tests {
    use super::*;

    #[test]
    fn uniforms_map_a_rect_to_normalised_uvs_at_unit_scale() {
        let rect = egui::Rect::from_min_max(egui::pos2(100.0, 50.0), egui::pos2(300.0, 150.0));
        let u = composite_uniforms([1000, 500], 1.0, rect, 8.0).expect("non-zero screen");

        assert!((u.uv_rect.x - 0.1).abs() < 1e-6, "uv min x");
        assert!((u.uv_rect.y - 0.1).abs() < 1e-6, "uv min y");
        assert!((u.uv_rect.z - 0.3).abs() < 1e-6, "uv max x");
        assert!((u.uv_rect.w - 0.3).abs() < 1e-6, "uv max y");
        assert!((u.half_extent.x - 100.0).abs() < 1e-6, "half width in px");
        assert!((u.half_extent.y - 50.0).abs() < 1e-6, "half height in px");
        assert!((u.corner_radius - 8.0).abs() < 1e-6);
        assert!((u._pad - 0.0).abs() < 1e-6);
    }

    #[test]
    fn uniforms_scale_points_to_physical_pixels_by_pixels_per_point() {
        let rect = egui::Rect::from_min_max(egui::pos2(100.0, 50.0), egui::pos2(300.0, 150.0));
        let u = composite_uniforms([1000, 500], 2.0, rect, 8.0).expect("non-zero screen");

        // Rect is in points; screen_size_px is already physical.
        assert!((u.uv_rect.x - 0.2).abs() < 1e-6);
        assert!((u.uv_rect.y - 0.2).abs() < 1e-6);
        assert!((u.uv_rect.z - 0.6).abs() < 1e-6);
        assert!((u.uv_rect.w - 0.6).abs() < 1e-6);
        assert!((u.half_extent.x - 200.0).abs() < 1e-6);
        assert!((u.half_extent.y - 100.0).abs() < 1e-6);
        assert!(
            (u.corner_radius - 16.0).abs() < 1e-6,
            "corner radius is scaled too"
        );
    }

    #[test]
    fn uniforms_bail_on_a_zero_sized_screen() {
        let rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(10.0, 10.0));
        assert!(composite_uniforms([0, 500], 1.0, rect, 0.0).is_none());
        assert!(composite_uniforms([1000, 0], 1.0, rect, 0.0).is_none());
    }
}
