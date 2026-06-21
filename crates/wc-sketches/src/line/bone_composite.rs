//! Additive bone-glow composite pass for the Line sketch.
//!
//! ## Role
//!
//! The hand-mesh wireframe bones are rendered by [`crate::line::hand_mesh`]'s
//! `HandMeshCamera3d` into an off-screen HDR image (emissive bones on black, no
//! bloom, no tonemapping). [`line_bone_composite`] then **adds** that image into
//! the main camera's HDR view target. It runs in the Core2d schedule's
//! `EarlyPostProcess` set *after* the gravity smear
//! ([`crate::line::post_process`]) and *before* bloom/tonemapping in
//! `PostProcess`.
//!
//! Because the add happens in linear HDR before the main camera's `Bloom` +
//! `AgX` tonemap, the bones are glowed and tonemapped together with the scene —
//! as if they were emissive geometry in it. This sidesteps the transparent-
//! overlay alpha problem entirely (no second camera composites onto the window;
//! see `hand_mesh`'s module docs and bevyengine/bevy#8286): additive
//! compositing never consults an alpha channel, so a black background passes the
//! scene through untouched and emissive texels add their light.
//!
//! ## Wiring
//!
//! [`HandMeshTarget`] holds the off-screen image handle; it is inserted on
//! `OnEnter(AppState::Line)` (by `hand_mesh::spawn_hand_mesh_camera`) and
//! removed on exit. [`ExtractResourcePlugin`] mirrors it into the render world.
//! The node no-ops cleanly whenever the resource (or its GPU image) is absent —
//! so it costs nothing outside Line and during the brief window before the
//! image first uploads.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "u32 ↔ usize casts for GPU draw counts are intentional"
)]

use bevy::core_pipeline::{Core2d, Core2dSystems};
use bevy::prelude::*;
use bevy::render::camera::ExtractedCamera;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{
    binding_types::{sampler, texture_2d},
    BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, CachedRenderPipelineId,
    ColorTargetState, ColorWrites, FragmentState, LoadOp, Operations, PipelineCache,
    PrimitiveState, RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor,
    Sampler, SamplerBindingType, SamplerDescriptor, ShaderStages, StoreOp, TextureFormat,
    TextureSampleType, VertexState,
};
use bevy::render::renderer::{RenderContext, RenderDevice, ViewQuery};
use bevy::render::texture::GpuImage;
use bevy::render::view::ViewTarget;
use bevy::render::RenderApp;
use bevy::shader::Shader;

/// Off-screen render target the hand-mesh bones are rasterized into.
///
/// `Rgba16Float` so the emissive bones (`> 1.0`) survive un-clamped. Created on
/// `OnEnter(AppState::Line)` and removed on exit; [`ExtractResource`] mirrors it
/// into the render world where [`line_bone_composite`] samples it. When absent
/// (every non-Line state) the composite node is a clean no-op.
#[derive(Resource, Clone, ExtractResource)]
pub struct HandMeshTarget {
    /// Handle to the off-screen HDR image. Sized to the window's physical
    /// resolution and resized with the window (see `hand_mesh`).
    pub image: Handle<Image>,
}

/// Plugin that registers the additive bone-glow composite node.
pub struct LineBoneCompositePlugin;

impl Plugin for LineBoneCompositePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ExtractResourcePlugin::<HandMeshTarget>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        // Run in EarlyPostProcess after the gravity smear and before bloom +
        // tonemapping (`PostProcess`), so the main camera's bloom/tonemap process
        // the scene *with the bones added*.
        render_app.add_systems(
            Core2d,
            line_bone_composite
                .in_set(Core2dSystems::EarlyPostProcess)
                .after(super::post_process::line_post_process),
        );
    }

    fn finish(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.init_resource::<BoneCompositePipeline>();
    }
}

/// Cached pipeline state for the composite. Initialised once in
/// [`LineBoneCompositePlugin::finish`].
#[derive(Resource)]
pub struct BoneCompositePipeline {
    /// Bind-group layout descriptor (scene texture, sampler, bone texture).
    pub bind_group_layout_descriptor: BindGroupLayoutDescriptor,
    /// Filtering sampler used to read both textures.
    pub sampler: Sampler,
    /// Handle into Bevy's `PipelineCache` for the composite pipeline.
    pub pipeline_id: CachedRenderPipelineId,
}

