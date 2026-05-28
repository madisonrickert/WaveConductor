//! Wireframe bone visualization for the Line sketch.
//!
//! ## Role
//!
//! Ports v4's `HandMesh` wireframe rendering onto v5's `TrackedHand` entity
//! model. Each tracked hand entity spawns 20 wireframe icosphere children
//! (one per bone). The bones render to an off-screen `Image` via a dedicated
//! [`HandMeshCamera3d`] on [`HAND_MESH_LAYER`], and a separate compositor
//! [`HandMeshCompositorCamera`] draws that image as a sprite on top of the
//! Camera2d output of the Line scene.
//!
//! ## Why the off-screen image
//!
//! A first attempt rendered Camera3d directly to the swap chain at `order = 1`
//! with `ClearColorConfig::None`. With the Line scene's HDR Camera2d (gravity
//! post-process + bloom + `AgX` tonemap) drawing at `order = 0`, Bevy 0.18's
//! per-frame swap chain content was not being fully reset between the two
//! cameras — bones accumulated in opaque pixels, eventually covering the
//! particles + UI. Cropping at window edges matched the Camera3d frustum.
//!
//! Routing through an alpha-aware off-screen target severs that contention:
//! Camera3d clears the image to fully-transparent each frame, draws the
//! bones, and the compositor sprite alpha-blends over the swap chain
//! exactly where bones are.
//!
//! ## Data flow
//!
//! 1. `OnEnter(AppState::Line)`: [`spawn_hand_mesh_camera`] allocates the
//!    off-screen `Image` sized to the window, then spawns
//!    `HandMeshCamera3d` (targeting the image), `HandMeshCompositorSprite`
//!    (textured with the image), and `HandMeshCompositorCamera` (a Camera2d
//!    at `order = 2` that draws only the compositor sprite layer onto the
//!    swap chain without clearing).
//! 2. `On<Add, TrackedHand>` observer: [`spawn_bones_on_tracked_hand_added`]
//!    attaches 20 wireframe-sphere children to the new `TrackedHand`.
//! 3. Every `Update`: [`update_bone_transforms`] writes the projected
//!    bone-center world coords to each sphere's `Transform`.
//! 4. `OnExit(AppState::Line)`: [`despawn_hand_mesh_camera`] tears down the
//!    Camera3d, compositor camera, compositor sprite, and the `Image`
//!    handle (asset is GC'd once the last handle drops).

use bevy::camera::visibility::RenderLayers;
use bevy::camera::{ImageRenderTarget, RenderTarget, ScalingMode};
use bevy::image::Image;
use bevy::pbr::wireframe::{Wireframe, WireframeColor};
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use wc_core::input::entity::{BoneCenters, TrackedHand, BONE_COUNT};
use wc_core::input::projection::palm_to_world;
use wc_core::lifecycle::state::AppState;
use wc_core::sketch::sketch_active;

/// `RenderLayers` index for the Camera3d wireframe pass.
///
/// Layer 0 is the default layer used by the main `Camera2d` and all 2D
/// content. Layer 1 is reserved exclusively for bone-sphere children +
/// `HandMeshCamera3d`.
pub const HAND_MESH_LAYER_INDEX: usize = 1;

/// `RenderLayers` index for the compositor `Camera2d` and its sprite.
///
/// Layer 2 is reserved exclusively for the single fullscreen
/// `HandMeshCompositorSprite` that displays the off-screen render target,
/// drawn only by `HandMeshCompositorCamera`.
pub const HAND_MESH_COMPOSITOR_LAYER_INDEX: usize = 2;

/// The [`RenderLayers`] value for bone spheres and the Camera3d.
pub const HAND_MESH_LAYER: RenderLayers = RenderLayers::layer(HAND_MESH_LAYER_INDEX);

/// The [`RenderLayers`] value for the compositor sprite + compositor camera.
pub const HAND_MESH_COMPOSITOR_LAYER: RenderLayers =
    RenderLayers::layer(HAND_MESH_COMPOSITOR_LAYER_INDEX);

/// Wireframe color matching v4's `HandMesh` `defaultMaterial` (`#add6b6`).
fn hand_mesh_color() -> Color {
    Color::srgb(
        f32::from(0xad_u8) / 255.0,
        f32::from(0xd6_u8) / 255.0,
        f32::from(0xb6_u8) / 255.0,
    )
}

