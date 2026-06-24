//! Additive bone-glow composite pass for the shared hand-mesh overlay.
//!
//! ## Role
//!
//! The hand-mesh wireframe bones are rendered by [`super`]'s
//! `HandMeshCamera3d` into an off-screen HDR image (emissive bones on black,
//! no bloom, no tonemapping). [`hand_mesh_composite`] then **adds** that image
//! into the main camera's HDR view target. It runs in the Core2d schedule's
//! `EarlyPostProcess` set in [`HandMeshCompositeSet`]; each sketch orders this
//! set after its own post-process node via `configure_sets`.
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
//! `OnEnter` by [`super::spawn_hand_mesh_camera`] and removed on exit.
//! [`ExtractResourcePlugin`] mirrors it into the render world. The node no-ops
//! cleanly whenever the resource (or its GPU image) is absent — so it costs
//! nothing outside the active sketch and during the brief window before the
//! image first uploads.
//!
//! ## Render-world removal (D3 lesson)
//!
//! Bevy 0.19's `ExtractResourcePlugin` propagates inserts and updates but **not**
//! removals. After `OnExit` removes [`HandMeshTarget`] from the main world, the
//! render-world copy would linger, keeping the composite running on other sketches
//! with a stale bone image. The [`remove_hand_mesh_target_if_absent`] system
//! (registered in [`ExtractSchedule`]) issues the explicit `remove_resource` when
//! the main-world source is absent. The same D3 pattern is applied to
//! [`HandPresence`] via [`remove_hand_presence_if_absent`].
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
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
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

/// Off-screen render target the hand-mesh bones are rasterized into.
///
/// `Rgba16Float` so emissive bones (`> 1.0`) survive un-clamped. Inserted on
/// `OnEnter` by `super::spawn_hand_mesh_camera`, removed on exit;
/// [`ExtractResource`] mirrors it into the render world. When absent (every
/// non-overlay state) the composite node is a clean no-op.
#[derive(Resource, Clone, ExtractResource)]
pub struct HandMeshTarget {
    /// Handle to the off-screen HDR image, sized to the window's physical resolution.
    pub image: Handle<Image>,
}

/// Hand-presence gate for the bone camera and composite.
///
/// Set each frame by `super::update_hand_presence`: `true` when ≥1
/// [`wc_core::input::entity::TrackedHand`] exists. Extracted to the render world
/// so [`hand_mesh_composite`] can early-return before flipping the post-process
/// ping-pong when no hand is tracked — preventing stale-bone ghosting.
#[derive(Resource, Clone, Copy, ExtractResource)]
pub struct HandPresence(pub bool);

/// Render-system set the shared composite runs in. Each sketch orders this set
/// after its own post-process node via `configure_sets` (see `line`/`dots` mod).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct HandMeshCompositeSet;

/// Plugin that registers the shared additive bone-glow composite node.
///
/// Register this once globally (via `SketchesPlugin`). Each sketch's per-state
/// wiring is handled by [`super::HandMeshPlugin`].
pub struct HandMeshCompositePlugin;

impl Plugin for HandMeshCompositePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ExtractResourcePlugin::<HandMeshTarget>::default());
        app.add_plugins(ExtractResourcePlugin::<HandPresence>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        // EarlyPostProcess; each sketch orders this set after its own
        // post-process via `configure_sets(Core2d, HandMeshCompositeSet.after(..))`.
        render_app.add_systems(
            Core2d,
            hand_mesh_composite
                .in_set(Core2dSystems::EarlyPostProcess)
                .in_set(HandMeshCompositeSet),
        );
        render_app.add_systems(
            ExtractSchedule,
            (remove_hand_mesh_target_if_absent, remove_hand_presence_if_absent),
        );
    }

    fn finish(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.init_resource::<HandMeshCompositePipeline>();
    }
}

/// Cached pipeline state for the shared hand-mesh composite. Initialised once
/// in [`HandMeshCompositePlugin::finish`].
#[derive(Resource)]
pub struct HandMeshCompositePipeline {
    /// Bind-group layout descriptor (scene texture, sampler, bone texture).
    pub(crate) bind_group_layout_descriptor: BindGroupLayoutDescriptor,
    /// Filtering sampler used to read both textures.
    pub(crate) sampler: Sampler,
    /// Handle into Bevy's `PipelineCache` for the composite pipeline.
    pub(crate) pipeline_id: CachedRenderPipelineId,
}

impl FromWorld for HandMeshCompositePipeline {
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
            BindGroupLayoutDescriptor::new("hand_mesh_composite_layout", &entries);

        let sampler = render_device.create_sampler(&SamplerDescriptor::default());

        let shader: Handle<Shader> = world
            .resource::<AssetServer>()
            .load("shaders/hand_mesh/bone_composite.wgsl");

