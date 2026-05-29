//! Wireframe bone visualization for the Line sketch.
//!
//! ## Role
//!
//! Ports v4's `HandMesh` wireframe rendering onto v5's `TrackedHand` entity
//! model. Each tracked hand entity spawns 20 wireframe icosphere children
//! (one per bone) on [`HAND_MESH_LAYER`]. A dedicated [`HandMeshCamera3d`]
//! renders those bones into an off-screen image that is added into the Line
//! scene just before bloom.
//!
//! ## Compositing: off-screen bone image + additive pre-bloom composite
//!
//! [`HandMeshCamera3d`] is an HDR `Camera3d` that renders the bones into a
//! **private off-screen `Rgba16Float` image** ([`super::bone_composite::HandMeshTarget`])
//! — emissive bones (`> 1.0`, their colour × [`BONE_GLOW_INTENSITY`]) on a black
//! background, with **no bloom and no tonemapping**, so the image holds raw
//! linear-HDR light. [`super::bone_composite::LineBoneCompositeNode`] then
//! **additively adds** that image into the main `Camera2d`'s HDR view target, in
//! the Core2d graph *after* the gravity smear ([`super::post_process`]) and
//! *before* `Node2d::Bloom`.
//!
//! The main camera's `Bloom` + `AgX` then process the scene-with-bones in one
//! pass: the emissive bones bloom into a neon glow and tonemap coherently with
//! the rest of the scene — exactly as if they were emissive geometry in it.
//!
//! ### Why this design (and what it fixes)
//!
//! The bones must composite *after* the gravity smear (a Core2d post-process) so
//! they aren't smeared, but *before* bloom so they glow. The Core2d graph has a
//! slot for exactly that: `… EndMainPass → [bone composite] → Bloom → …`.
//!
//! Compositing **additively in linear HDR before tonemap** dodges every trap of
//! the earlier same-window-overlay design:
//!
//! - **No transparent-overlay alpha.** Additive (`scene.rgb + bones.rgb`) never
//!   consults an alpha channel, so the black bone-image background passes the
//!   scene through untouched and emissive texels add their light. This sidesteps
//!   bevyengine/bevy#8286 (bloom does not preserve a transparent framebuffer's
//!   alpha) — the bug that made the previous overlay overwrite the whole window
//!   with flat gray. (That overlay used a same-window `Camera3d` compositing via
//!   `CameraOutputMode::Write` with `PREMULTIPLIED_ALPHA_BLENDING`; bloom forced
//!   the empty-region alpha to ≈1, so premultiplied-OVER degenerated to *replace*
//!   the window. See the issue cluster #8286 / #18901 / #18902 / #24263 / #14711.)
//! - **No shared intermediate.** The bone camera owns a private off-screen
//!   target, so it can never share — and corrupt — the main camera's HDR
//!   intermediate. That was the failure mode of bevyengine/bevy#17530 ("second
//!   camera's tonemapping/RenderLayers mutes the first") which the old design
//!   worked around with a load-bearing `Msaa::Sample4`-vs-`Msaa::Off` distinction.
//!   MSAA on the bone camera is now a free anti-aliasing choice, not a workaround.
//! - **Coherent tonemapping.** The bones are tonemapped by the *same* `AgX` as
//!   the scene (added pre-tonemap) instead of a separately-tonemapped layer
//!   pasted on top. The cost — bones share the main camera's `Bloom` rather than
//!   a dedicated `TonyMcMapface` + bloom — is a minor, tunable aesthetic
//!   difference (emissive `> 1.0` cores still roll to white under `AgX`; tune
//!   the glow via [`BONE_GLOW_INTENSITY`]).
//!
//! ### Earlier attempts (superseded)
//!
//! 1. **Same-window HDR overlay `Camera3d`** at `order = 1` compositing via
//!    `CameraOutputMode::Write { PREMULTIPLIED_ALPHA_BLENDING }` — overwrote the
//!    window with flat gray (#8286, above), and needed the `Msaa::Sample4` hack
//!    to dodge the #17530 shared-intermediate corruption. Replaced by this
//!    off-screen + additive path, which removes both hazards by construction.
//! 2. A second non-HDR compositor `Camera2d` drawing an off-screen image as a
//!    sprite — never composited (relied on the default auto-blend heuristic).
//! 3. An egui overlay painting an off-screen render target — egui re-gamma-encodes
//!    user textures (wrong bone color), assumes premultiplied alpha (edge
//!    fringing), and gave no same-frame ordering guarantee.
//!
//! ## Data flow
//!
//! 1. `OnEnter(AppState::Line)`: [`spawn_hand_mesh_camera`] creates the
//!    off-screen [`super::bone_composite::HandMeshTarget`] image and spawns the
//!    bone `Camera3d` that renders into it.
//! 2. Every `Update` (while Line is active): [`ensure_bone_meshes`] reconciles
//!    bones onto every `TrackedHand` that lacks them — covering hands present
//!    at sketch entry as well as hands that appear later (see that function for
//!    why a reconcile pass replaced the original `Add` observer).
//! 3. Every `Update`: [`update_bone_transforms`] writes the projected
//!    bone-center world coords to each sphere's `Transform`.
//! 4. Each frame the bone camera (`order = -1`) fills the off-screen image, then
//!    [`super::bone_composite::LineBoneCompositeNode`] adds it into the main
//!    camera's HDR target before bloom.
//! 5. `OnExit(AppState::Line)`: [`despawn_hand_mesh_camera`] tears down the bone
//!    camera and drops the target image; [`despawn_all_bone_children`] removes
//!    the bones.

