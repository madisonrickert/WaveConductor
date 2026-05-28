//! Wireframe bone visualization for the Line sketch.
//!
//! ## Role
//!
//! Ports v4's `HandMesh` wireframe rendering onto v5's `TrackedHand` entity
//! model. Each tracked hand entity spawns 20 wireframe icosphere children
//! (one per bone) on [`HAND_MESH_LAYER`]. A dedicated [`HandMeshCamera3d`]
//! renders those bones and composites them on top of the Line scene.
//!
//! ## Compositing: a glowing HDR overlay camera (native multi-camera path)
//!
//! [`HandMeshCamera3d`] targets the **same window** as the Line scene's main
//! HDR `Camera2d`, at a higher [`Camera::order`]. It is itself an **HDR** camera
//! with its own [`Tonemapping`] + [`Bloom`], so the bones — which emit values
//! `> 1.0` (their colour × [`BONE_GLOW_INTENSITY`]) — bloom into a neon glow:
//! the bright wireframe cores roll toward white under the tone curve while the
//! halo keeps the `#add6b6` hue. It composites over the main camera's
//! already-tonemapped frame via [`CameraOutputMode::Write`].
//!
//! This is Bevy's first-class multi-camera-same-window mechanism (not a custom
//! render-graph node or a UI overlay). `CameraDriverNode` runs camera sub-graphs
//! in ascending `order` within one frame's command buffer, so the bones
//! composite over the *current* frame — no inter-frame lag. The bone camera is
//! a `Camera3d` (Core3d graph), so the Line gravity post-process (a Core2d node)
//! never touches the bones.
//!
//! Three subtleties make this correct — each hard-won:
//!
//! ### 1. `Msaa::Off` — the load-bearing scene-preservation fix
//!
//! Bevy caches each camera's intermediate ping-pong textures (and the atomic
//! that swaps them) keyed by `(render target, usage, hdr, msaa)`. Two HDR
//! cameras on the **same window with the same MSAA** therefore *share* one
//! intermediate + swap atomic — and the overlay's tonemapping `post_process`
//! swap then corrupts the main camera's frame (this was the "dim parts of the
//! gravity scene disappear / tonemapping thrown off" bug). Giving the overlay
//! `Msaa::Off` changes its cache key so it gets its **own** intermediate,
//! isolated from the main camera. (Lines don't benefit from MSAA coverage
//! anyway, so this is free.)
//!
//! ### 2. `PREMULTIPLIED_ALPHA_BLENDING` — so the glow halo survives compositing
//!
//! Bevy's bloom leaves the alpha channel untouched (its upsample blends alpha
//! `Zero / One`) and tonemapping passes alpha through. The overlay clears to
//! `(0,0,0,0)`, so after bloom the glow *halo* has bright RGB but alpha ≈ 0,
//! while bone *cores* are opaque (alpha 1). Straight `ALPHA_BLENDING`
//! (`SrcAlpha / …`) would multiply the halo by `≈0` and **drop the glow**.
//! `PREMULTIPLIED_ALPHA_BLENDING` (`One / OneMinusSrcAlpha`) instead *adds* the
//! halo over the scene (`src.rgb·1 + dst·(1−0)`) while opaque cores replace it
//! (`src + dst·0`) — exactly neon-over-scene, in one blend.
//!
//! ### 3. Explicit `Tonemapping` + per-overlay `Bloom`
//!
//! `Camera3d` requires a `Tonemapping` component; we set it explicitly
//! (`TonyMcMapface`, which desaturates highlights to a clean white core — the
//! tonemapper Bevy's bloom docs recommend) rather than relying on the default.
//! `Bloom` is per-camera; the main camera's doesn't reach this one, so the
//! overlay carries its own. (`Tonemapping::None` is NOT an option — its node
//! early-returns, leaving raw linear values mis-encoded to the SDR swapchain.)
//!
//! ### Why not earlier attempts
//!
//! 1. A `Camera3d` on the swap chain at `order = 1` with
//!    `ClearColorConfig::None` *as the main-pass clear* — bones accumulated
//!    (the camera's own target was never reset between frames). Fixed by
//!    clearing the main pass to transparent (`Custom(Color::NONE)`) while
//!    leaving only the *output* write un-clearing (`output_mode.clear_color`).
//! 2. A second non-HDR compositor `Camera2d` drawing an off-screen image as a
//!    sprite — never composited (relied on the default auto-blend heuristic;
//!    the explicit `output_mode` is the fix).
//! 3. An egui overlay painting an off-screen render target — worked, but egui
//!    re-gamma-encodes user textures (wrong bone color), assumes premultiplied
//!    alpha (edge fringing), and gave no same-frame ordering guarantee.
//! 4. An HDR overlay with default MSAA (matching the main camera) — shared the
//!    main camera's intermediate texture and corrupted the scene (see §1); and a
//!    *non-HDR* overlay rendered the bones correctly but clamped to `[0,1]`,
//!    foreclosing the HDR glow. The current design (HDR + `Msaa::Off`) keeps the
//!    glow headroom without the corruption.
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
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::pbr::MaterialPlugin;
use bevy::post_process::bloom::{Bloom, BloomPrefilter};
use bevy::prelude::*;
use bevy::render::render_resource::BlendState;
use bevy::render::view::{Hdr, Msaa};
use wc_core::input::entity::{BoneCenters, TrackedHand, BONE_COUNT};
use wc_core::input::projection::palm_to_world;
use wc_core::lifecycle::state::AppState;
use wc_core::sketch::sketch_active;

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
        app.add_plugins(MaterialPlugin::<BoneWireframeMaterial>::default())
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
            );
    }
}