/// Marker for the off-screen `Camera3d` that rasterizes the wireframe bones
/// into the shared image target.
#[derive(Component)]
pub struct HandMeshCamera3d;

/// Marker for the `Camera2d` that composites the off-screen image over the
/// Line scene's swap chain output.
#[derive(Component)]
pub struct HandMeshCompositorCamera;

/// Marker for the `Sprite` that carries the off-screen image and feeds the
/// compositor camera.
#[derive(Component)]
pub struct HandMeshCompositorSprite;

/// Index of a bone sphere child on a `TrackedHand` entity.
///
/// Value is `0..BONE_COUNT` (20). Set once at spawn; used by
/// [`update_bone_transforms`] to index into `BoneCenters`.
#[derive(Component, Debug, Clone, Copy)]
pub struct BoneIndex(pub usize);

/// Plugin wiring the wireframe bone visualization.
pub struct LineHandMeshPlugin;

impl Plugin for LineHandMeshPlugin {
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

/// `OnEnter(AppState::Line)` — allocate the off-screen image and spawn the
/// three entities that drive the bone overlay: Camera3d (writes), compositor
/// sprite (carries the image), compositor Camera2d (composites onto the
/// swap chain).
fn spawn_hand_mesh_camera(
    mut commands: Commands<'_, '_>,
    mut images: ResMut<'_, Assets<Image>>,
    window: Single<'_, '_, &Window>,
) {
    // Logical-pixel sizing keeps the off-screen image, the Camera3d
    // frustum, the Camera2d compositor frustum, and the sprite all on the
    // same coordinate convention. HiDPI sharpness for the bones is a
    // non-goal (wireframes are thin lines; the resolution penalty isn't
    // visible at viewing distance). `.round() + max(1, ..)` clamps the
    // edge case where the window's logical size has not been resolved yet
    // (zero-size would trigger a wgpu validation error). The truncation
    // via `as_u32_lossy` is acceptable here because window dimensions are
    // always positive and well within `u32::MAX`.
    let width = f32_to_u32_lossy(window.width()).max(1);
    let height = f32_to_u32_lossy(window.height()).max(1);

    // Canonical Bevy 0.18 render-to-texture format pair (matches
    // `examples/3d/render_to_texture.rs`): `Rgba8Unorm` as the storage
    // format, with an `Rgba8UnormSrgb` view so sampling produces
    // gamma-correct values. `Image::new_target_texture` sets the
    // required `TEXTURE_BINDING | COPY_DST | RENDER_ATTACHMENT` usages
    // for us.
    let image = Image::new_target_texture(
        width,
        height,
        TextureFormat::Rgba8Unorm,
        Some(TextureFormat::Rgba8UnormSrgb),
    );
    let image_handle = images.add(image);

    // Camera3d — wireframe bone pass.
    //
    // Clears its render-target image to fully transparent each frame, so
    // bones from the previous frame are gone before this frame's geometry
    // is drawn. Without the image-based clear, the swap chain accumulates
    // opaque bone pixels and eventually hides everything underneath.
    //
    // `RenderTarget` is a Bevy-0.18 component required by `Camera` (via
    // `#[require]`); it's no longer a field of `Camera` as in earlier
    // versions. Setting it explicitly here overrides the default of
    // `RenderTarget::Window(PrimaryWindow)`.
    commands.spawn((
        HandMeshCamera3d,
        Camera3d::default(),
        Camera {
            // Negative order = renders before the main Camera2d (order 0).
            // Per Bevy's canonical `render_to_texture` example: image-target
            // cameras run with negative order so their output is available
            // when subsequent passes read the texture in the same frame.
            order: -1,
            clear_color: ClearColorConfig::Custom(Color::NONE),
            ..default()
        },
        RenderTarget::Image(ImageRenderTarget {
            handle: image_handle.clone(),
            scale_factor: 1.0,
        }),
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: ScalingMode::WindowSize,
            near: -1000.0,
            far: 1000.0,
            ..OrthographicProjection::default_3d()
        }),
        Transform::from_xyz(0.0, 0.0, 500.0).looking_at(Vec3::ZERO, Vec3::Y),
        HAND_MESH_LAYER,
    ));

    // Compositor sprite — textured with the off-screen image, sized to
    // match the window in world units. Sits at origin so the compositor
    // Camera2d sees it centred. `custom_size` overrides the implicit
    // physical-pixel sizing of the image to keep the sprite in logical
    // units (matches Camera2d's default orthographic scaling).
    commands.spawn((
        HandMeshCompositorSprite,
        Sprite {
            image: image_handle,
            custom_size: Some(Vec2::new(window.width(), window.height())),
            ..default()
        },
        Transform::default(),
        HAND_MESH_COMPOSITOR_LAYER,
    ));

    // Compositor Camera2d — `order = 2` runs after the main Camera2d
    // (`order = 0`). `HandMeshCamera3d` (`order = -1`) has already
    // populated the off-screen image by the time this camera renders.
    // `ClearColorConfig::None` preserves the main Camera2d's
    // tonemapped swap-chain output; sprites alpha-blend over it by
    // default.
    commands.spawn((
        HandMeshCompositorCamera,
        Camera2d,
        Camera {
            order: 2,
            clear_color: ClearColorConfig::None,
            ..default()
        },
        HAND_MESH_COMPOSITOR_LAYER,
    ));
}