use bevy::camera::visibility::RenderLayers;
use bevy::camera::{RenderTarget, ScalingMode};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::pbr::MaterialPlugin;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureFormat};
use bevy::render::view::{Hdr, Msaa};
use bevy::window::WindowResized;
use wc_core::input::entity::{BoneCenters, TrackedHand, BONE_COUNT};
use wc_core::input::projection::palm_to_world;
use wc_core::lifecycle::state::AppState;
use wc_core::sketch::sketch_active;

use super::bone_composite::{HandMeshTarget, LineBoneCompositePlugin};
use super::bone_wireframe::{icosphere_line_mesh, BoneWireframeMaterial};

/// Radius of each bone wireframe icosphere, in logical pixels (the overlay
/// camera maps 1 world unit to 1 logical pixel).
const BONE_RADIUS: f32 = 10.0;

/// Emissive multiplier on the bone colour. Pushing the linear `#add6b6` above
/// `1.0` is what makes the bones bloom into a neon glow on the HDR overlay
/// camera (the bright cores roll toward white under tonemapping; the halo keeps
/// the hue). Tunable; `~3–8` is the tasteful range.
const BONE_GLOW_INTENSITY: f32 = 5.0;

/// `RenderLayers` index for the Camera3d wireframe pass.
///
/// Layer 0 is the default layer used by the main `Camera2d` and all 2D
/// content. Layer 1 is reserved exclusively for bone-sphere children +
/// `HandMeshCamera3d`, so the overlay camera never picks up the 2D scene and
/// the main camera never picks up the bones.
pub const HAND_MESH_LAYER_INDEX: usize = 1;

/// The [`RenderLayers`] value for bone spheres and the Camera3d.
pub const HAND_MESH_LAYER: RenderLayers = RenderLayers::layer(HAND_MESH_LAYER_INDEX);

/// Wireframe color matching v4's `HandMesh` `defaultMaterial` (`#add6b6`).
fn hand_mesh_color() -> Color {
    Color::srgb(
        f32::from(0xad_u8) / 255.0,
        f32::from(0xd6_u8) / 255.0,
        f32::from(0xb6_u8) / 255.0,
    )
}

/// Marker for the off-screen `Camera3d` that rasterizes the wireframe bones into
/// the [`super::bone_composite::HandMeshTarget`] image (raw linear HDR, no bloom,
/// no tonemap). The [`super::bone_composite::LineBoneCompositeNode`] adds that
/// image into the main scene before bloom.
#[derive(Component)]
pub struct HandMeshCamera3d;

