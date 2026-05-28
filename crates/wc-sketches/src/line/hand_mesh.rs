//! Wireframe bone visualization for the Line sketch.
//!
//! ## Role
//!
//! Ports v4's `HandMesh` wireframe rendering onto v5's `TrackedHand` entity
//! model. Each tracked hand entity spawns 20 wireframe icosphere children
//! (one per bone), rendered by a dedicated [`HandMeshCamera3d`] on
//! [`HAND_MESH_LAYER`] at `order = 1` with [`ClearColorConfig::None`] on top of
//! Camera2d's output.
//!
//! ## Data flow
//!
//! 1. `OnEnter(AppState::Line)`: [`spawn_hand_mesh_camera`] creates the
//!    `Camera3d` with an orthographic projection matching the window's logical
//!    size, `order = 1`, and `ClearColorConfig::None` so it composites over the
//!    2D layer without clearing.
//! 2. `On<Add, TrackedHand>` observer: [`spawn_bones_on_tracked_hand_added`]
//!    calls [`spawn_bones`] to attach 20 wireframe-sphere `Mesh3d` children to
//!    the `TrackedHand` entity. Only fires while `AppState::Line` is active.
//! 3. Every `Update` frame (while Line is active): [`update_bone_transforms`]
//!    reads [`wc_core::input::entity::BoneCenters`] from each `TrackedHand` and
//!    projects each center through [`wc_core::input::projection::palm_to_world`]
//!    to align with the attractor coordinate convention, writing the result to
//!    the corresponding bone sphere's `Transform`.
//! 4. `OnExit(AppState::Line)`: [`despawn_hand_mesh_camera`] drops the Camera3d.
//!    Bone entities are children of `TrackedHand`; they despawn automatically
//!    with their parent when `sync_hand_entities` removes the `TrackedHand`
//!    entity. [`despawn_all_bone_children`] catches any orphaned bones if the
//!    sketch exits before all hands depart.
//!
//! ## Scope limit (Phase 13)
//!
//! The `HandMeshCamera3d` does NOT carry `Hdr`, `Bloom`, or `Tonemapping`.
//! Wireframes render at full forward-3D brightness but are not bloomed; they
//! composite over Camera2d's already-bloomed + AgX-tonemapped output. Bloom
//! parity with v4 requires a shared `Image` render target + final-blit camera,
//! which is deferred to a follow-up plan.

use bevy::camera::visibility::RenderLayers;
use bevy::camera::ScalingMode;
use bevy::pbr::wireframe::{Wireframe, WireframeColor};
use bevy::prelude::*;
use wc_core::input::entity::{BoneCenters, TrackedHand, BONE_COUNT};
use wc_core::input::projection::palm_to_world;
use wc_core::lifecycle::state::AppState;
use wc_core::sketch::sketch_active;

/// `RenderLayers` index used for the 3D wireframe pass.
///
/// Layer 0 is the default layer used by Camera2d and all 2D content. Layer 1
/// is reserved exclusively for bone mesh spheres + `HandMeshCamera3d` so the
/// two passes stay independent and `ClearColorConfig::None` composites cleanly.
pub const HAND_MESH_LAYER_INDEX: usize = 1;

/// The [`RenderLayers`] value for bone spheres and the `HandMeshCamera3d`.
///
/// `RenderLayers::layer` is const so this is a zero-cost constant.
pub const HAND_MESH_LAYER: RenderLayers = RenderLayers::layer(HAND_MESH_LAYER_INDEX);

/// Wireframe color matching v4's `HandMesh` `defaultMaterial` (`#add6b6`,
/// a muted green). Precomputed as `f32` fractions to avoid `as` casts (which
/// the `as_conversions` lint would flag).
///
/// `Color::srgb` is not a `const fn` in Bevy 0.18, so this is a regular
/// function; call sites cache the result or accept the trivial construction cost.
fn hand_mesh_color() -> Color {
    // #add6b6 = (0xad / 255, 0xd6 / 255, 0xb6 / 255)
    // Precomputed: 0xad = 173 → 173/255 ≈ 0.6784; 0xd6 = 214 → 214/255 ≈ 0.8392;
    //              0xb6 = 182 → 182/255 ≈ 0.7137.
    Color::srgb(
        f32::from(0xad_u8) / 255.0,
        f32::from(0xd6_u8) / 255.0,
        f32::from(0xb6_u8) / 255.0,
    )
}

