//! Shared wireframe-bone hand overlay for sketches.
//!
//! Extracts the off-screen HDR bone-camera + additive composite that Line and
//! Dots each forked. Each consumer registers [`HandMeshPlugin`] with a
//! [`HandMeshConfig`]; the global [`bone_composite::HandMeshCompositePlugin`]
//! (registered once by `SketchesPlugin`) owns the composite pipeline and node.
//!
//! See [`bone_wireframe`] for the Metal-safe `LineList` bone mesh + material.

pub mod bone_composite;
pub mod bone_wireframe;

pub use bone_composite::{
    HandMeshCompositePlugin, HandMeshCompositeSet, HandMeshTarget, HandPresence,
};
pub use bone_wireframe::{icosphere_line_mesh, BoneWireframeMaterial};

use bevy::camera::visibility::RenderLayers;
use bevy::camera::{Hdr, RenderTarget, ScalingMode};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureFormat};
use bevy::render::view::Msaa;
use bevy::window::WindowResized;
use wc_core::input::entity::{BoneCenters, TrackedHand, BONE_COUNT};
use wc_core::input::projection::palm_to_world;
use wc_core::lifecycle::state::AppState;
use wc_core::sketch::sketch_active;

/// `RenderLayers` index for the Camera3d wireframe pass. Layer 0 is the default
/// (main Camera2d + 2D content); layer 1 is reserved for bone spheres + the
/// overlay `HandMeshCamera3d`.
pub const HAND_MESH_LAYER_INDEX: usize = 1;

/// The [`RenderLayers`] value for bone spheres and the overlay Camera3d.
pub const HAND_MESH_LAYER: RenderLayers = RenderLayers::layer(HAND_MESH_LAYER_INDEX);

/// Per-sketch configuration for the hand-mesh overlay. Inserted on
/// `OnEnter(config.app_state)`, read by `ensure_bone_meshes`, removed on exit.
#[derive(Resource, Clone)]
pub struct HandMeshConfig {
    /// Sketch this overlay belongs to.
    pub app_state: AppState,
    /// Base wireframe colour (sRGB); scaled by `glow_intensity` for HDR bloom.
    pub bone_color: Color,
    /// Emissive multiplier (`~3–8`) pushing the linear colour above 1.0.
    pub glow_intensity: f32,
    /// Radius of each bone icosphere, in logical pixels.
    pub bone_radius: f32,
}

/// Marker for the off-screen `Camera3d` that rasterizes the wireframe bones into
/// the [`HandMeshTarget`] image (raw linear HDR, no bloom, no tonemap).
#[derive(Component)]
pub struct HandMeshCamera3d;

/// Index of a bone sphere child on a `TrackedHand`. `0..BONE_COUNT` (20).
#[derive(Component, Debug, Clone, Copy)]
pub struct BoneIndex(pub usize);

/// Marker placed on a `TrackedHand` once its 20 bone children are attached, so
/// [`ensure_bone_meshes`] is idempotent. Removed on `OnExit` with the children.
#[derive(Component)]
struct HandMeshBones;

/// Plugin wiring the wireframe bone overlay for one sketch. Add once per sketch
/// with that sketch's [`HandMeshConfig`]. The composite node is owned globally
/// by [`HandMeshCompositePlugin`] (registered once by `SketchesPlugin`).
pub struct HandMeshPlugin {
    /// Per-sketch overlay configuration.
    pub config: HandMeshConfig,
}

impl Plugin for HandMeshPlugin {
    fn build(&self, app: &mut App) {
        let config = self.config.clone();
        let state = config.app_state;

        // Always insert per-sketch config + presence on enter. The `Fn`
        // closure re-clones `config` on every entry because `insert_resource`
        // consumes it; `config` is unused after this, so move it in.
        app.add_systems(OnEnter(state), move |mut commands: Commands<'_, '_>| {
            commands.insert_resource(config.clone());
            commands.insert_resource(HandPresence(false));
        });

        // `WC_DEBUG_DISABLE_BONE_CAMERA` skips the off-screen camera in debug.
        #[cfg(debug_assertions)]
        let spawn_camera = !app
            .world()
            .get_resource::<wc_core::debug::DebugToggles>()
            .is_some_and(|t| t.disable_bone_camera);
        #[cfg(not(debug_assertions))]
        let spawn_camera = true;
        if spawn_camera {
            app.add_systems(OnEnter(state), spawn_hand_mesh_camera);
        }

        app.add_systems(
            OnExit(state),
            (
                despawn_hand_mesh_camera,
                despawn_all_bone_children,
                remove_hand_mesh_config_and_presence,
            ),
        )
        .add_systems(
            Update,
            (
                ensure_bone_meshes,
                update_bone_transforms,
                update_hand_presence,
            )
                .chain()
                .run_if(sketch_active(state)),
        )
        .add_systems(Update, resize_bone_target.run_if(in_state(state)));
    }