/// Index of a bone sphere child on a `TrackedHand` entity.
///
/// Value is `0..BONE_COUNT` (20). Set once at spawn; used by
/// [`update_bone_transforms`] to index into `BoneCenters`.
#[derive(Component, Debug, Clone, Copy)]
pub struct BoneIndex(pub usize);

/// Marker placed on a `TrackedHand` once [`ensure_bone_meshes`] has attached
/// its 20 bone children, so the reconcile pass is idempotent (it only spawns
/// bones for hands that lack this marker). Removed on `OnExit(AppState::Line)`
/// alongside the bone children, so re-entering Line re-spawns bones for hands
/// that are still being tracked.
#[derive(Component)]
struct HandMeshBones;

/// Plugin wiring the wireframe bone visualization.
pub struct LineHandMeshPlugin;

impl Plugin for LineHandMeshPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<BoneWireframeMaterial>::default())
            // The additive composite node that adds the off-screen bone image
            // into the main camera's HDR target before bloom.
            .add_plugins(LineBoneCompositePlugin)
            .add_systems(OnEnter(AppState::Line), spawn_hand_mesh_camera)
            .add_systems(
                OnExit(AppState::Line),
                (despawn_hand_mesh_camera, despawn_all_bone_children),
            )
            .add_systems(
                Update,
                (ensure_bone_meshes, update_bone_transforms)
                    .chain()
                    .run_if(sketch_active(AppState::Line)),
            )
            // Keep the off-screen target sized to the window. Not gated on
            // `sketch_active` so a resize while idle is still tracked; it no-ops
            // when the target resource is absent (outside Line).
            .add_systems(Update, resize_bone_target.run_if(in_state(AppState::Line)));
    }
}

/// `OnEnter(AppState::Line)` — create the off-screen bone image and spawn the
/// `Camera3d` that rasterizes the wireframe bones into it.
///
/// The camera renders **raw linear HDR** emissive bones on a black background
/// into an `Rgba16Float` image: no bloom, no tonemapping, `order = -1` (so the
/// image is populated before the main camera's Core2d graph samples it). The
/// glow and tonemap happen later, on the *main* camera, after
/// [`super::bone_composite::LineBoneCompositeNode`] adds this image into the
/// scene (see the module docs). `Msaa::Sample4` anti-aliases the wireframe
/// lines; since this camera owns a private off-screen target it can no longer
/// collide with the main camera's intermediate, so the MSAA value is now a free
/// quality choice rather than the load-bearing workaround it used to be.
fn spawn_hand_mesh_camera(
    mut commands: Commands<'_, '_>,
    window: Single<'_, '_, &Window>,
    mut images: ResMut<'_, Assets<Image>>,
) {
    let target = create_bone_target(&window, &mut images);
    let image = target.image.clone();
    commands.insert_resource(target);
    commands.spawn((
        HandMeshCamera3d,
        Camera3d::default(),
        // HDR so the emissive bones (`> 1.0`) survive un-clamped into the
        // `Rgba16Float` image, with headroom to bloom on the main camera.
        Hdr,
        // Anti-alias the wireframe lines. Harmless now: this camera writes to a
        // private off-screen image, so it can't share (and corrupt) the main
        // camera's intermediate the way the old same-window overlay could.
        Msaa::Sample4,
        // No tonemapping: the image must hold *raw linear* emissive values so the
        // composite can add them to the linear scene before the main camera's
        // AgX rolls the combined frame to display range. (`Tonemapping::None`'s
        // usual caveat — mis-encoding to an SDR swapchain — does not apply here:
        // we render to an HDR image and consume it pre-tonemap.)
        Tonemapping::None,
        Camera {
            // Render before the main `Camera2d` (order 0) so the bone image is
            // populated by the time the composite node samples it this frame.
            order: -1,
            // Black background → contributes nothing under the additive composite.
            clear_color: ClearColorConfig::Custom(Color::BLACK),
            ..default()
        },
        // In Bevy 0.18 the render target is a separate component (not a `Camera`
        // field). Point it at the off-screen bone image.
        RenderTarget::Image(image.into()),
        // Orthographic, 1 world unit = 1 logical pixel (matches the main
        // Camera2d and `palm_to_world`'s logical-pixel output) *regardless* of
        // the image's physical pixel size, centred at the origin. `near`/`far`
        // straddle the bone z-plane (z = 0).
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: ScalingMode::Fixed {
                width: window.width(),
                height: window.height(),
            },
            near: -1000.0,
            far: 1000.0,
            ..OrthographicProjection::default_3d()
        }),
        Transform::from_xyz(0.0, 0.0, 500.0).looking_at(Vec3::ZERO, Vec3::Y),
        HAND_MESH_LAYER,
    ));
}