/// Marker component for the Camera3d entity that renders wireframe bones.
///
/// Spawned in `OnEnter(AppState::Line)`, despawned in `OnExit`.
#[derive(Component)]
pub struct HandMeshCamera3d;

/// Index of a bone sphere child on a `TrackedHand` entity.
///
/// Value is `0..BONE_COUNT` (20). Set once at spawn; used by
/// [`update_bone_transforms`] to index into `BoneCenters`.
#[derive(Component, Debug, Clone, Copy)]
pub struct BoneIndex(pub usize);

/// Plugin registering the wireframe bone visualization.
///
/// Appended to [`super::LinePlugin`] via `app.add_plugins(LineHandMeshPlugin)`.
pub struct LineHandMeshPlugin;

impl Plugin for LineHandMeshPlugin {
    /// Register Camera3d lifecycle, bone spawn observer, and per-frame transform
    /// update.
    ///
    /// Camera is spawned in `OnEnter(Line)` and despawned in `OnExit(Line)`.
    /// Bones are children of each `TrackedHand`; they despawn with the parent
    /// automatically. `despawn_all_bone_children` is an `OnExit` guard for
    /// bones whose parent hands hadn't despawned by the time the sketch exits.
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::Line), spawn_hand_mesh_camera)
            .add_systems(
                OnExit(AppState::Line),
                (despawn_hand_mesh_camera, despawn_all_bone_children),
            )
            .add_observer(spawn_bones_on_tracked_hand_added)
            .add_systems(
                Update,
                update_bone_transforms.run_if(sketch_active(AppState::Line)),
            );
    }
}

/// `OnEnter(AppState::Line)` — spawn the orthographic Camera3d used for
/// wireframe bone rendering.
///
/// The camera uses `ScalingMode::WindowSize` (the `default_3d` default) so
/// world units match logical pixels, consistent with Camera2d's coordinate
/// space. Positioned at `z = 500`, looking toward the origin, so that bone
/// spheres placed at `z = 0` land in view.
///
/// `order = 1` means this camera's pass executes after Camera2d's pass (order
/// 0). `ClearColorConfig::None` skips the clear so the 3D output composites
/// over the already-rendered 2D layer instead of painting over it.
fn spawn_hand_mesh_camera(mut commands: Commands<'_, '_>) {
    commands.spawn((
        HandMeshCamera3d,
        Camera3d::default(),
        Camera {
            order: 1,
            clear_color: ClearColorConfig::None,
            ..default()
        },
        Projection::Orthographic(OrthographicProjection {
            // ScalingMode::WindowSize is `default_3d()`'s default: 1 world
            // unit = 1 logical pixel. Matches Camera2d's coordinate space.
            scaling_mode: ScalingMode::WindowSize,
            // Give the frustum plenty of depth so all bone z-values (0.0) are
            // well within view from the camera at z = 500.
            near: -1000.0,
            far: 1000.0,
            ..OrthographicProjection::default_3d()
        }),
        Transform::from_xyz(0.0, 0.0, 500.0).looking_at(Vec3::ZERO, Vec3::Y),
        HAND_MESH_LAYER,
    ));
}

/// `OnExit(AppState::Line)` — despawn the `HandMeshCamera3d`.
fn despawn_hand_mesh_camera(
    mut commands: Commands<'_, '_>,
    query: Query<'_, '_, Entity, With<HandMeshCamera3d>>,
) {
    for entity in &query {
        commands.entity(entity).despawn();
    }
}

/// `OnExit(AppState::Line)` — despawn any `BoneIndex` entities that were not
/// already despawned by their `TrackedHand` parent leaving the tracking volume.
///
/// This is a safety guard: under normal operation, bones despawn with their
/// `TrackedHand` parent. But if the sketch exits while hands are still tracked,
/// `BoneIndex` children might outlive `OnExit` if the parent's despawn hasn't
/// cascaded yet. Querying directly for `BoneIndex` entities ensures a clean slate.
fn despawn_all_bone_children(
    mut commands: Commands<'_, '_>,
    query: Query<'_, '_, Entity, With<BoneIndex>>,
) {
    for entity in &query {
        commands.entity(entity).despawn();
    }
}

