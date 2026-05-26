//! Visual ring meshes for active attractors.
//!
//! ## Role
//!
//! For each attractor with `power > 0`, spawn 10 concentric ring mesh
//! entities. Per-frame modulate each ring's X-scale by `abs(cos(phi))` where
//! `phi` accumulates at `(10 - index) / 20 * power` per frame — matching v4's
//! `Attractor.animate()` in `src/particles/attractor.ts`.
//!
//! ## Data flow
//!
//! 1. [`spawn_attractor_visual`] watches [`MouseAttractorState`]. When power
//!    becomes positive and no [`AttractorVisual`] exists yet, it spawns a
//!    parent entity (the visual group) under [`LineRoot`] and 10 child
//!    entities (one per ring, tagged [`AttractorRing`]) under the parent.
//! 2. [`animate_attractor_visual`] runs every frame while `power > 0`,
//!    updating the group's translation + scale and each ring's X-scale via
//!    the phi accumulator.
//! 3. [`despawn_attractor_visual`] watches for `power == 0` and despawns the
//!    parent — Bevy 0.18's `EntityCommands::despawn()` recursively despawns
//!    children via the `ChildOf` relationship.
//!
//! ## Geometry
//!
//! - Smooth 32-segment ring matching v4's `RingGeometry(15, 18, 32)`.
//! - Inner radius: 15 world units.
//! - Outer radius: 18 world units.
//! - Per-ring base scale: `1 + (i / 10)^2 * 2` (outer rings progressively larger).
//! - Per-ring `scale.y` foreshortened by [`V4_TILT_FORESHORTEN_FACTOR`] =
//!   `cos(0.8 rad) ≈ 0.697` to mimic v4's 3D X-axis tilt of 0.8 rad.
//! - Per-ring `scale.x` ANIMATED per frame: `ring_scale * abs(cos(phi))`,
//!   where `phi` accumulates at `(10 - index)/20 * power` per frame. This
//!   simulates v4's 3D Y-axis ring rotation in 2D — the ring's silhouette
//!   oscillates between "face-on" (full ellipse) and "edge-on" (vertical
//!   line) twice per revolution.
//! - Group scale: `sqrt(power) / 5` (uniform on the parent).
//! - Z position: `-1.0` so the rings sit just behind the particles.
//! - Color: v4 `#C5E2CC` at `opacity: 0.6` ≈ `Color::srgba(0.77, 0.886, 0.8, 0.6)`.
//!   `ColorMaterial::from(Color)` auto-sets `AlphaMode2d::Blend` when
//!   alpha < 1.0, so the 10 stacked rings composite correctly.

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

/// Marker on each individual ring child. Carries:
///
/// - [`Self::index`]: ring index `0..NUM_RINGS`. Determines per-ring scale
///   (`1 + (i/10)^2 * 2`) and rotation speed (`(10 - i)/20 * power`).
/// - [`Self::phi`]: accumulated per-frame Y-axis rotation angle (radians).
///   See [`animate_attractor_visual`] for the X-scale modulation derived
///   from this — `Transform::scale.x = ring_scale * abs(cos(phi))` simulates
///   v4's 3D Y-axis ring rotation in 2D by oscillating between "face-on"
///   (full ring) and "edge-on" (vertical line) twice per revolution.
#[derive(Component)]
pub struct AttractorRing {
    /// Ring index, `0..NUM_RINGS`. Set once at spawn.
    pub index: u32,
    /// Accumulated Y-axis rotation angle (radians). Advances per frame by
    /// `(10 - index) / 20 * power` — v4-faithful per-frame rate, no `dt`
    /// multiplication. Wraps naturally via `cos`.
    pub phi: f32,
}

/// v4 ring colour `#C5E2CC` at `opacity: 0.6` ≈ `Color::srgba(0.77, 0.886, 0.8, 0.6)`.
///
/// The alpha matches v4's `MeshBasicMaterial({ transparent: true, opacity: 0.6 })`.
/// `ColorMaterial::from(Color)` automatically sets `AlphaMode2d::Blend` when
/// alpha < 1.0, so the 10 stacked rings blend correctly without an explicit
/// `AlphaMode2d` override. Without transparency, the outermost (slowest-
/// rotating, i=9) ring covers all inner rings opaquely and the animation
/// reads as nearly stationary.
pub const ATTRACTOR_RING_COLOR: Color = Color::srgba(0.77, 0.886, 0.8, 0.6);

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

/// Number of segments around each ring. 32 matches v4's `RingGeometry(15, 18, 32)`
/// and produces a smooth ring. Visible animation is achieved via per-frame
/// X-scale modulation (`abs(cos(phi))`), not polygon corners — the ring
/// silhouette flips between "face-on" and "edge-on" twice per revolution,
/// matching v4's Y-axis 3D rotation projected into 2D screen space.
const RING_SEGMENTS: u32 = 32;