/// Build a fresh [`HandMeshTarget`] sized to the window's physical resolution.
///
/// `Rgba16Float` so emissive bones keep their `> 1.0` headroom. Physical (not
/// logical) size keeps the wireframes crisp on high-DPI displays; the orthographic
/// `ScalingMode::Fixed` makes the world→pixel mapping independent of the pixel
/// count, so bones stay aligned with the scene at any scale factor.
fn create_bone_target(window: &Window, images: &mut Assets<Image>) -> HandMeshTarget {
    let width = window.physical_width().max(1);
    let height = window.physical_height().max(1);
    let image = Image::new_target_texture(width, height, TextureFormat::Rgba16Float, None);
    HandMeshTarget {
        image: images.add(image),
    }
}

/// Keep the off-screen bone image sized to the window. Resizes the existing
/// image in place (so the camera's `RenderTarget` and the composite binding stay
/// valid) and refreshes the orthographic area to the new logical size. No-ops
/// when no resize event arrived this frame or the target is absent.
fn resize_bone_target(
    mut resized: MessageReader<'_, '_, WindowResized>,
    window: Single<'_, '_, &Window>,
    target: Option<Res<'_, HandMeshTarget>>,
    mut images: ResMut<'_, Assets<Image>>,
    mut projection: Single<'_, '_, &mut Projection, With<HandMeshCamera3d>>,
) {
    if resized.read().count() == 0 {
        return;
    }
    let Some(target) = target else {
        return;
    };
    if let Some(image) = images.get_mut(&target.image) {
        image.resize(Extent3d {
            width: window.physical_width().max(1),
            height: window.physical_height().max(1),
            depth_or_array_layers: 1,
        });
    }
    if let Projection::Orthographic(ortho) = &mut **projection {
        ortho.scaling_mode = ScalingMode::Fixed {
            width: window.width(),
            height: window.height(),
        };
    }
}

/// `OnExit(AppState::Line)` — despawn the bone camera and drop the off-screen
/// target so its `Image` asset (and GPU texture) is freed, per the AGENTS.md
/// rule that per-sketch GPU resources are released on exit.
fn despawn_hand_mesh_camera(
    mut commands: Commands<'_, '_>,
    cameras: Query<'_, '_, Entity, With<HandMeshCamera3d>>,
) {
    for entity in &cameras {
        commands.entity(entity).despawn();
    }
    // Removing the resource drops the last strong `Handle<Image>`, releasing the
    // render target. The composite node then no-ops (its `RenderAssets` lookup
    // returns `None`) until the next `OnEnter` re-creates the target.
    commands.remove_resource::<HandMeshTarget>();
}

/// `OnExit(AppState::Line)` — despawn every bone-sphere child and clear the
/// [`HandMeshBones`] markers from their `TrackedHand` parents.
///
/// Despawning the children directly (rather than relying solely on hierarchy
/// cleanup) covers the case where the hands themselves persist across the
/// sketch exit — the hands stay tracked, but their Line-specific bone visuals
/// must go. Clearing the marker lets [`ensure_bone_meshes`] re-spawn bones for
/// those same hands if Line is re-entered.
fn despawn_all_bone_children(
    mut commands: Commands<'_, '_>,
    bones: Query<'_, '_, Entity, With<BoneIndex>>,
    marked_hands: Query<'_, '_, Entity, With<HandMeshBones>>,
) {
    for entity in &bones {
        commands.entity(entity).despawn();
    }
    for entity in &marked_hands {
        commands.entity(entity).remove::<HandMeshBones>();
    }
}

