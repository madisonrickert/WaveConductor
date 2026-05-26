//! Visual ring meshes for active attractors.
//!
//! ## Role
//!
//! For each attractor with `power > 0`, spawn 10 concentric annulus mesh
//! entities. Per-frame rotate each ring (speed ∝ power, varies by ring
//! index) and scale the group by `sqrt(power) / 5` — matching v4's
//! `Attractor.animate()` in `src/particles/attractor.ts`.
//!
//! ## Data flow
//!
//! 1. [`spawn_attractor_visual`] watches [`MouseAttractorState`]. When power
//!    becomes positive and no [`AttractorVisual`] exists yet, it spawns a
//!    parent entity (the visual group) under [`LineRoot`] and 10 child
//!    entities (one per ring, tagged [`AttractorRing`]) under the parent.
//! 2. [`animate_attractor_visual`] runs every frame while `power > 0`,
//!    updating the group's translation + scale and each ring's rotation.
//! 3. [`despawn_attractor_visual`] watches for `power == 0` and despawns the
//!    parent — Bevy 0.18's `EntityCommands::despawn()` recursively despawns
//!    children via the `ChildOf` relationship.
//!
//! ## Geometry
//!
//! - Inner radius: 15 world units.
//! - Outer radius: 18 world units.
//! - Per-ring scale: `1 + (i / 10)^2 * 2` (outer rings progressively larger).
//! - Group scale: `sqrt(power) / 5`.
//! - Per-ring rotation speed: `(10 - i) / 20 * power` rad/s (inner rings
//!   spin faster).
//! - Z position: `-1.0` so the rings sit just behind the particles.
//! - Color: v4 `#C5E2CC` ≈ `Color::srgb(0.77, 0.886, 0.8)`.

use bevy::color::Color;
use bevy::math::primitives::Annulus;
use bevy::prelude::*;
use bevy::sprite_render::ColorMaterial;

use super::systems::MouseAttractorState;
use super::LineRoot;

/// Marker on the parent entity that owns all 10 ring children for an attractor.
///
/// The parent's `Transform` carries the attractor's world-space position and
/// the group scale (`sqrt(power) / 5`). Children inherit this transform via
/// Bevy's transform-propagation system.
#[derive(Component)]
pub struct AttractorVisual;

/// Marker on each individual ring child. Carries its ring index `0..NUM_RINGS`
/// so [`animate_attractor_visual`] can scale rotation speed by index.
#[derive(Component)]
pub struct AttractorRing(pub u32);

/// v4 ring colour `#C5E2CC` — `Color::srgb(0.77, 0.886, 0.8)`. Stored once at
/// module scope so the spawn system uses a single constant value (and the
/// expected colour is greppable from tests / inspectors).
pub const ATTRACTOR_RING_COLOR: Color = Color::srgb(0.77, 0.886, 0.8);

/// Number of concentric rings per attractor visual. Matches v4's
/// `Attractor.RING_COUNT`.
const NUM_RINGS: u32 = 10;

/// Inner radius of the annulus mesh (world units, before per-ring scaling).
const RING_INNER_RADIUS: f32 = 15.0;

/// Outer radius of the annulus mesh (world units, before per-ring scaling).
const RING_OUTER_RADIUS: f32 = 18.0;

/// Group scale denominator: `scale = sqrt(power) / 5`. v4 parity.
const GROUP_SCALE_DIVISOR: f32 = 5.0;

/// Per-ring scale curve denominator: `1 + (i / 10)^2 * 2`.
const RING_SCALE_INDEX_DIVISOR: f32 = 10.0;

/// Per-ring scale curve multiplier: `1 + (i / 10)^2 * 2`.
const RING_SCALE_MULTIPLIER: f32 = 2.0;

/// Rotation speed denominator: `speed = (10 - i) / 20 * power`. v4 parity.
const ROTATION_SPEED_DIVISOR: f32 = 20.0;

/// Z offset for the attractor visual parent — sits just behind particles
/// (which render at z=0) so the rings appear underneath the star sprites.
const VISUAL_Z: f32 = -1.0;

