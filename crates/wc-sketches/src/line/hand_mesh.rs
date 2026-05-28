//! Wireframe bone visualization for the Line sketch.
//!
//! ## Role
//!
//! Ports v4's `HandMesh` wireframe rendering onto v5's `TrackedHand` entity
//! model. Each tracked hand entity spawns 20 wireframe icosphere children
//! (one per bone) on [`HAND_MESH_LAYER`]. A dedicated [`HandMeshCamera3d`]
//! renders those bones and composites them on top of the Line scene.
//!
//! ## Compositing: a same-window overlay camera (native multi-camera path)
//!
//! [`HandMeshCamera3d`] targets the **same window** as the Line scene's main
//! HDR `Camera2d`, at a higher [`Camera::order`], and uses
//! [`CameraOutputMode::Write`] with [`BlendState::ALPHA_BLENDING`] to
//! alpha-blend its output **over** the main camera's already-tonemapped frame.
//!
//! This is Bevy's first-class multi-camera-same-window mechanism, not a custom
//! render-graph node or a UI overlay:
//!
//! - Both cameras render into their own intermediate textures but blit to the
//!   **one** window surface (keyed only by render target). The
//!   `UpscalingNode` blit's blend state comes from `output_mode.blend_state`,
//!   so `ALPHA_BLENDING` (`SrcAlpha / OneMinusSrcAlpha`) gives straight-alpha
//!   OVER compositing — correct color (the engine's own blit, no egui gamma
//!   round-trip) and correct edge blending.
//! - `CameraDriverNode` runs camera sub-graphs in ascending `order` within a
//!   single frame's command buffer, so the bones composite over the *current*
//!   frame's scene — no inter-frame lag.
//! - Because the bone camera is a `Camera3d` it renders through the **Core3d**
//!   graph, so the Line gravity post-process (a **Core2d** node) and the main
//!   camera's bloom never touch the bones.
//!
//! ### HDR must match
//!
//! The bone camera carries [`Hdr`] to match the main camera. Mixing HDR and
//! non-HDR cameras on one window is a known-flaky Bevy path (bevyengine/bevy
//! #18901, #17530); matching HDR and setting `output_mode` explicitly (rather
//! than relying on the default auto-blend heuristic) is the supported
//! configuration.
//!
//! ### Why not earlier attempts
//!
//! 1. A `Camera3d` on the swap chain at `order = 1` with
//!    `ClearColorConfig::None` *as the main-pass clear* — bones accumulated
//!    (the camera's own target was never reset between frames). The fix is to
//!    clear the main pass to transparent (`Custom(Color::NONE)`) while leaving
//!    only the *output* write un-clearing (`output_mode.clear_color = None`).
//! 2. A second non-HDR compositor `Camera2d` drawing an off-screen image as a
//!    sprite — never composited, because HDR/non-HDR mismatch on one window is
//!    the open Bevy bug above.
//! 3. An egui overlay painting an off-screen render target — worked, but egui
//!    re-gamma-encodes user textures (wrong bone color), assumes premultiplied
//!    alpha (edge fringing), and gave no same-frame ordering guarantee.
//!
//! ## Data flow
//!
//! 1. `OnEnter(AppState::Line)`: [`spawn_hand_mesh_camera`] spawns the overlay
//!    `Camera3d`.
//! 2. Every `Update` (while Line is active): [`ensure_bone_meshes`] reconciles
//!    bones onto every `TrackedHand` that lacks them — covering hands present
//!    at sketch entry as well as hands that appear later (see that function for
//!    why a reconcile pass replaced the original `Add` observer).
//! 3. Every `Update`: [`update_bone_transforms`] writes the projected
//!    bone-center world coords to each sphere's `Transform`.
//! 4. `OnExit(AppState::Line)`: [`despawn_hand_mesh_camera`] tears down the
//!    overlay camera and [`despawn_all_bone_children`] removes the bones.

use bevy::camera::visibility::RenderLayers;
use bevy::camera::{CameraOutputMode, ScalingMode};
use bevy::pbr::wireframe::{Wireframe, WireframeColor};
use bevy::prelude::*;
use bevy::render::render_resource::BlendState;
use bevy::render::view::Hdr;
use wc_core::input::entity::{BoneCenters, TrackedHand, BONE_COUNT};
use wc_core::input::projection::palm_to_world;
use wc_core::lifecycle::state::AppState;
use wc_core::sketch::sketch_active;

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