        let pipeline_id =
            world
                .resource_mut::<PipelineCache>()
                .queue_render_pipeline(RenderPipelineDescriptor {
                    label: Some("hand_mesh_composite_pipeline".into()),
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
/// Runs in [`Core2dSystems::EarlyPostProcess`] in [`HandMeshCompositeSet`].
/// Gates on [`ExtractedCamera::hdr`]: the sketch's main `Camera2d` is the only
/// Core2d camera and it is HDR, so this matches just it. (The bone
/// `HandMeshCamera3d` is on the Core3d graph and never reaches this system.)
/// As of Bevy 0.19 the `Hdr` marker is no longer extracted to the render world,
/// so we read HDR-ness from the extracted camera; a `&'static Hdr` `ViewQuery`
/// would silently never match.
///
/// No-ops cleanly when [`HandMeshTarget`] is absent (any non-active state, or
/// the brief window before the image first uploads) **or** when [`HandPresence`]
/// is `false` (no [`wc_core::input::entity::TrackedHand`] entities this frame).
/// Both early-returns fire BEFORE [`ViewTarget::post_process_write`] so the
/// ping-pong is not flipped — preventing stale-bone ghosting.
///
/// The render-world removal systems ensure both resources are absent after
/// `OnExit` — see the module docs for why the `RenderAssets` lookup alone does
/// not suffice (the D3 bug).
///
/// [`Core2dSystems::EarlyPostProcess`]: bevy::core_pipeline::Core2dSystems::EarlyPostProcess
pub fn hand_mesh_composite(
    view: ViewQuery<'_, '_, (&'static ViewTarget, &'static ExtractedCamera)>,
    target: Option<Res<'_, HandMeshTarget>>,
    presence: Option<Res<'_, HandPresence>>,
    gpu_images: Res<'_, RenderAssets<GpuImage>>,
    pipeline_res: Option<Res<'_, HandMeshCompositePipeline>>,
    pipeline_cache: Res<'_, PipelineCache>,
    mut bind_group_cache: Local<'_, (Option<TextureViewId>, HashMap<TextureViewId, BindGroup>)>,
    mut render_context: RenderContext<'_, '_>,
) {
    let (view_target, camera) = view.into_inner();
    // Skip non-HDR Core2d cameras (see the doc note above).
    if !camera.hdr {
        return;
    }

    let Some(target) = target else { return; };
    // No hands tracked → skip BEFORE post_process_write so the ping-pong is not
    // flipped (prevents stale-bone ghosting). Unconditional for every consumer.
    if !presence.is_some_and(|p| p.0) {
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
    // image is recreated per sketch entry, so when its view id changes we clear
    // the per-source entries — dropping the bind groups that referenced the old
    // (now freed) bone HDR target. Without that eviction the cache would retain a
    // stale bone image across every re-entry, a soak-stability leak. Steady state
    // holds two entries (one per source view) for the current bone image.
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
                "hand_mesh_composite_bind_group",
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
            label: Some("hand_mesh_composite_pass"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: post_process.destination,
                depth_slice: None,
                resolve_target: None,
                // The fullscreen triangle writes every pixel (scene + bones), so
                // the loaded contents are immaterial; `Load` avoids a clear and
                // matches the sketch post-process pass's pattern.
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

/// Removes the render-world [`HandMeshTarget`] when the main world no longer
/// has it — i.e. after `OnExit` fires.
///
/// [`ExtractResourcePlugin`] propagates inserts and updates each
/// [`ExtractSchedule`] tick but does **not** issue `remove_resource` when the
/// main-world source is absent (verified against `bevy_render` 0.19
/// `extract_resource.rs`: the `None` arm is a no-op). Without this explicit
/// removal the stale render-world copy keeps [`hand_mesh_composite`] running on
/// other sketches with a stale bone image — and the render-world `Handle<Image>`
/// clone keeps the GPU texture alive, so the `RenderAssets<GpuImage>` lookup
/// would not self-guard (the D3 bug).
fn remove_hand_mesh_target_if_absent(
    mut commands: Commands<'_, '_>,
    main_resource: Extract<'_, '_, Option<Res<'_, HandMeshTarget>>>,
    render_resource: Option<Res<'_, HandMeshTarget>>,
) {
    if main_resource.is_none() && render_resource.is_some() {
        commands.remove_resource::<HandMeshTarget>();
    }
}

/// Removes the render-world [`HandPresence`] when the main world no longer has
/// it — i.e. after `OnExit` removes it from the main world.
///
/// Mirrors [`remove_hand_mesh_target_if_absent`] exactly: the D3 removal
/// pattern applied to the hand-presence flag so the composite guard resets
/// cleanly when the sketch is not active.
fn remove_hand_presence_if_absent(
    mut commands: Commands<'_, '_>,
    main_resource: Extract<'_, '_, Option<Res<'_, HandPresence>>>,
    render_resource: Option<Res<'_, HandPresence>>,
) {
    if main_resource.is_none() && render_resource.is_some() {
        commands.remove_resource::<HandPresence>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build-smoke test: `HandMeshCompositePlugin` adds cleanly under
    /// `MinimalPlugins` (no `RenderApp` present) without panicking.
    ///
    /// The plugin's `build` and `finish` both early-return when
    /// `get_sub_app_mut(RenderApp)` returns `None`, so registering it outside
    /// a full render context must be a no-op — not a panic.
    ///
    /// Mirrors the `particle_compute_plugin_builds` smoke test pattern in
    /// `tests/particles_foundation.rs`.
    #[test]
    fn hand_mesh_composite_plugin_builds() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(HandMeshCompositePlugin);
        app.update();
    }
}
