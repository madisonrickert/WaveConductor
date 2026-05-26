//! Pipeline cache for the dual-Kawase backdrop blur.
//!
//! This module owns [`BackdropBlurPipeline`], a [`Resource`] that lives in the
//! [`RenderApp`] and caches the bind-group layout descriptor plus queued
//! render-pipeline IDs for the downsample and upsample WGSL programs.
//! Allocation happens once via [`FromWorld`] — [`PipelineCache`] defers the
//! actual GPU compilation to the first frame that uses the pipeline.
//!
//! [`BackdropBlurNode`] (the render-graph node that executes the Kawase
//! passes) is planned for Task 10 and is **not** implemented here. This task
//! covers only the resource and its `FromWorld` constructor.
//!
//! The `BindGroupLayout` is not stored directly because
//! `RenderPipelineDescriptor::layout` expects [`BindGroupLayoutDescriptor`] in
//! Bevy 0.18 — the live layout object is retrieved at bind-group creation time
//! via [`PipelineCache::get_bind_group_layout`]. This matches the pattern used
//! by [`crate::line::post_process::PostProcessPipeline`].
//!
//! [`RenderApp`]: bevy::render::RenderApp

use bevy::prelude::*;
use bevy::render::render_resource::{
    BindGroupLayoutDescriptor, BindGroupLayoutEntry, BindingType, BufferBindingType,
    CachedRenderPipelineId, ColorTargetState, ColorWrites, FragmentState, MultisampleState,
    PipelineCache, PrimitiveState, RenderPipelineDescriptor, SamplerBindingType, ShaderStages,
    ShaderType, TextureFormat, TextureSampleType, TextureViewDimension, VertexState,
};

/// Asset path for the Kawase downsample WGSL shader, relative to `assets/`.
const DOWNSAMPLE_SHADER: &str = "shaders/backdrop_blur/downsample.wgsl";

/// Asset path for the Kawase upsample WGSL shader, relative to `assets/`.
const UPSAMPLE_SHADER: &str = "shaders/backdrop_blur/upsample.wgsl";

/// Cached render-pipeline state for the dual-Kawase backdrop blur.
///
/// Lives in the [`RenderApp`]; created once via [`FromWorld`] in
/// [`BackdropBlurPlugin::finish`]. The bind-group layout descriptor and queued
/// pipeline IDs are reused every frame — no per-frame GPU allocation.
///
/// The live [`BindGroupLayout`] object is not stored here. Retrieve it at
/// bind-group creation time via:
/// ```ignore
/// let layout = pipeline_cache.get_bind_group_layout(&pipeline.bind_group_layout_descriptor);
/// ```
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

impl FromWorld for BackdropBlurPipeline {
    fn from_world(world: &mut World) -> Self {
        // Build the bind-group layout entries for the three WGSL bindings at
        // @group(0). `BindGroupLayoutEntries::with_indices` lets us specify
        // bindings 0, 1, 2 explicitly. We use raw `BindGroupLayoutEntry`
        // structs to avoid pulling in the `binding_types` helper macros, which
        // are not imported in the existing wc-core convention.
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
        let upsample_shader: Handle<Shader> =
            world.resource::<AssetServer>().load(UPSAMPLE_SHADER);

        // Queue both pipelines. `queue_render_pipeline` returns immediately
        // with a `CachedRenderPipelineId`; the actual GPU compilation is
        // deferred. The node checks `get_render_pipeline(id)` before issuing
        // draw calls.
        //
        // `entry_point` is `Option<Cow<'static, str>>` in Bevy 0.18.
        // `zero_initialize_workgroup_memory: false` — these shaders have no
        // workgroup memory.
        let make_descriptor = |label: &'static str, shader: Handle<Shader>| {
            RenderPipelineDescriptor {
                label: Some(label.into()),
                layout: vec![bind_group_layout_descriptor.clone()],
                push_constant_ranges: vec![],
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
                    targets: vec![Some(ColorTargetState {
                        format: TextureFormat::Rgba8UnormSrgb,
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

        let downsample = world
            .resource_mut::<PipelineCache>()
            .queue_render_pipeline(make_descriptor(
                "backdrop_blur_downsample",
                downsample_shader.clone(),
            ));

        let upsample = world
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
