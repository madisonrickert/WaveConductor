//! Additive bone-glow composite pass for the Dots (Fabric) sketch.
//!
//! ## Role
//!
//! The hand-mesh wireframe bones are rendered by [`crate::dots::hand_mesh`]'s
//! `DotsHandMeshCamera3d` into an off-screen HDR image (emissive bones on black,
//! no bloom, no tonemapping). [`dots_bone_composite`] then **adds** that image
//! into the main camera's HDR view target. It runs in the Core2d schedule's
//! `EarlyPostProcess` set *after* the explode post-process
//! ([`crate::dots::post_process`]) and *before* bloom/tonemapping in `PostProcess`.
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
//! [`DotsHandMeshTarget`] holds the off-screen image handle; it is inserted on
//! `OnEnter(AppState::Dots)` (by `hand_mesh::spawn_hand_mesh_camera`) and
//! removed on exit. [`ExtractResourcePlugin`] mirrors it into the render world.
//! The node no-ops cleanly whenever the resource (or its GPU image) is absent —
//! so it costs nothing outside Dots and during the brief window before the
//! image first uploads.
//!
//! ## Render-world removal (D3 lesson)
//!
//! Bevy 0.19's `ExtractResourcePlugin` propagates inserts and updates but **not**
//! removals. After `OnExit(AppState::Dots)` removes `DotsHandMeshTarget` from
//! the main world, the render-world copy would linger, keeping the composite
//! running on other sketches with a stale bone image. The
//! `remove_dots_hand_mesh_target_if_absent` system (registered in
//! [`ExtractSchedule`]) issues the explicit `remove_resource` when the
//! main-world source is absent. Mirrors the `remove_dots_post_params_if_absent`
//! pattern in [`crate::dots::post_process`].
//!
//! **Known limitation (carry-forward #75):** bones bypass the main camera's
//! bloom rolloff — they are added into the scene before bloom runs, so
//! over-bright bone texels bloom via the main camera's bloom settings rather
//! than a dedicated per-bone bloom stage.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "u32 ↔ usize casts for GPU draw counts are intentional"
)]

use bevy::core_pipeline::{Core2d, Core2dSystems};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::camera::ExtractedCamera;
use bevy::render::extract_resource::ExtractResourcePlugin;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{
    binding_types::{sampler, texture_2d},
    BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries,
    CachedRenderPipelineId, ColorTargetState, ColorWrites, FragmentState, LoadOp, Operations,
    PipelineCache, PrimitiveState, RenderPassColorAttachment, RenderPassDescriptor,
    RenderPipelineDescriptor, Sampler, SamplerBindingType, SamplerDescriptor, ShaderStages,
    StoreOp, TextureFormat, TextureSampleType, TextureViewId, VertexState,
};
use bevy::render::renderer::{RenderContext, RenderDevice, ViewQuery};
use bevy::render::texture::GpuImage;
use bevy::render::view::ViewTarget;
use bevy::render::{Extract, ExtractSchedule, RenderApp};
use bevy::shader::Shader;

use super::hand_mesh::{DotsBoneActive, DotsHandMeshTarget};

/// Plugin that registers the additive bone-glow composite node for Dots.
pub struct DotsBoneCompositePlugin;

impl Plugin for DotsBoneCompositePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ExtractResourcePlugin::<DotsHandMeshTarget>::default());
        // Extract the hand-presence flag alongside the target so the composite
        // node can read it in the render world without an extra cross-world query.
        app.add_plugins(ExtractResourcePlugin::<DotsBoneActive>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        // Run in EarlyPostProcess after the explode post-process and before
        // bloom + tonemapping (`PostProcess`), so the main camera's bloom/tonemap
        // process the scene *with the bones added*.
        render_app.add_systems(
            Core2d,
            dots_bone_composite
                .in_set(Core2dSystems::EarlyPostProcess)
                .after(super::post_process::dots_post_process),
        );

        // Explicitly remove the render-world copy when the main-world resource
        // is gone. `ExtractResourcePlugin` propagates inserts and updates but
        // NOT removals (verified against bevy_render 0.19 extract_resource.rs —
        // the None branch is a complete no-op). Without this, `DotsHandMeshTarget`
        // lingers in the render world after `OnExit(AppState::Dots)`, keeping
        // the composite running on other sketches with the last-frame bone image.
        // The render-world `Handle<Image>` clone also keeps the GPU texture alive,
        // so `gpu_images.get` returns `Some` and the composite does NOT self-guard
        // via the RenderAssets lookup alone (the D3 bug). See the module docs.
        render_app.add_systems(ExtractSchedule, remove_dots_hand_mesh_target_if_absent);
        // Mirror the same D3 removal pattern for the presence flag.
        render_app.add_systems(ExtractSchedule, remove_dots_bone_active_if_absent);
    }

    fn finish(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.init_resource::<DotsBoneCompositePipeline>();
    }
}

