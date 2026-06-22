//! Wireframe bone visualization for the Dots sketch.
//!
//! ## Role
//!
//! Ports v4's `HandMesh` wireframe rendering onto v5's `TrackedHand` entity
//! model for the Dots (Fabric) sketch. Each tracked hand entity spawns 20
//! wireframe icosphere children (one per bone) on [`HAND_MESH_LAYER`]. A
//! dedicated [`DotsHandMeshCamera3d`] renders those bones into an off-screen
//! HDR image ([`DotsHandMeshTarget`]) that the additive composite pass (Task
//! D6b-2, `bone_composite`) will blend into the Dots scene before bloom.
//!
//! ## Architecture
//!
//! Mirrors [`crate::line::hand_mesh`] exactly; only the sketch state
//! (`AppState::Dots`) and material type ([`super::bone_wireframe::DotsBoneWireframeMaterial`])
//! differ. See that module for the full compositing rationale, the bone-camera
//! design notes (off-screen HDR, additive composite, bevyengine/bevy#8286 /
//! #17530), and the list of superseded approaches.
//!
//! [`HAND_MESH_LAYER`] and [`BoneIndex`] are re-exported from
//! [`crate::line::hand_mesh`] (both `pub`). Dots' `Camera3d` and Line's are
//! mutually exclusive (only one sketch is active at a time), so sharing the
//! layer index is safe — no cross-sketch bone rendering can occur.
//!
//! ## Data flow
//!
//! 1. `OnEnter(AppState::Dots)`: [`spawn_hand_mesh_camera`] creates the
//!    off-screen [`DotsHandMeshTarget`] image and spawns the bone `Camera3d`
//!    that renders into it.
//! 2. Every `Update` (while Dots is active): [`ensure_bone_meshes`] reconciles
//!    bones onto every `TrackedHand` that lacks them — covering hands present
//!    at sketch entry as well as hands that appear later (see that function for
//!    why a reconcile pass replaced the original `Add` observer).
//! 3. Every `Update`: [`update_bone_transforms`] writes the projected
//!    bone-center world coords to each sphere's `Transform`.
//! 4. Each frame the bone camera (`order = -1`) fills the off-screen image.
//!    Task D6b-2 adds the composite node that adds this image into the Dots
//!    scene before bloom.
//! 5. `OnExit(AppState::Dots)`: [`despawn_hand_mesh_camera`] tears down the
//!    bone camera and drops the target image; [`despawn_all_bone_children`]
//!    removes the bones.

use bevy::camera::{Hdr, RenderTarget, ScalingMode};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::pbr::MaterialPlugin;
use bevy::prelude::*;
use bevy::render::extract_resource::ExtractResource;
use bevy::render::render_resource::{Extent3d, TextureFormat};
use bevy::render::view::Msaa;
use bevy::window::WindowResized;
use wc_core::input::entity::{BoneCenters, TrackedHand, BONE_COUNT};
use wc_core::input::projection::palm_to_world;
use wc_core::lifecycle::state::AppState;
use wc_core::sketch::sketch_active;

use super::bone_wireframe::{icosphere_line_mesh, DotsBoneWireframeMaterial};

// Reuse Line's layer constants — Dots and Line are mutually exclusive at
// runtime, so their Camera3d instances never coexist on this layer.
pub use crate::line::hand_mesh::BoneIndex;
pub use crate::line::hand_mesh::{HAND_MESH_LAYER, HAND_MESH_LAYER_INDEX};

/// Radius of each bone wireframe icosphere, in logical pixels (the overlay
/// camera maps 1 world unit to 1 logical pixel). Mirrors Line's value.
const BONE_RADIUS: f32 = 10.0;

/// Emissive multiplier on the bone colour. Pushing the linear base hue above
/// `1.0` is what makes the bones bloom into a neon glow on the HDR overlay
/// camera. Tunable; `~3–8` is the tasteful range. Mirrors Line's value.
const BONE_GLOW_INTENSITY: f32 = 5.0;

/// Wireframe color for Dots bones: a cool ice-blue that reads on the dark
/// "Fabric" particle field without competing with the warm dot haze.
///
/// Operator-tunable; this is the starting point for hardware calibration.
fn hand_mesh_color() -> Color {
    // Ice blue: sRGB `#b0d8ff` — cool, neutral, reads clearly on dark fabric.
    Color::srgb(
        f32::from(0xb0_u8) / 255.0,
        f32::from(0xd8_u8) / 255.0,
        f32::from(0xff_u8) / 255.0,
    )
}