/// Observer — reacts to `Add<TrackedHand>` and spawns wireframe bone spheres as
/// children of the new `TrackedHand` entity, but only while `AppState::Line` is
/// active.
///
/// Using an observer rather than a system means the bones appear on the same
/// frame the hand entity is spawned, with no one-frame gap.
///
/// `meshes` and `materials` are `Option`-wrapped because headless test apps
/// (using `MinimalPlugins` without `PbrPlugin`) do not register these asset
/// stores. When running without a real `RenderApp` the observer skips spawning
/// rather than panicking — the bones are purely visual and their absence does
/// not affect hand-tracking logic.
fn spawn_bones_on_tracked_hand_added(
    trigger: On<'_, '_, Add, TrackedHand>,
    state: Res<'_, State<AppState>>,
    commands: Commands<'_, '_>,
    meshes: Option<ResMut<'_, Assets<Mesh>>>,
    materials: Option<ResMut<'_, Assets<StandardMaterial>>>,
) {
    if *state.get() != AppState::Line {
        return;
    }
    let (Some(meshes), Some(materials)) = (meshes, materials) else {
        // Headless test context: asset stores not present; skip visual spawn.
        return;
    };
    spawn_bones(trigger.event_target(), commands, meshes, materials);
}

/// Spawn 20 wireframe icosphere children on `parent` (a `TrackedHand` entity).
///
/// Each child carries:
/// - [`Mesh3d`] + [`MeshMaterial3d<StandardMaterial>`]: the icosphere geometry
///   with `unlit = true` so the wireframe color is not shaded by lights.
/// - [`Wireframe`] + [`WireframeColor`]: opt-in wireframe rendering. The
///   [`Wireframe`] component enables wireframe mode for this specific entity
///   regardless of the global [`bevy::pbr::wireframe::WireframeConfig`].
/// - [`HAND_MESH_LAYER`]: restricts rendering to the `HandMeshCamera3d`.
/// - [`BoneIndex`]: records which bone center in `BoneCenters` this sphere
///   represents, used by [`update_bone_transforms`].
/// - [`Transform::default()`]: positioned at the origin until the first
///   `update_bone_transforms` run; this only lasts one frame.
fn spawn_bones(
    parent: Entity,
    mut commands: Commands<'_, '_>,
    mut meshes: ResMut<'_, Assets<Mesh>>,
    mut materials: ResMut<'_, Assets<StandardMaterial>>,
) {
    let color = hand_mesh_color();

    // ico(1) on a small sphere: 1 subdivision keeps the geometry light (80
    // triangles) while still reading as a sphere at bone-visualization scale.
    // `ico` returns `Err` only for subdivisions >= 80; using 1 is provably safe.
    // The allow is intentional and narrowly scoped — this exact call site is
    // the only place in the module where `ico` is called.
    #[allow(
        clippy::expect_used,
        reason = "ico(1) only fails if subdivisions >= 80; using 1 is statically safe"
    )]
    let sphere_mesh = meshes.add(
        Sphere::new(10.0)
            .mesh()
            .ico(1)
            .expect("ico(1) is well within the 79-subdivision limit; unreachable"),
    );

    let bone_material = materials.add(StandardMaterial {
        base_color: color,
        // Unlit so wireframe color is not dimmed by the absence of a 3D light
        // source in the scene. Bones should appear at their nominal color.
        unlit: true,
        ..default()
    });

    commands.entity(parent).with_children(|parent_builder| {
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

/// Per-frame: project each hand's bone centers to world space and write to the
/// corresponding bone sphere's `Transform`.
///
/// Queries `TrackedHand` entities that have both `BoneCenters` (filled by the
/// provider) and `Children` (the 20 bone spheres). For each child that carries
/// a `BoneIndex`, looks up the bone center, projects through `palm_to_world`,
/// and sets the sphere's translation.
///
/// `BoneIndex.0` is always in `0..BONE_COUNT` by construction (20 spheres
/// spawned with indices 0..20); the `continue` is a defensive guard against any
/// edge case where the index is somehow out of range.
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
                // Defensive: should never happen since spawn_bones only creates
                // indices 0..BONE_COUNT, but guard rather than panic.
                continue;
            }
            let center_mm = bone_centers.0[idx];
            let projected = palm_to_world(center_mm, window_size);
            transform.translation = Vec3::new(projected.x, projected.y, 0.0);
        }
    }
}