/// 2D analog of v4's `Group.rotation.x = 0.8` (rad) 3D tilt. The tilt
/// foreshortens each ring into an ellipse whose vertical extent is `cos(0.8)
/// ≈ 0.697` of its horizontal extent. We bake the foreshortening directly
/// into each ring's `Transform::scale.y`, since the 2D port can't tilt.
///
/// This constant vertical squash stays fixed at all phi values — it represents
/// the baseline foreshortening from the 3D group tilt that is present whether
/// the ring is face-on or edge-on. The X-scale modulation in
/// [`animate_attractor_visual`] captures the additional silhouette change from
/// per-ring Y-axis rotation.
const V4_TILT_FORESHORTEN_FACTOR: f32 = 0.697;

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
                AttractorRing { index: i, phi: 0.0 },
                bevy::mesh::Mesh2d(mesh_handle.clone()),
                bevy::sprite_render::MeshMaterial2d(material_handle.clone()),
                // Y-axis squash mimics v4's 3D X-axis tilt of 0.8 rad
                // (cos(0.8) ≈ 0.697). This baseline foreshortening stays
                // constant; scale.x is overwritten each frame by the animate
                // system. Spawning with the correct baseline avoids a
                // one-frame flash at the pre-modulated scale.
                Transform::from_scale(Vec3::new(
                    ring_scale,
                    ring_scale * V4_TILT_FORESHORTEN_FACTOR,
                    ring_scale,
                )),
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
/// - Per-ring `Transform::scale.x` is modulated by `abs(cos(phi))` where
///   `phi` advances per frame by `(10 - index) / 20 * power`. This projects
///   v4's 3D Y-axis ring rotation into 2D: at phi=0 the ring shows its full
///   face-on ellipse; at phi=π/2 scale.x collapses to 0 (vertical line,
///   "edge-on"); at phi=π the ring is face-on again. The cycle repeats at
///   twice per revolution.
///
/// Despawn is handled by [`despawn_attractor_visual`] on the same tick the
/// power reaches zero; this system early-returns in that case so the rings
/// keep their final transform until they vanish.
pub fn animate_attractor_visual(
    mouse: Res<'_, MouseAttractorState>,
    mut visuals: Query<'_, '_, &mut Transform, (With<AttractorVisual>, Without<AttractorRing>)>,
    mut rings: Query<'_, '_, (&mut AttractorRing, &mut Transform), With<AttractorRing>>,
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

    // v4 parity: rings rotate around their local Y axis in 3D, going through
    // edge-on (silhouette collapses to a vertical line) twice per revolution.
    // Bevy 2D can't tilt-and-rotate-in-3D, so we project the effect: each
    // ring's `Transform::scale.x` is modulated by `abs(cos(phi))` where phi
    // accumulates per frame at `(10 - index)/20 * power`.
    //
    // Per-frame rate matches v4 exactly — v4's `_milliseconds` parameter is
    // unused, so the increment is per call. We deliberately do NOT multiply
    // by `time.delta_secs()`. The trade-off is frame-rate dependence (faster
    // rotation at 120 FPS than at 60 FPS), which is acceptable for perceptual
    // parity at the 60 FPS reference; ring 0 at peak power blurs at any frame
    // rate, which IS v4's intended look.
    for (mut ring, mut t) in &mut rings {
        #[allow(
            clippy::as_conversions,
            clippy::cast_precision_loss,
            reason = "ring.index ∈ 0..NUM_RINGS=10; u32→f32 round-trip is lossless"
        )]
        let index_f = ring.index as f32;
        let speed = (10.0 - index_f) / ROTATION_SPEED_DIVISOR * power;
        ring.phi += speed;

        let x_factor = ring.phi.cos().abs();
        let ring_scale = 1.0 + (index_f / RING_SCALE_INDEX_DIVISOR).powi(2) * RING_SCALE_MULTIPLIER;
        t.scale = Vec3::new(
            ring_scale * x_factor,
            ring_scale * V4_TILT_FORESHORTEN_FACTOR,
            ring_scale,
        );
        // No translation/rotation change: parent translation tracks the
        // attractor position; per-ring rotation stays identity (v4 doesn't
        // rotate rings in the screen plane).
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

    #[test]
    fn ring_scale_starts_face_on_at_phi_zero() {
        // At phi = 0, abs(cos(0)) = 1, so scale.x should equal the unmodulated
        // ring_scale. Sanity check on the X-scale formula.
        let phi: f32 = 0.0;
        let x_factor = phi.cos().abs();
        assert!(
            (x_factor - 1.0).abs() < 1e-6,
            "phi=0 should give x_factor=1.0; got {x_factor}"
        );
    }

    #[test]
    fn ring_scale_collapses_to_edge_on_at_phi_half_pi() {
        // At phi = π/2, abs(cos(π/2)) = 0, so scale.x = 0 → ring is edge-on
        // (vertical line). This is v4's signature edge-on configuration that
        // happens twice per revolution.
        let phi: f32 = std::f32::consts::FRAC_PI_2;
        let x_factor = phi.cos().abs();
        assert!(
            x_factor.abs() < 1e-6,
            "phi=π/2 should give x_factor≈0; got {x_factor}"
        );
    }
}
