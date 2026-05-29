//! Additive bone-glow composite pass for the Line sketch.
//!
//! ## Role
//!
//! The hand-mesh wireframe bones are rendered by [`crate::line::hand_mesh`]'s
//! `HandMeshCamera3d` into an off-screen HDR image (emissive bones on black, no
//! bloom, no tonemapping). [`LineBoneCompositeNode`] then **adds** that image
//! into the main camera's HDR view target, inserted into the Core2d graph
//! *after* the gravity smear ([`crate::line::post_process`]) and *before*
//! [`Node2d::Bloom`].
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

use bevy::core_pipeline::core_2d::graph::{Core2d, Node2d};
use bevy::ecs::query::QueryItem;
use bevy::prelude::*;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_graph::{
    NodeRunError, RenderGraphContext, RenderGraphExt, RenderLabel, ViewNode, ViewNodeRunner,
};
use bevy::render::render_resource::{
    binding_types::{sampler, texture_2d},
    BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, CachedRenderPipelineId,
    ColorTargetState, ColorWrites, FragmentState, LoadOp, Operations, PipelineCache,
    PrimitiveState, RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor,
    Sampler, SamplerBindingType, SamplerDescriptor, ShaderStages, StoreOp, TextureFormat,
    TextureSampleType, VertexState,
};
use bevy::render::renderer::{RenderContext, RenderDevice};
use bevy::render::texture::GpuImage;
use bevy::render::view::{Hdr, ViewTarget};
use bevy::render::RenderApp;
use bevy::shader::Shader;

/// Off-screen render target the hand-mesh bones are rasterized into.
///
/// `Rgba16Float` so the emissive bones (`> 1.0`) survive un-clamped. Created on
/// `OnEnter(AppState::Line)` and removed on exit; [`ExtractResource`] mirrors it
/// into the render world where [`LineBoneCompositeNode`] samples it. When absent
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

        render_app
            .add_render_graph_node::<ViewNodeRunner<LineBoneCompositeNode>>(
                Core2d,
                LineBoneCompositeLabel,
            )
            // After the scene's main pass (which includes the gravity-smear node,
            // edged before `EndMainPass`) and before `Bloom`, so the main camera's
            // bloom + tonemap process the scene *with the bones added*.
            .add_render_graph_edges(
                Core2d,
                (
                    Node2d::EndMainPass,
                    LineBoneCompositeLabel,
                    Node2d::Bloom,
                ),
            );
    }

    fn finish(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.init_resource::<BoneCompositePipeline>();
    }
}

/// Render-graph label for the additive bone-glow composite node.
#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
pub struct LineBoneCompositeLabel;

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

/// Render-graph node that adds the off-screen bone image into the main view
/// target. Runs after the gravity smear and before `Bloom`.
#[derive(Default)]
pub struct LineBoneCompositeNode;

impl ViewNode for LineBoneCompositeNode {
    // Gate on `Hdr`: the main Line `Camera2d` is the only Core2d camera and it is
    // HDR, so this matches just it. (The bone `Camera3d` is on the Core3d graph,
    // so it never reaches this node.)
    type ViewQuery = (&'static ViewTarget, &'static Hdr);

    fn run<'w>(
        &self,
        _graph: &mut RenderGraphContext<'_>,
        render_context: &mut RenderContext<'w>,
        view_target: QueryItem<'w, '_, Self::ViewQuery>,
        world: &'w World,
    ) -> Result<(), NodeRunError> {
        let (view_target, _hdr) = view_target;

        // No bone target this frame (not in Line, or image not yet uploaded) →
        // clean no-op. Return BEFORE `post_process_write` so the view target is
        // left untouched.
        let Some(target) = world.get_resource::<HandMeshTarget>() else {
            return Ok(());
        };
        let gpu_images = world.resource::<RenderAssets<GpuImage>>();
        let Some(bone_image) = gpu_images.get(&target.image) else {
            return Ok(());
        };

        let pipeline_cache = world.resource::<PipelineCache>();
        let Some(pipeline_res) = world.get_resource::<BoneCompositePipeline>() else {
            return Ok(());
        };
        let Some(pipeline) = pipeline_cache.get_render_pipeline(pipeline_res.pipeline_id) else {
            return Ok(());
        };

        let post_process = view_target.post_process_write();
        let layout =
            pipeline_cache.get_bind_group_layout(&pipeline_res.bind_group_layout_descriptor);

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
                    // The fullscreen triangle writes every pixel (scene + bones),
                    // so the loaded contents are immaterial; `Load` avoids a
                    // clear and matches the gravity-smear node's pattern.
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