impl FromWorld for BoneCompositePipeline {
    fn from_world(world: &mut World) -> Self {
        let render_device = world.resource::<RenderDevice>();

        // binding 0: scene texture, 1: sampler, 2: bone texture.
        let entries = BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
                texture_2d(TextureSampleType::Float { filterable: true }),
            ),
        );
        let bind_group_layout_descriptor =
            BindGroupLayoutDescriptor::new("line_bone_composite_layout", &entries);

        let sampler = render_device.create_sampler(&SamplerDescriptor::default());

        let shader: Handle<Shader> = world
            .resource::<AssetServer>()
            .load("shaders/line/bone_composite.wgsl");

        let pipeline_id =
            world
                .resource_mut::<PipelineCache>()
                .queue_render_pipeline(RenderPipelineDescriptor {
                    label: Some("line_bone_composite_pipeline".into()),
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
                        // Writes into the main camera's HDR view target
                        // (`Rgba16Float`). The shader already sums scene + bones,
                        // so the pipeline blend is a plain replace (`None`).
                        targets: vec![Some(ColorTargetState {
                            format: TextureFormat::Rgba16Float,
                            blend: None,
                            write_mask: ColorWrites::ALL,
                        })],
                    }),
                    primitive: PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: bevy::render::render_resource::MultisampleState::default(),
                    zero_initialize_workgroup_memory: false,
                });

        Self {
            bind_group_layout_descriptor,
            sampler,
            pipeline_id,
        }
    }
}

/// Render system that adds the off-screen bone image into the main view target.
///
/// Runs in [`Core2dSystems::EarlyPostProcess`] after the gravity smear and
/// before bloom. Gates on [`ExtractedCamera::hdr`]: the main Line `Camera2d` is
/// the only Core2d camera and it is HDR, so this matches just it. (The bone
/// `Camera3d` is on the Core3d graph, so it never reaches this system.) As of
/// Bevy 0.19 the `Hdr` marker is no longer extracted to the render world, so we
/// read HDR-ness from the extracted camera; a `&'static Hdr` `ViewQuery` would
/// silently never match.
///
/// [`Core2dSystems::EarlyPostProcess`]: bevy::core_pipeline::Core2dSystems::EarlyPostProcess
pub fn line_bone_composite(
    view: ViewQuery<'_, '_, (&'static ViewTarget, &'static ExtractedCamera)>,
    target: Option<Res<'_, HandMeshTarget>>,
    gpu_images: Res<'_, RenderAssets<GpuImage>>,
    pipeline_res: Option<Res<'_, BoneCompositePipeline>>,
    pipeline_cache: Res<'_, PipelineCache>,
    mut render_context: RenderContext<'_, '_>,
) {
    let (view_target, camera) = view.into_inner();
    // Skip non-HDR Core2d cameras (see the doc note above).
    if !camera.hdr {
        return;
    }

    // No bone target this frame (not in Line, or image not yet uploaded) → clean
    // no-op. Return BEFORE `post_process_write` so the view target is untouched.
    let Some(target) = target else {
        return;
    };
    let Some(bone_image) = gpu_images.get(&target.image) else {
        return;
    };

    let Some(pipeline_res) = pipeline_res else {
        return;
    };
    let Some(pipeline) = pipeline_cache.get_render_pipeline(pipeline_res.pipeline_id) else {
        return;
    };

    let post_process = view_target.post_process_write();
    let layout = pipeline_cache.get_bind_group_layout(&pipeline_res.bind_group_layout_descriptor);

    let bind_group = render_context.render_device().create_bind_group(
        "line_bone_composite_bind_group",
        &layout,
        &BindGroupEntries::sequential((
            post_process.source,
            &pipeline_res.sampler,
            &bone_image.texture_view,
        )),
    );

    let mut pass = render_context
        .command_encoder()
        .begin_render_pass(&RenderPassDescriptor {
            label: Some("line_bone_composite_pass"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: post_process.destination,
                depth_slice: None,
                resolve_target: None,
                // The fullscreen triangle writes every pixel (scene + bones), so
                // the loaded contents are immaterial; `Load` avoids a clear and
                // matches the gravity-smear pass's pattern.
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