/// Off-screen render target the Dots hand-mesh bones are rasterized into.
///
/// `Rgba16Float` so emissive bones (`> 1.0`) survive un-clamped. Created on
/// `OnEnter(AppState::Dots)` and removed on exit. The additive composite pass
/// (Task D6b-2) reads this image and adds it into the Dots scene before bloom.
///
/// [`ExtractResource`] mirrors this into the render world each frame so the
/// composite pass in [`super::bone_composite`] can sample the GPU image. The
/// render-world copy is explicitly removed by a `remove_dots_hand_mesh_target_if_absent`
/// system in that plugin (see the D3 lesson there) so the composite no-ops after
/// `OnExit(AppState::Dots)`.
#[derive(Resource, Clone, ExtractResource)]
pub struct DotsHandMeshTarget {
    /// Handle to the off-screen HDR image. Sized to the window's physical
    /// resolution and resized with the window (see [`resize_bone_target`]).
    pub image: Handle<Image>,
}

/// Marker for the off-screen `Camera3d` that rasterizes the Dots wireframe
/// bones into the [`DotsHandMeshTarget`] image (raw linear HDR, no bloom,
/// no tonemap). The composite pass (Task D6b-2) adds that image into the Dots
/// scene before bloom.
#[derive(Component)]
pub struct DotsHandMeshCamera3d;

/// Marker placed on a `TrackedHand` once [`ensure_bone_meshes`] has attached
/// its 20 bone children, so the reconcile pass is idempotent. Removed on
/// `OnExit(AppState::Dots)` alongside the bone children, so re-entering Dots
/// re-spawns bones for hands that are still being tracked.
#[derive(Component)]
struct DotsHandMeshBones;

/// Plugin wiring the Dots wireframe bone visualization.
pub struct DotsHandMeshPlugin;

impl Plugin for DotsHandMeshPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<DotsBoneWireframeMaterial>::default());

        // In debug builds, `WC_DEBUG_DISABLE_BONE_CAMERA` skips spawning the
        // off-screen bone camera for render-stage isolation. Always spawns in
        // release (no `DebugToggles`).
        #[cfg(debug_assertions)]
        let spawn_camera = !app
            .world()
            .get_resource::<wc_core::debug::DebugToggles>()
            .is_some_and(|t| t.disable_bone_camera);
        #[cfg(not(debug_assertions))]
        let spawn_camera = true;
        if spawn_camera {
            app.add_systems(OnEnter(AppState::Dots), spawn_hand_mesh_camera);
        }

        app.add_systems(
            OnExit(AppState::Dots),
            (despawn_hand_mesh_camera, despawn_all_bone_children),
        )
        .add_systems(
            Update,
            (ensure_bone_meshes, update_bone_transforms)
                .chain()
                .run_if(sketch_active(AppState::Dots)),
        )
        // Keep the off-screen target sized to the window. Not gated on
        // `sketch_active` so a resize while idle is still tracked; it no-ops
        // when the target resource is absent (outside Dots).
        .add_systems(Update, resize_bone_target.run_if(in_state(AppState::Dots)));
    }
}

