//! Visual ring meshes for active attractors.
//!
//! ## Role
//!
//! For each attractor with `power > 0`, spawn 10 concentric polygonal ring mesh
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
//! - Polygonal ring with [`RING_SEGMENTS`] = 6 segments. v4 uses 32-segment
//!   smooth annuli but tilts the parent group 0.8 rad on X so Y-rotation is
//!   visibly elliptical. This 2D port can't tilt, so a 6-segment polygon
//!   gives the rotation a legible corner to spin around — see Plan 11 § A.
//! - Inner radius: 15 world units.
//! - Outer radius: 18 world units.
//! - Per-ring scale: `1 + (i / 10)^2 * 2` (outer rings progressively larger).
//! - Group scale: `sqrt(power) / 5`.
//! - Per-ring rotation speed: `(10 - i) / 20 * power` rad/s (inner rings
//!   spin faster).
//! - Z position: `-1.0` so the rings sit just behind the particles.
//! - Color: v4 `#C5E2CC` ≈ `Color::srgb(0.77, 0.886, 0.8)`.

use bevy::color::Color;
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

/// Inner radius of the polygonal ring mesh (world units, before per-ring scaling).
const RING_INNER_RADIUS: f32 = 15.0;

/// Outer radius of the polygonal ring mesh (world units, before per-ring scaling).
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

/// Number of segments around each ring. Six is the smallest count that still
/// reads as a "ring" (a circle) at typical viewing distances but is angular
/// enough that the per-frame rotation is visibly perceivable. v4 uses 32 with
/// a 3D tilt; we use 6 to compensate for the lack of 3D in this 2D port.
///
/// Carry-forward #56 (PARITY.md verdict §1) is the source-of-record for the
/// rotation-visibility motivation.
const RING_SEGMENTS: u32 = 6;

/// Build a flat polygonal ring mesh as an indexed triangle list.
///
/// Vertices alternate inner / outer around the ring at evenly-spaced angles
/// (`segments` segments → `2 × segments` vertices). The triangle list links
/// each pair `(inner_i, outer_i, outer_{i+1})` and `(inner_i, outer_{i+1},
/// inner_{i+1})` so the ring is two strips of triangles closing on itself.
///
/// Built once at sketch entry; all 10 rings of an attractor visual share this
/// mesh handle and use per-entity `Transform::scale` to size themselves.
///
/// Returns a `Mesh` with `Float32x3` positions and indexed topology. No
/// normals or UVs — the ring material is a flat `ColorMaterial`.
fn build_polygonal_ring_mesh(inner_radius: f32, outer_radius: f32, segments: u32) -> Mesh {
    use bevy::asset::RenderAssetUsages;
    use bevy::mesh::{Indices, PrimitiveTopology};

    let n = segments;
    // usize::try_from is infallible on all targets (u32 ≤ usize on 32-bit+).
    let n_usize = usize::try_from(n).unwrap_or(0);
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(2 * n_usize);
    for i in 0..n {
        #[allow(
            clippy::as_conversions,
            clippy::cast_precision_loss,
            reason = "i ∈ 0..segments (≤ 16 in practice); u32→f32 round-trip is lossless"
        )]
        let angle = (i as f32) / (n as f32) * std::f32::consts::TAU;
        let (s, c) = angle.sin_cos();
        // Convention used by tests: even index i → inner, odd index → outer.
        positions.push([c * inner_radius, s * inner_radius, 0.0]);
        positions.push([c * outer_radius, s * outer_radius, 0.0]);
    }

    let mut indices: Vec<u32> = Vec::with_capacity(6 * n_usize);
    for i in 0..n {
        let inner_i = 2 * i;
        let outer_i = 2 * i + 1;
        let inner_next = 2 * ((i + 1) % n);
        let outer_next = 2 * ((i + 1) % n) + 1;
        // Two triangles per segment forming a quad slice of the ring.
        indices.extend_from_slice(&[inner_i, outer_i, outer_next]);
        indices.extend_from_slice(&[inner_i, outer_next, inner_next]);
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

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
    let mesh_handle = meshes.add(build_polygonal_ring_mesh(
        RING_INNER_RADIUS,
        RING_OUTER_RADIUS,
        RING_SEGMENTS,
    ));
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

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::panic,
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "expect/panic/as-cast with a clear message is appropriate in test code"
)]
mod tests {
    use super::*;
    use bevy::mesh::PrimitiveTopology;

    #[test]
    fn polygonal_ring_has_2n_vertices_and_2n_triangles() {
        let n: u32 = 6;
        let mesh = build_polygonal_ring_mesh(RING_INNER_RADIUS, RING_OUTER_RADIUS, n);
        assert_eq!(mesh.primitive_topology(), PrimitiveTopology::TriangleList);
        // 2n vertices (one inner + one outer per segment).
        let positions = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .expect("position attribute");
        let n_usize = usize::try_from(n).expect("n fits in usize");
        if let bevy::mesh::VertexAttributeValues::Float32x3(pos) = positions {
            assert_eq!(pos.len(), 2 * n_usize);
        } else {
            panic!("position attribute must be Float32x3");
        }
        // 2n triangles → 6n indices (3 per triangle).
        let indices = mesh.indices().expect("indexed mesh");
        assert_eq!(indices.len(), 6 * n_usize);
    }

    #[test]
    fn polygonal_ring_first_outer_vertex_is_on_outer_radius() {
        let mesh = build_polygonal_ring_mesh(15.0, 18.0, 6);
        let positions = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .expect("position attribute");
        if let bevy::mesh::VertexAttributeValues::Float32x3(pos) = positions {
            // Convention used by build_polygonal_ring_mesh:
            // vertex 0 = inner radius at angle 0; vertex 1 = outer at angle 0.
            let inner = pos[0];
            let outer = pos[1];
            let inner_len = (inner[0] * inner[0] + inner[1] * inner[1]).sqrt();
            let outer_len = (outer[0] * outer[0] + outer[1] * outer[1]).sqrt();
            assert!((inner_len - 15.0).abs() < 1e-4);
            assert!((outer_len - 18.0).abs() < 1e-4);
        } else {
            panic!("position attribute must be Float32x3");
        }
    }
}