/// `OnExit(AppState::Line)` — despawn all three compositor entities. The
/// off-screen `Image` asset reference count drops to zero when the Camera3d
/// and Sprite both despawn, and Bevy's asset GC frees the GPU texture
/// shortly after.
fn despawn_hand_mesh_camera(
    mut commands: Commands<'_, '_>,
    cameras: Query<'_, '_, Entity, With<HandMeshCamera3d>>,
    compositor_cameras: Query<'_, '_, Entity, With<HandMeshCompositorCamera>>,
    compositor_sprites: Query<'_, '_, Entity, With<HandMeshCompositorSprite>>,
) {
    for entity in &cameras {
        commands.entity(entity).despawn();
    }
    for entity in &compositor_cameras {
        commands.entity(entity).despawn();
    }
    for entity in &compositor_sprites {
        commands.entity(entity).despawn();
    }
}

/// `OnExit(AppState::Line)` — despawn orphaned bone-sphere children that
/// outlived their `TrackedHand` parent. Under normal operation Bevy's
/// hierarchy cleanup handles this; the explicit query is a guard against
/// any race where the sketch exits mid-frame.
fn despawn_all_bone_children(
    mut commands: Commands<'_, '_>,
    query: Query<'_, '_, Entity, With<BoneIndex>>,
) {
    for entity in &query {
        commands.entity(entity).despawn();
    }
}

/// Observer — on `Add<TrackedHand>` while Line is active, spawn 20 bone
/// children. Bones inherit the parent `TrackedHand`'s hierarchy and despawn
/// with it.
///
/// `meshes` and `materials` are `Option`-wrapped so the observer also runs
/// cleanly in headless `MinimalPlugins` test apps where the asset stores
/// aren't registered.
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
        return;
    };
    spawn_bones(trigger.event_target(), commands, meshes, materials);
}

/// Spawn 20 wireframe icosphere children on `parent` (a `TrackedHand`).
fn spawn_bones(
    parent: Entity,
    mut commands: Commands<'_, '_>,
    mut meshes: ResMut<'_, Assets<Mesh>>,
    mut materials: ResMut<'_, Assets<StandardMaterial>>,
) {
    let color = hand_mesh_color();

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

/// Cast a non-negative `f32` (a logical window dimension in pixels) to `u32`.
///
/// The workspace lint suite denies bare `as` casts and the floating-point
/// truncation variants. This helper localises the lossy conversion to one
/// site with a documented invariant: window dimensions are always finite,
/// non-negative, and well below `u32::MAX`. Saturates to `u32::MAX` if the
/// value is implausibly large (NaN clamps to 0).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "window dimensions are non-negative finite f32 within u32 range; \
              checked at runtime via `is_finite` + `>= 0.0`"
)]
fn f32_to_u32_lossy(v: f32) -> u32 {
    if v.is_finite() && v >= 0.0 {
        v as u32
    } else {
        0
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