/// Reconcile pass (runs while Line is the active sketch): give every
/// `TrackedHand` that doesn't yet have bones its 20 wireframe-sphere children,
/// then mark it with [`HandMeshBones`].
///
/// This replaces an earlier `Add<TrackedHand>` observer gated on
/// `AppState::Line`. That observer missed hands that were already being tracked
/// at the moment Line began: hand-tracking runs in `PreUpdate`, which is *before*
/// the `StateTransition` into Line, so such hands were added while the state was
/// still `Home` and never received bones. Reconciling each frame on a
/// `Without<HandMeshBones>` query is timing-independent — it covers hands present
/// at entry, hands that appear mid-sketch, and hands that persist across a
/// leave/re-enter — and is idempotent (it does nothing once every hand is
/// marked, the steady-state case).
///
/// `meshes` / `materials` are `Option`-wrapped so the system is a no-op in
/// headless `MinimalPlugins` test apps where the asset stores aren't registered.
fn ensure_bone_meshes(
    mut commands: Commands<'_, '_>,
    new_hands: Query<'_, '_, Entity, (With<TrackedHand>, Without<HandMeshBones>)>,
    meshes: Option<ResMut<'_, Assets<Mesh>>>,
    materials: Option<ResMut<'_, Assets<BoneWireframeMaterial>>>,
) {
    // Steady state: every tracked hand already has bones. Bail before touching
    // the asset stores so the common path does no work.
    if new_hands.is_empty() {
        return;
    }
    let (Some(mut meshes), Some(mut materials)) = (meshes, materials) else {
        return;
    };

    // Build the shared LineList wireframe mesh + bone material once per
    // reconcile call and clone the handles onto every bone of every hand
    // processed this call — avoids one mesh/material allocation per bone. The
    // LineList mesh renders as a true wireframe on Metal (no
    // `POLYGON_MODE_LINE`); see `super::bone_wireframe`.
    let line_mesh = meshes.add(icosphere_line_mesh(BONE_RADIUS));
    // Emissive bone colour: scale the linear `#add6b6` above 1.0 so the bones
    // are over-bright in the off-screen image and the main camera's bloom turns
    // them into a glow after the additive composite (see the module docs). Alpha
    // is irrelevant — the composite is additive and never reads it.
    let base = hand_mesh_color().to_linear();
    let bone_material = materials.add(BoneWireframeMaterial {
        color: LinearRgba::rgb(
            base.red * BONE_GLOW_INTENSITY,
            base.green * BONE_GLOW_INTENSITY,
            base.blue * BONE_GLOW_INTENSITY,
        ),
    });

    for hand in &new_hands {
        commands
            .entity(hand)
            .insert(HandMeshBones)
            .with_children(|parent_builder| {
                for i in 0..BONE_COUNT {
                    parent_builder.spawn((
                        Mesh3d(line_mesh.clone()),
                        MeshMaterial3d(bone_material.clone()),
                        HAND_MESH_LAYER,
                        BoneIndex(i),
                        Transform::default(),
                    ));
                }
            });
    }
}

/// Per-frame: project each hand's bone centres to world space and write the
/// projected position to each child sphere's `Transform.translation`.
fn update_bone_transforms(
    hands: Query<'_, '_, (&BoneCenters, &Children), With<TrackedHand>>,
    mut bones: Query<'_, '_, (&BoneIndex, &mut Transform), Without<TrackedHand>>,
    window: Single<'_, '_, &Window>,
) {
    let window_size = Vec2::new(window.width(), window.height());

    for (bone_centers, children) in &hands {
        for child in children.iter() {
            let Ok((bone_index, mut transform)) = bones.get_mut(child) else {
                continue;
            };
            let idx = bone_index.0;
            if idx >= BONE_COUNT {
                continue;
            }
            let center_mm = bone_centers.0[idx];
            let projected = palm_to_world(center_mm, window_size);
            transform.translation = Vec3::new(projected.x, projected.y, 0.0);
        }
    }
}