/// Marker for the overlay `Camera3d` that rasterizes the wireframe bones and
/// composites them over the Line scene.
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
        app.add_systems(OnEnter(AppState::Line), spawn_hand_mesh_camera)
            .add_systems(
                OnExit(AppState::Line),
                (despawn_hand_mesh_camera, despawn_all_bone_children),
            )
            .add_systems(
                Update,
                (ensure_bone_meshes, update_bone_transforms)
                    .chain()
                    .run_if(sketch_active(AppState::Line)),
            );
    }
}

/// `OnEnter(AppState::Line)` — spawn the overlay `Camera3d` that rasterizes the
/// bones and alpha-composites them over the Line scene.
fn spawn_hand_mesh_camera(mut commands: Commands<'_, '_>) {
    commands.spawn((
        HandMeshCamera3d,
        Camera3d::default(),
        Camera {
            // Higher than the main Camera2d (order 0): `CameraDriverNode` runs
            // sub-graphs in ascending order within one frame's command buffer,
            // so the bones blit *after* — and thus on top of — the scene.
            order: 1,
            // Main-pass clear: reset this camera's own intermediate to fully
            // transparent every frame so last frame's bones are gone before
            // this frame draws. (Using `None` here was the original smear bug.)
            clear_color: ClearColorConfig::Custom(Color::NONE),
            // Output write: blit this camera's result onto the shared window
            // surface with straight-alpha OVER (`ALPHA_BLENDING` =
            // `SrcAlpha / OneMinusSrcAlpha`), and DON'T clear the window —
            // preserving the main camera's tonemapped frame underneath.
            output_mode: CameraOutputMode::Write {
                blend_state: Some(BlendState::ALPHA_BLENDING),
                clear_color: ClearColorConfig::None,
            },
            ..default()
        },
        // Match the main camera's HDR so both share a compatible window-output
        // path (see module docs: HDR-mismatch multi-camera is a Bevy bug).
        Hdr,
        // Orthographic, 1 world unit = 1 logical pixel (matches the main
        // Camera2d and `palm_to_world`'s logical-pixel output), centred at the
        // world origin. `near`/`far` straddle the bone z-plane (z = 0).
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: ScalingMode::WindowSize,
            near: -1000.0,
            far: 1000.0,
            ..OrthographicProjection::default_3d()
        }),
        Transform::from_xyz(0.0, 0.0, 500.0).looking_at(Vec3::ZERO, Vec3::Y),
        HAND_MESH_LAYER,
    ));
}

/// `OnExit(AppState::Line)` — despawn the overlay camera.
fn despawn_hand_mesh_camera(
    mut commands: Commands<'_, '_>,
    cameras: Query<'_, '_, Entity, With<HandMeshCamera3d>>,
) {
    for entity in &cameras {
        commands.entity(entity).despawn();
    }
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
    materials: Option<ResMut<'_, Assets<StandardMaterial>>>,
) {
    // Steady state: every tracked hand already has bones. Bail before touching
    // the asset stores so the common path does no work.
    if new_hands.is_empty() {
        return;
    }
    let (Some(mut meshes), Some(mut materials)) = (meshes, materials) else {
        return;
    };

    let color = hand_mesh_color();

    // Build the shared sphere mesh + bone material once per reconcile call and
    // clone the handles onto every bone of every hand processed this call —
    // avoids one mesh/material allocation per bone.
    //
    // `ico(1)` only fails on subdivisions ≥ 80; using 1 is statically safe.
    #[allow(
        clippy::expect_used,
        reason = "ico(1) only fails if subdivisions >= 80; 1 is statically safe"
    )]
    let sphere_mesh = meshes.add(
        Sphere::new(10.0)
            .mesh()
            .ico(1)
            .expect("ico(1) is well within the 79-subdivision limit"),
    );
    let bone_material = materials.add(StandardMaterial {
        base_color: color,
        unlit: true,
        ..default()
    });

    for hand in &new_hands {
        commands
            .entity(hand)
            .insert(HandMeshBones)
            .with_children(|parent_builder| {
                for i in 0..BONE_COUNT {
                    parent_builder.spawn((
                        Mesh3d(sphere_mesh.clone()),
                        MeshMaterial3d(bone_material.clone()),
                        Wireframe,
                        WireframeColor { color },
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