/// `OnEnter(AppState::Line)` — spawn the HDR overlay `Camera3d` that rasterizes
/// the bones, blooms them into a glow, and composites them over the Line scene.
///
/// See the module docs for the three load-bearing details: `Msaa::Off` (so the
/// overlay's intermediate texture doesn't collide with the main HDR camera's and
/// corrupt the scene), `PREMULTIPLIED_ALPHA_BLENDING` (so the bloom glow halo —
/// bright RGB, ~0 alpha — composites additively while opaque bone cores
/// replace), and the explicit `Tonemapping` + per-overlay `Bloom` that produce
/// the neon glow.
fn spawn_hand_mesh_camera(mut commands: Commands<'_, '_>) {
    commands.spawn((
        HandMeshCamera3d,
        Camera3d::default(),
        // HDR intermediate: gives the emissive bones (`> 1.0`) headroom to bloom
        // instead of clamping at `1.0`.
        Hdr,
        // Critical: a distinct MSAA setting from the main HDR camera so this
        // overlay gets its OWN intermediate ping-pong textures. Sharing them
        // (same `(target, usage, hdr, msaa)` key) let the overlay's tonemapping
        // swap corrupt the main scene — the "dim parts disappear" bug. Lines
        // gain nothing from MSAA, so disabling it is free.
        Msaa::Off,
        // Explicit (not the required-component default). TonyMcMapface
        // desaturates the bright bone cores to a clean white, leaving the halo
        // hued — the neon look. Independent of the main camera's AgX.
        Tonemapping::TonyMcMapface,
        // Per-camera bloom: the main camera's bloom doesn't reach this one.
        // `threshold: 0.0` blooms everything (the bones are the only content);
        // EnergyConserving (from `Bloom::NATURAL`) pairs with a zero threshold.
        Bloom {
            intensity: 0.25,
            low_frequency_boost: 0.7,
            prefilter: BloomPrefilter {
                threshold: 0.0,
                threshold_softness: 0.0,
            },
            ..Bloom::NATURAL
        },
        Camera {
            // Higher than the main Camera2d (order 0): `CameraDriverNode` runs
            // sub-graphs in ascending order within one frame's command buffer,
            // so the bones blit *after* — on top of — the scene.
            order: 1,
            // Main-pass clear: reset this camera's own intermediate to fully
            // transparent every frame so last frame's bones are gone before
            // this frame draws, and so the cleared backdrop is premultiplied-
            // valid `(0,0,0,0)`. (Using `None` here was the original smear.)
            clear_color: ClearColorConfig::Custom(Color::NONE),
            // Output write: blit onto the shared window surface with
            // premultiplied-alpha OVER (`One / OneMinusSrcAlpha`) so the bloom
            // glow halo (bright RGB, alpha ~0) adds over the scene while opaque
            // bone cores (alpha 1) replace it. DON'T clear the window —
            // preserving the main camera's tonemapped frame underneath.
            output_mode: CameraOutputMode::Write {
                blend_state: Some(BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                clear_color: ClearColorConfig::None,
            },
            ..default()
        },
        // Orthographic, 1 world unit = 1 logical pixel (matches the main
        // Camera2d and `palm_to_world`'s logical-pixel output), centred at
        // the world origin. `near`/`far` straddle the bone z-plane (z = 0).
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
    // Emissive bone colour: scale the linear `#add6b6` above 1.0 so the HDR
    // overlay camera blooms it into a glow. Alpha stays 1.0 so bone cores are
    // opaque for the premultiplied-alpha composite (the glow halo carries its
    // own near-zero alpha; see the module docs).
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