    /// Not unique: each sketch (Line, Dots, Cymatics, …) registers its own
    /// `HandMeshPlugin` with that sketch's [`HandMeshConfig`]. Bevy 0.19's default
    /// `is_unique() == true` dedupes plugins by `name()` (the type name), so the
    /// second sketch's add would panic with "plugin was already added"; returning
    /// `false` lets the per-sketch instances coexist (each only wires systems for
    /// its own `app_state`). The shared composite node + bone material stay
    /// singletons in [`HandMeshCompositePlugin`] / the `MaterialPlugin` registered
    /// once by `SketchesPlugin`.
    fn is_unique(&self) -> bool {
        false
    }
}

/// `OnEnter` — create the off-screen bone image and spawn the `Camera3d` that
/// rasterizes the wireframe bones into it.
///
/// The camera renders **raw linear HDR** emissive bones on a black background
/// into an `Rgba16Float` image: no bloom, no tonemapping, `order = -1` (so the
/// image is populated before the main camera's Core2d graph samples it). The
/// glow and tonemap happen later, on the *main* camera, after
/// [`bone_composite::hand_mesh_composite`] adds this image into the scene (see
/// the module docs). `Msaa::Sample4` anti-aliases the wireframe lines; since
/// this camera owns a private off-screen target it can no longer collide with
/// the main camera's intermediate, so the MSAA value is now a free quality
/// choice rather than the load-bearing workaround it used to be.
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

/// `OnExit` — despawn the bone camera and drop the off-screen target so its
/// `Image` asset (and GPU texture) is freed, per the AGENTS.md rule that
/// per-sketch GPU resources are released on exit.
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

/// `OnExit` — despawn every bone-sphere child and clear the [`HandMeshBones`]
/// markers from their `TrackedHand` parents.
///
/// Despawning the children directly (rather than relying solely on hierarchy
/// cleanup) covers the case where the hands themselves persist across the
/// sketch exit — the hands stay tracked, but their bone visuals must go.
/// Clearing the marker lets [`ensure_bone_meshes`] re-spawn bones for those
/// same hands if the sketch is re-entered.
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