/// Spawn the 10-ring visual for the (single) mouse attractor when its power
/// becomes positive and no visual already exists.
///
/// **Invariant:** the early-return on `!visuals.is_empty()` is load-bearing.
/// Without it, this system would spawn 10 new ring entities every frame the
/// button is held, exhausting entity IDs and tanking the frame rate.
///
/// Plan 8 Phase B handles only the single mouse attractor. Multi-attractor
/// support (Leap hands) lands in a later plan that will likely re-shape this
/// system to key visuals by attractor ID rather than the
/// "any visual exists?" guard used here.
pub fn spawn_attractor_visual(
    mut commands: Commands<'_, '_>,
    mouse: Res<'_, MouseAttractorState>,
    visuals: Query<'_, '_, Entity, With<AttractorVisual>>,
    mut meshes: ResMut<'_, Assets<Mesh>>,
    mut materials: ResMut<'_, Assets<ColorMaterial>>,
    line_root: Query<'_, '_, Entity, With<LineRoot>>,
) {
    if mouse.power <= 0.0 || !visuals.is_empty() {
        return;
    }
    let Some(root) = line_root.iter().next() else {
        // The sketch hasn't spawned yet (or has been torn down). Nothing to
        // parent the visual onto — try again next frame.
        return;
    };

    // One mesh + one material shared across all 10 rings of this visual.
    // Per-ring `Transform` carries the index-dependent scale.
    let mesh_handle = meshes.add(Mesh::from(Annulus::new(
        RING_INNER_RADIUS,
        RING_OUTER_RADIUS,
    )));
    let material_handle = materials.add(ColorMaterial::from(ATTRACTOR_RING_COLOR));

    let parent = commands
        .spawn((
            AttractorVisual,
            Transform::from_translation(Vec3::new(mouse.position[0], mouse.position[1], VISUAL_Z)),
            GlobalTransform::default(),
            Visibility::Visible,
        ))
        .id();
    commands.entity(root).add_child(parent);

    for i in 0..NUM_RINGS {
        // Outer rings are progressively larger: ring 0 = 1.0×, ring 9 ≈ 2.62×.
        #[allow(
            clippy::as_conversions,
            clippy::cast_precision_loss,
            reason = "i ∈ 0..NUM_RINGS=10; u32→f32 round-trip is lossless"
        )]
        let ring_index_f = i as f32;
        let ring_scale =
            1.0 + (ring_index_f / RING_SCALE_INDEX_DIVISOR).powi(2) * RING_SCALE_MULTIPLIER;
        let child = commands
            .spawn((
                AttractorRing(i),
                bevy::mesh::Mesh2d(mesh_handle.clone()),
                bevy::sprite_render::MeshMaterial2d(material_handle.clone()),
                Transform::from_scale(Vec3::splat(ring_scale)),
                GlobalTransform::default(),
                Visibility::default(),
            ))
            .id();
        commands.entity(parent).add_child(child);
    }
}

/// Animate the rings while attractor power is non-zero.
///
/// - Group translation tracks `MouseAttractorState.position`.
/// - Group scale tracks `sqrt(power) / 5`.
/// - Per-ring rotation accumulates `(10 - ring_index) / 20 * power * dt`
///   radians around Z each frame.
///
/// Despawn is handled by [`despawn_attractor_visual`] on the same tick the
/// power reaches zero; this system early-returns in that case so the rings
/// keep their final transform until they vanish.
pub fn animate_attractor_visual(
    time: Res<'_, Time>,
    mouse: Res<'_, MouseAttractorState>,
    mut visuals: Query<'_, '_, &mut Transform, (With<AttractorVisual>, Without<AttractorRing>)>,
    mut rings: Query<'_, '_, (&AttractorRing, &mut Transform), With<AttractorRing>>,
) {
    let power = mouse.power;
    if power <= 0.0 {
        return;
    }
    let group_scale = power.sqrt() / GROUP_SCALE_DIVISOR;
    for mut t in &mut visuals {
        t.translation.x = mouse.position[0];
        t.translation.y = mouse.position[1];
        // Z stays at VISUAL_Z (set at spawn); only XY tracks the pointer.
        t.scale = Vec3::splat(group_scale);
    }
    let dt = time.delta_secs();
    for (ring, mut t) in &mut rings {
        #[allow(
            clippy::as_conversions,
            clippy::cast_precision_loss,
            reason = "ring.0 ∈ 0..NUM_RINGS=10; u32→f32 round-trip is lossless"
        )]
        let ring_index_f = ring.0 as f32;
        let speed = (10.0 - ring_index_f) / ROTATION_SPEED_DIVISOR * power;
        t.rotation *= Quat::from_rotation_z(speed * dt);
    }
}

/// Despawn the ring visual once attractor power drops back to zero.
///
/// Bevy 0.18's `EntityCommands::despawn()` recursively despawns descendants
/// linked through the `ChildOf` relationship, so a single `despawn()` on the
/// `AttractorVisual` parent removes the 10 ring children too.
pub fn despawn_attractor_visual(
    mut commands: Commands<'_, '_>,
    mouse: Res<'_, MouseAttractorState>,
    visuals: Query<'_, '_, Entity, With<AttractorVisual>>,
) {
    if mouse.power > 0.0 {
        return;
    }
    for entity in &visuals {
        commands.entity(entity).despawn();
    }
}