/// Cached pipeline state for the Dots bone composite. Initialised once in
/// [`DotsBoneCompositePlugin::finish`].
#[derive(Resource)]
pub struct DotsBoneCompositePipeline {
    /// Bind-group layout descriptor (scene texture, sampler, bone texture).
    pub bind_group_layout_descriptor: BindGroupLayoutDescriptor,
    /// Filtering sampler used to read both textures.
    pub sampler: Sampler,
    /// Handle into Bevy's `PipelineCache` for the composite pipeline.
    pub pipeline_id: CachedRenderPipelineId,
}

impl FromWorld for DotsBoneCompositePipeline {
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
            BindGroupLayoutDescriptor::new("dots_bone_composite_layout", &entries);

        let sampler = render_device.create_sampler(&SamplerDescriptor::default());

        let shader: Handle<Shader> = world
            .resource::<AssetServer>()
            .load("shaders/dots/bone_composite.wgsl");

        let pipeline_id =
            world
                .resource_mut::<PipelineCache>()
                .queue_render_pipeline(RenderPipelineDescriptor {
                    label: Some("dots_bone_composite_pipeline".into()),
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

/// Render system that adds the off-screen bone image into the main Dots view
/// target.
///
/// Runs in [`Core2dSystems::EarlyPostProcess`] after the explode post-process
/// and before bloom. Gates on [`ExtractedCamera::hdr`]: the main Dots `Camera2d`
/// is the only Core2d camera and it is HDR, so this matches just it. (The bone
/// `DotsHandMeshCamera3d` is on the Core3d graph and never reaches this system.)
/// As of Bevy 0.19 the `Hdr` marker is no longer extracted to the render world,
/// so we read HDR-ness from the extracted camera; a `&'static Hdr` `ViewQuery`
/// would silently never match.
///
/// No-ops cleanly when [`DotsHandMeshTarget`] is absent (any non-Dots state, or
/// the brief window before the image first uploads) **or** when [`DotsBoneActive`]
/// is `false` (no [`wc_core::input::entity::TrackedHand`] entities this frame).
/// Both early-returns fire BEFORE [`ViewTarget::post_process_write`] so the
/// ping-pong is not flipped — the explode output flows to bloom unchanged.
///
/// The render-world removal systems ensure both resources are absent after
/// `OnExit(AppState::Dots)` — see the module docs for why the `RenderAssets`
/// lookup alone does not suffice.
///
/// [`Core2dSystems::EarlyPostProcess`]: bevy::core_pipeline::Core2dSystems::EarlyPostProcess
pub fn dots_bone_composite(
    view: ViewQuery<'_, '_, (&'static ViewTarget, &'static ExtractedCamera)>,
    target: Option<Res<'_, DotsHandMeshTarget>>,
    bone_active: Option<Res<'_, DotsBoneActive>>,
    gpu_images: Res<'_, RenderAssets<GpuImage>>,
    pipeline_res: Option<Res<'_, DotsBoneCompositePipeline>>,
    pipeline_cache: Res<'_, PipelineCache>,
    mut bind_group_cache: Local<'_, (Option<TextureViewId>, HashMap<TextureViewId, BindGroup>)>,
    mut render_context: RenderContext<'_, '_>,
) {
    let (view_target, camera) = view.into_inner();
    // Skip non-HDR Core2d cameras (see the doc note above).
    if !camera.hdr {
        return;
    }

    // No bone target this frame (not in Dots, or image not yet uploaded) →
    // clean no-op. Return BEFORE `post_process_write` so the view target is
    // untouched and the explode output flows to bloom unchanged.
    let Some(target) = target else {
        return;
    };

    // No hands tracked this frame → skip the composite BEFORE `post_process_write`
    // so the ping-pong is not flipped. This prevents the stale (last-rendered)
    // bone image from ghosting onto the scene when no hand is present. The bone
    // Camera3d is also inactive at this point (see `update_dots_bone_activity`),
    // so the bone image itself has not been refreshed this frame.
    if !bone_active.is_some_and(|a| a.0) {
        return;
    }

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

    // Reuse the bind group for this (source view, bone image) combination.
    // `post_process_write` cycles `source` between two stable views; the bone
    // image is recreated per Dots entry, so when its view id changes we clear the
    // per-source entries — dropping the bind groups that referenced the old (now
    // freed) bone HDR target. Without that eviction the cache would retain a stale
    // bone image across every re-entry, a soak-stability leak. Steady state holds
    // two entries (one per source view) for the current bone image.
    let bone_id = bone_image.texture_view.id();
    if bind_group_cache.0 != Some(bone_id) {
        bind_group_cache.1.clear();
        bind_group_cache.0 = Some(bone_id);
    }
    let bind_group = bind_group_cache
        .1
        .entry(post_process.source.id())
        .or_insert_with(|| {
            render_context.render_device().create_bind_group(
                "dots_bone_composite_bind_group",
                &layout,
                &BindGroupEntries::sequential((
                    post_process.source,
                    &pipeline_res.sampler,
                    &bone_image.texture_view,
                )),
            )
        });

    let mut pass = render_context
        .command_encoder()
        .begin_render_pass(&RenderPassDescriptor {
            label: Some("dots_bone_composite_pass"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: post_process.destination,
                depth_slice: None,
                resolve_target: None,
                // The fullscreen triangle writes every pixel (scene + bones), so
                // the loaded contents are immaterial; `Load` avoids a clear and
                // matches the explode post-process pass's pattern.
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

/// Removes the render-world [`DotsHandMeshTarget`] when the main world no
/// longer has it — i.e. after `OnExit(AppState::Dots)` fires.
///
/// [`ExtractResourcePlugin`] propagates inserts and updates each
/// [`ExtractSchedule`] tick but does **not** issue `remove_resource` when the
/// main-world source is absent (verified against `bevy_render` 0.19
/// `extract_resource.rs`: the `None` arm is a no-op). Without this explicit
/// removal the stale render-world copy keeps [`dots_bone_composite`] running
/// Dots' composite pass on other sketches with a stale bone image — and the
/// render-world `Handle<Image>` clone keeps the GPU texture alive, so the
/// `RenderAssets<GpuImage>` lookup would not self-guard (the D3 bug).
///
/// Mirrors [`crate::dots::post_process`]'s `remove_dots_post_params_if_absent`.
fn remove_dots_hand_mesh_target_if_absent(
    mut commands: Commands<'_, '_>,
    main_resource: Extract<'_, '_, Option<Res<'_, DotsHandMeshTarget>>>,
    render_resource: Option<Res<'_, DotsHandMeshTarget>>,
) {
    if main_resource.is_none() && render_resource.is_some() {
        commands.remove_resource::<DotsHandMeshTarget>();
    }
}

/// Removes the render-world [`DotsBoneActive`] when the main world no longer
/// has it — i.e. after `OnExit(AppState::Dots)` removes it from the main world.
///
/// Mirrors [`remove_dots_hand_mesh_target_if_absent`] exactly: the D3 removal
/// pattern applied to the hand-presence flag so the composite guard resets
/// cleanly when Dots is not active.
fn remove_dots_bone_active_if_absent(
    mut commands: Commands<'_, '_>,
    main_resource: Extract<'_, '_, Option<Res<'_, DotsBoneActive>>>,
    render_resource: Option<Res<'_, DotsBoneActive>>,
) {
    if main_resource.is_none() && render_resource.is_some() {
        commands.remove_resource::<DotsBoneActive>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build-smoke test: `DotsBoneCompositePlugin` adds cleanly under
    /// `MinimalPlugins` (no `RenderApp` present) without panicking.
    ///
    /// The plugin's `build` and `finish` both early-return when
    /// `get_sub_app_mut(RenderApp)` returns `None`, so registering it outside
    /// a full render context must be a no-op — not a panic.
    ///
    /// Mirrors the `particle_compute_plugin_builds` smoke test pattern in
    /// `tests/particles_foundation.rs`.
    #[test]
    fn dots_bone_composite_plugin_builds() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(DotsBoneCompositePlugin);
        app.update();
    }
}