/// `OnEnter(AppState::Dots)` — create the off-screen bone image and spawn the
/// `Camera3d` that rasterizes the Dots wireframe bones into it.
///
/// The camera renders **raw linear HDR** emissive bones on a black background
/// into an `Rgba16Float` image: no bloom, no tonemapping, `order = -1` (so the
/// image is populated before the main camera's Core2d graph samples it this
/// frame). The glow and tonemap happen later on the main camera, after Task
/// D6b-2's composite node adds this image additively into the Dots scene
/// (before bloom). `Msaa::Sample4` anti-aliases the wireframe lines; since
/// this camera owns a private off-screen target it cannot collide with the
/// main camera's intermediate, so MSAA is a free quality choice.
fn spawn_hand_mesh_camera(
    mut commands: Commands<'_, '_>,
    window: Single<'_, '_, &Window>,
    mut images: ResMut<'_, Assets<Image>>,
) {
    let target = create_bone_target(&window, &mut images);
    let image = target.image.clone();
    commands.insert_resource(target);
    commands.spawn((
        DotsHandMeshCamera3d,
        Camera3d::default(),
        // HDR so the emissive bones (`> 1.0`) survive un-clamped into the
        // `Rgba16Float` image, with headroom to bloom on the main camera.
        Hdr,
        // Anti-alias the wireframe lines. Harmless: this camera writes to a
        // private off-screen image, so it can't share (and corrupt) the main
        // camera's intermediate the way an old same-window overlay could.
        Msaa::Sample4,
        // No tonemapping: the image must hold *raw linear* emissive values so
        // the Task D6b-2 composite can add them to the linear Dots scene
        // before the main camera's tonemap rolls the combined frame to display
        // range. (`Tonemapping::None`'s usual caveat — mis-encoding to an SDR
        // swapchain — does not apply: we render to an HDR image consumed
        // pre-tonemap.)
        Tonemapping::None,
        Camera {
            // Render before the main `Camera2d` (order 0) so the bone image is
            // populated by the time the composite node samples it this frame.
            order: -1,
            // Black background → contributes nothing under the additive
            // composite. Mirrors Line's choice.
            clear_color: ClearColorConfig::Custom(Color::BLACK),
            ..default()
        },
        // In Bevy 0.19 the render target is a separate component (not a
        // `Camera` field). Point it at the off-screen bone image.
        RenderTarget::Image(image.into()),
        // Orthographic, 1 world unit = 1 logical pixel (matches the main
        // Camera2d and `palm_to_world`'s logical-pixel output) regardless of
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

/// Build a fresh [`DotsHandMeshTarget`] sized to the window's physical
/// resolution.
///
/// `Rgba16Float` so emissive bones keep their `> 1.0` headroom. Physical (not
/// logical) size keeps the wireframes crisp on high-DPI displays; the
/// orthographic `ScalingMode::Fixed` makes the world→pixel mapping independent
/// of the pixel count, so bones stay aligned with the Dots scene at any scale.
fn create_bone_target(window: &Window, images: &mut Assets<Image>) -> DotsHandMeshTarget {
    let width = window.physical_width().max(1);
    let height = window.physical_height().max(1);
    let image = Image::new_target_texture(width, height, TextureFormat::Rgba16Float, None);
    DotsHandMeshTarget {
        image: images.add(image),
    }
}

/// Keep the off-screen bone image sized to the window. Resizes the existing
/// image in place (so the camera's `RenderTarget` and the Task D6b-2 composite
/// binding stay valid) and refreshes the orthographic area to the new logical
/// size. No-ops when no resize event arrived this frame or the target is absent.
fn resize_bone_target(
    mut resized: MessageReader<'_, '_, WindowResized>,
    window: Single<'_, '_, &Window>,
    target: Option<Res<'_, DotsHandMeshTarget>>,
    mut images: ResMut<'_, Assets<Image>>,
    mut projection: Single<'_, '_, &mut Projection, With<DotsHandMeshCamera3d>>,
) {
    if resized.read().count() == 0 {
        return;
    }
    let Some(target) = target else {
        return;
    };
    if let Some(mut image) = images.get_mut(&target.image) {
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

/// `OnExit(AppState::Dots)` — despawn the bone camera and drop the off-screen
/// target so its `Image` asset (and GPU texture) is freed, per the AGENTS.md
/// rule that per-sketch GPU resources are released on exit.
fn despawn_hand_mesh_camera(
    mut commands: Commands<'_, '_>,
    cameras: Query<'_, '_, Entity, With<DotsHandMeshCamera3d>>,
) {
    for entity in &cameras {
        commands.entity(entity).despawn();
    }
    // Removing the resource drops the last strong `Handle<Image>`, releasing
    // the render target. The Task D6b-2 composite node then no-ops (its
    // `RenderAssets` lookup returns `None`) until the next `OnEnter`
    // re-creates the target.
    commands.remove_resource::<DotsHandMeshTarget>();
}

/// `OnExit(AppState::Dots)` — despawn every bone-sphere child and clear the
/// [`DotsHandMeshBones`] markers from their `TrackedHand` parents.
///
/// Despawning the children directly (rather than relying solely on hierarchy
/// cleanup) covers the case where the hands themselves persist across the
/// sketch exit — the hands stay tracked, but their Dots-specific bone visuals
/// must go. Clearing the marker lets [`ensure_bone_meshes`] re-spawn bones for
/// those same hands if Dots is re-entered.
fn despawn_all_bone_children(
    mut commands: Commands<'_, '_>,
    bones: Query<'_, '_, Entity, With<BoneIndex>>,
    marked_hands: Query<'_, '_, Entity, With<DotsHandMeshBones>>,
) {
    for entity in &bones {
        commands.entity(entity).despawn();
    }
    for entity in &marked_hands {
        commands.entity(entity).remove::<DotsHandMeshBones>();
    }
}

/// Reconcile pass (runs while Dots is the active sketch): give every
/// `TrackedHand` that doesn't yet have bones its 20 wireframe-sphere children,
/// then mark it with [`DotsHandMeshBones`].
///
/// Mirrors [`crate::line::hand_mesh`]'s `ensure_bone_meshes` exactly (same
/// reconcile-vs-observer rationale: hands present at sketch entry arrive via
/// `PreUpdate` before `StateTransition`, so an `Add<TrackedHand>` observer
/// would miss them). This reconcile pass is timing-independent and idempotent.
///
/// `meshes` / `materials` are `Option`-wrapped so the system is a no-op in
/// headless `MinimalPlugins` test apps where the asset stores aren't registered.
fn ensure_bone_meshes(
    mut commands: Commands<'_, '_>,
    new_hands: Query<'_, '_, Entity, (With<TrackedHand>, Without<DotsHandMeshBones>)>,
    meshes: Option<ResMut<'_, Assets<Mesh>>>,
    materials: Option<ResMut<'_, Assets<DotsBoneWireframeMaterial>>>,
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
    // Emissive bone colour: scale the ice-blue base above 1.0 so the bones
    // are over-bright in the off-screen image and the main camera's bloom (via
    // the Task D6b-2 additive composite) turns them into a glow. Alpha is
    // irrelevant — the composite is additive and never reads it.
    let base = hand_mesh_color().to_linear();
    let bone_material = materials.add(DotsBoneWireframeMaterial {
        color: LinearRgba::rgb(
            base.red * BONE_GLOW_INTENSITY,
            base.green * BONE_GLOW_INTENSITY,
            base.blue * BONE_GLOW_INTENSITY,
        ),
    });

    for hand in &new_hands {
        commands
            .entity(hand)
            .insert(DotsHandMeshBones)
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

/// Per-frame: project each Dots hand's bone centres to world space and write
/// the projected position to each child sphere's `Transform.translation`.
///
/// Mirrors [`crate::line::hand_mesh`]'s `update_bone_transforms` exactly —
/// same `palm_to_world` projection, same orthographic-pixel mapping.
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

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions — panicking on unexpected None is the correct behaviour"
)]
mod tests {
    use super::*;
    use bevy::asset::AssetPlugin;
    use bevy::ecs::system::RunSystemOnce;

    /// Verifies that [`ensure_bone_meshes`] spawns exactly [`BONE_COUNT`] (20)
    /// [`BoneIndex`] children on a freshly-spawned `TrackedHand` when the
    /// required asset stores are present.
    ///
    /// Uses `App` + `AssetPlugin` + `MeshPlugin` + `init_asset` to register
    /// the asset stores (without them `ensure_bone_meshes` no-ops cleanly per
    /// its `Option`-wrapped params — tested separately would be trivially
    /// true). Mirrors the reconcile-vs-observer design note in the module docs:
    /// the system is timing-independent, so calling it once via `run_system_once`
    /// is equivalent to the first Update tick in Dots.
    #[test]
    fn ensure_bone_meshes_spawns_20_bone_children() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(AssetPlugin::default());
        app.add_plugins(bevy::mesh::MeshPlugin);
        // Register `Assets<DotsBoneWireframeMaterial>` so `ensure_bone_meshes`
        // can call `materials.add(...)` rather than hitting the `None` early-return.
        app.init_asset::<DotsBoneWireframeMaterial>();

        // Spawn a synthetic TrackedHand. The `#[require(Transform, Visibility)]`
        // on `TrackedHand` auto-inserts those components so the hierarchy system
        // can attach children without B0004 warnings.
        let hand = app.world_mut().spawn(TrackedHand).id();

        app.world_mut()
            .run_system_once(ensure_bone_meshes)
            .expect("ensure_bone_meshes should run without error");

        let children = app
            .world()
            .get::<Children>(hand)
            .map(|c| c.to_vec())
            .unwrap_or_default();

        let bone_count = children
            .iter()
            .filter(|&&e| app.world().get::<BoneIndex>(e).is_some())
            .count();

        assert_eq!(
            bone_count, BONE_COUNT,
            "expected {BONE_COUNT} BoneIndex children on a fresh TrackedHand, got {bone_count}"
        );
    }

    /// Verifies that a second call to [`ensure_bone_meshes`] on a hand that
    /// already has bones is a no-op — the [`DotsHandMeshBones`] marker makes
    /// the reconcile idempotent.
    #[test]
    fn ensure_bone_meshes_is_idempotent() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(AssetPlugin::default());
        app.add_plugins(bevy::mesh::MeshPlugin);
        app.init_asset::<DotsBoneWireframeMaterial>();

        let hand = app.world_mut().spawn(TrackedHand).id();

        // First reconcile: spawns 20 children + inserts DotsHandMeshBones.
        app.world_mut()
            .run_system_once(ensure_bone_meshes)
            .expect("first ensure_bone_meshes run");

        // Second reconcile: should add nothing (hand already has DotsHandMeshBones).
        app.world_mut()
            .run_system_once(ensure_bone_meshes)
            .expect("second ensure_bone_meshes run");

        let bone_count = app.world().get::<Children>(hand).map_or(0, |c| {
            c.iter()
                .filter(|&e| app.world().get::<BoneIndex>(e).is_some())
                .count()
        });

        assert_eq!(
            bone_count, BONE_COUNT,
            "idempotent: second reconcile must not duplicate bones; got {bone_count}"
        );
    }
}