/// Reconcile pass (runs while the sketch is active): give every `TrackedHand`
/// that doesn't yet have bones its 20 wireframe-sphere children, then mark it
/// with [`HandMeshBones`].
///
/// Replaces an earlier `Add<TrackedHand>` observer gated on the sketch state.
/// That observer missed hands that were already being tracked at sketch entry:
/// hand-tracking runs in `PreUpdate`, which is *before* `StateTransition`, so
/// such hands were added while the state was still `Home` and never received
/// bones. Reconciling each frame on a `Without<HandMeshBones>` query is
/// timing-independent — it covers hands present at entry, hands that appear
/// mid-sketch, and hands that persist across a leave/re-enter — and is
/// idempotent (it does nothing once every hand is marked, the steady-state case).
///
/// `meshes` / `materials` are `Option`-wrapped so the system is a no-op in
/// headless `MinimalPlugins` test apps where the asset stores aren't registered.
fn ensure_bone_meshes(
    mut commands: Commands<'_, '_>,
    new_hands: Query<'_, '_, Entity, (With<TrackedHand>, Without<HandMeshBones>)>,
    meshes: Option<ResMut<'_, Assets<Mesh>>>,
    materials: Option<ResMut<'_, Assets<BoneWireframeMaterial>>>,
    config: Option<Res<'_, HandMeshConfig>>,
) {
    if new_hands.is_empty() {
        return;
    }
    let (Some(mut meshes), Some(mut materials), Some(config)) = (meshes, materials, config) else {
        return;
    };

    let line_mesh = meshes.add(icosphere_line_mesh(config.bone_radius));
    let base = config.bone_color.to_linear();
    let bone_material = materials.add(BoneWireframeMaterial {
        color: LinearRgba::rgb(
            base.red * config.glow_intensity,
            base.green * config.glow_intensity,
            base.blue * config.glow_intensity,
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

/// Per-frame: gate the bone camera + composite on tracked-hand presence.
/// Sets [`HandPresence`] and the bone camera's `is_active` from the
/// `TrackedHand` count. O(≤2 hands); no allocation.
fn update_hand_presence(
    hands: Query<'_, '_, (), With<TrackedHand>>,
    mut cameras: Query<'_, '_, &mut Camera, With<HandMeshCamera3d>>,
    mut presence: ResMut<'_, HandPresence>,
) {
    let present = !hands.is_empty();
    presence.0 = present;
    for mut cam in &mut cameras {
        cam.is_active = present;
    }
}

/// `OnExit` — drop the per-sketch config + presence flag. Removing
/// [`HandPresence`] triggers the render-world removal system so the composite
/// guard resets cleanly when the sketch is not active.
fn remove_hand_mesh_config_and_presence(mut commands: Commands<'_, '_>) {
    commands.remove_resource::<HandMeshConfig>();
    commands.remove_resource::<HandPresence>();
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
    /// required asset stores and [`HandMeshConfig`] are present.
    ///
    /// Uses `App` + `AssetPlugin` + `MeshPlugin` + `init_asset` to register
    /// the asset stores (without them `ensure_bone_meshes` no-ops cleanly per
    /// its `Option`-wrapped params). Mirrors the reconcile-vs-observer design
    /// note in the module docs: the system is timing-independent, so calling it
    /// once via `run_system_once` is equivalent to the first Update tick.
    #[test]
    fn ensure_bone_meshes_spawns_20_bone_children() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(AssetPlugin::default());
        app.add_plugins(bevy::mesh::MeshPlugin);
        // Register `Assets<BoneWireframeMaterial>` so `ensure_bone_meshes`
        // can call `materials.add(...)` rather than hitting the `None` early-return.
        app.init_asset::<BoneWireframeMaterial>();
        // Seed the config resource so `ensure_bone_meshes` can read colour/radius.
        app.insert_resource(HandMeshConfig {
            app_state: AppState::Dots,
            bone_color: Color::WHITE,
            glow_intensity: 5.0,
            bone_radius: 10.0,
        });

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
    /// already has bones is a no-op — the [`HandMeshBones`] marker makes the
    /// reconcile idempotent.
    #[test]
    fn ensure_bone_meshes_is_idempotent() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(AssetPlugin::default());
        app.add_plugins(bevy::mesh::MeshPlugin);
        app.init_asset::<BoneWireframeMaterial>();
        app.insert_resource(HandMeshConfig {
            app_state: AppState::Dots,
            bone_color: Color::WHITE,
            glow_intensity: 5.0,
            bone_radius: 10.0,
        });

        let hand = app.world_mut().spawn(TrackedHand).id();

        // First reconcile: spawns 20 children + inserts HandMeshBones.
        app.world_mut()
            .run_system_once(ensure_bone_meshes)
            .expect("first ensure_bone_meshes run");

        // Second reconcile: should add nothing (hand already has HandMeshBones).
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

    /// Verifies [`update_hand_presence`] gates the bone camera and the
    /// [`HandPresence`] resource on tracked-hand presence.
    ///
    /// With zero [`TrackedHand`] entities: camera `is_active` must be `false`
    /// and `HandPresence.0` must be `false`. After spawning one [`TrackedHand`]
    /// and running the system again: both must be `true`.
    ///
    /// This is the primary unit gate for the presence logic — the no-hands path
    /// is verifiable headlessly; the hands-present rendering path (bones still
    /// appear, no ghosting on exit) requires Leap/MediaPipe hardware and is
    /// operator-deferred.
    #[test]
    fn update_hand_presence_gates_camera_and_resource() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);

        // Seed the resource exactly as OnEnter will do.
        app.insert_resource(HandPresence(false));

        // Spawn a bone camera entity. Camera::default() has is_active = true,
        // so the system must flip it to false on the zero-hands run.
        let camera = app
            .world_mut()
            .spawn((HandMeshCamera3d, Camera::default()))
            .id();

        // --- zero TrackedHand entities ---
        app.world_mut()
            .run_system_once(update_hand_presence)
            .expect("update_hand_presence should run (zero hands)");

        assert!(
            !app.world()
                .get::<Camera>(camera)
                .expect("camera must exist")
                .is_active,
            "camera must be inactive when no TrackedHand is present"
        );
        assert!(
            !app.world().resource::<HandPresence>().0,
            "HandPresence must be false when no TrackedHand is present"
        );

        // --- spawn one TrackedHand ---
        app.world_mut().spawn(TrackedHand);

        app.world_mut()
            .run_system_once(update_hand_presence)
            .expect("update_hand_presence should run (one hand)");

        assert!(
            app.world()
                .get::<Camera>(camera)
                .expect("camera must exist")
                .is_active,
            "camera must be active when a TrackedHand is present"
        );
        assert!(
            app.world().resource::<HandPresence>().0,
            "HandPresence must be true when a TrackedHand is present"
        );
    }
}
