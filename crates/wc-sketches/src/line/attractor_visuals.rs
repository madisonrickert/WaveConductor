//! Multi-axis gyroscope visual for active attractors.
//!
//! ## Role
//!
//! For each active attractor — the singleton mouse/touch attractor *and* each
//! grabbing Leap hand — spawn a 10-ring multi-axis gyroscope: each ring is
//! assigned to one of three "gimbals" (X-axis, Y-axis, or Z-axis rotation) by
//! `index % 3`, with a per-gimbal rate multiplier and an outer-rings-slower base
//! speed. This matches v4, which drew the gyroscope at every attractor (Plan 8
//! shipped only the mouse path; the per-hand path closes that parity gap).
//!
//! ## Data flow
//!
//! Each visual group carries an [`AttractorSource`] tagging which attractor it
//! tracks ([`AttractorSource::Mouse`] or [`AttractorSource::Hand`] keyed by the
//! `TrackedHand` entity), so the mouse and any number of hands coexist.
//!
//! 1. [`spawn_attractor_visual`] spawns a group (parent + 10 [`AttractorRing`]
//!    children under [`LineRoot`]) for every source whose power exceeds
//!    [`VISUAL_POWER_EPSILON`] and that doesn't already have a visual.
//! 2. [`animate_attractor_visual`] runs every frame, updating each group's
//!    translation + scale from its source's position + power and advancing each
//!    ring's transform along its gimbal axis.
//! 3. [`despawn_attractor_visual`] despawns a group once its source's power
//!    drops back below the epsilon (mouse released, hand opened, or hand left
//!    the volume) — Bevy 0.18's `EntityCommands::despawn()` recursively despawns
//!    the ring children via the `ChildOf` relationship.
//!
//! ## Gyroscope geometry
//!
//! Per ring (index `i ∈ 0..10`):
//!
//! - **Gimbal axis** = `i % 3`. Determines how the ring's per-frame `phi`
//!   accumulator maps to a 2D transform:
//!   - `0` (X-axis): `scale.y = ring_scale * abs(cos(phi))` — vertical extent
//!     oscillates; the ring tips face-on → edge-on twice per revolution.
//!   - `1` (Y-axis): `scale.x = ring_scale * abs(cos(phi))` — horizontal
//!     extent oscillates.
//!   - `2` (Z-axis): in-plane rotation by `phi` with a baked-in elliptical
//!     `scale.y = ring_scale * Z_GIMBAL_ELLIPSE_RATIO` so the spin is visible.
//! - **Base scale** = `1 + (i / 10)² × 2` — outer rings progressively larger
//!   (matches v4's per-ring sizing curve).
//! - **Rotation speed** = `((10 - i) / 20) × GIMBAL_RATE[gimbal] × power` —
//!   outer rings slower, with per-gimbal multipliers that desynchronise the
//!   three axes for a chaotic gyroscopic feel.
//! - **Phase offset** = small deterministic per-ring stagger so the gimbal-0
//!   and gimbal-1 rings don't all reach edge-on simultaneously.
//!
//! Group transform:
//!
//! - Translation tracks `MouseAttractorState.position`.
//! - Uniform scale = `sqrt(power) / 5`.
//! - Z position = `-1.0` so the gyroscope sits just behind the particles.
//! - Color = `#C5E2CC` at `opacity: 0.6` ≈ `Color::srgba(0.77, 0.886, 0.8, 0.6)`,
//!   matching v4's `MeshBasicMaterial({ transparent: true, opacity: 0.6 })`.
//!   `ColorMaterial::from(Color)` auto-sets `AlphaMode2d::Blend` when alpha <
//!   1.0, so the 10 stacked rings composite correctly.
//!
//! Geometry primitive is a smooth 32-segment ring (inner radius 15, outer 18,
//! matching v4's `RingGeometry(15, 18, 32)`); the visual divergence from v4
//! is in the per-ring transform behavior, not the underlying mesh.

use bevy::color::Color;
use bevy::prelude::*;
use bevy::sprite_render::ColorMaterial;
use wc_core::input::entity::TrackedHand;

use super::leap_attractors::LineHandAttractor;
use super::systems::MouseAttractorState;
use super::LineRoot;

/// Marker on the parent entity that owns all 10 ring children for an attractor.
///
/// The parent's `Transform` carries the attractor's world-space position and
/// the group scale (`sqrt(power) / 5`). Children inherit this transform via
/// Bevy's transform-propagation system.
#[derive(Component)]
pub struct AttractorVisual;

/// Which attractor a given [`AttractorVisual`] group is bound to.
///
/// v4 drew the ring gyroscope at *every* attractor; v5 now matches that by
/// spawning one visual per source — the singleton mouse/touch attractor plus
/// one per grabbing Leap hand — each tracking its own position + power. Keying
/// hand visuals by the `TrackedHand` entity lets two hands show two gyroscopes
/// and lets a visual despawn cleanly when its hand leaves the tracking volume.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub enum AttractorSource {
    /// The singleton mouse/touch attractor ([`MouseAttractorState`]).
    Mouse,
    /// A per-hand Leap attractor, keyed by its `TrackedHand` entity.
    Hand(Entity),
}

/// Marker on each individual ring child. Carries:
///
/// - [`Self::index`]: ring index `0..NUM_RINGS`. Determines per-ring base
///   scale (`1 + (i/10)² × 2`) and base rotation speed (`(10 - i)/20 × power`).
/// - [`Self::phi`]: accumulated per-frame rotation angle (radians).
/// - [`Self::gimbal`]: which axis this ring rotates around. `0 = X`, `1 = Y`,
///   `2 = Z`. Assigned at spawn from `index % GIMBAL_COUNT` (so the 10 rings
///   distribute 4/3/3 across the three axes).
#[derive(Component)]
pub struct AttractorRing {
    /// Ring index, `0..NUM_RINGS`. Set once at spawn.
    pub index: u32,
    /// Accumulated rotation angle (radians). Advances per frame by
    /// `(10 - index)/20 × GIMBAL_RATE[gimbal] × power`. Wraps naturally via
    /// `cos` / `Quat::from_rotation_z`.
    pub phi: f32,
    /// Gimbal axis assignment: `0 = X`, `1 = Y`, `2 = Z`.
    pub gimbal: u8,
}

/// v4 ring colour `#C5E2CC` at `opacity: 0.6` ≈ `Color::srgba(0.77, 0.886, 0.8, 0.6)`.
///
/// The alpha matches v4's `MeshBasicMaterial({ transparent: true, opacity: 0.6 })`.
/// `ColorMaterial::from(Color)` automatically sets `AlphaMode2d::Blend` when
/// alpha < 1.0, so the 10 stacked rings blend correctly without an explicit
/// `AlphaMode2d` override.
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

/// Per-ring scale curve denominator: `1 + (i / 10)² × 2`.
const RING_SCALE_INDEX_DIVISOR: f32 = 10.0;

/// Per-ring scale curve multiplier: `1 + (i / 10)² × 2`.
const RING_SCALE_MULTIPLIER: f32 = 2.0;

/// Rotation speed denominator: `speed = (10 - i) / 20 × gimbal_rate × power`.
const ROTATION_SPEED_DIVISOR: f32 = 20.0;

/// Z offset for the attractor visual parent — sits just behind particles
/// (which render at z=0) so the rings appear underneath the star sprites.
const VISUAL_Z: f32 = -1.0;

/// Power below which an attractor shows no ring visual. The mouse attractor is
/// zeroed exactly on release, but a hand attractor decays geometrically and
/// only approaches zero, so a small epsilon despawns its rings once the pull is
/// imperceptible (rather than leaving a vanishingly-faint gyroscope forever).
const VISUAL_POWER_EPSILON: f32 = 0.01;

/// Number of segments around each ring. 32 produces a smooth ring at the
/// scales used here (matches v4's `RingGeometry(15, 18, 32)`).
const RING_SEGMENTS: u32 = 32;

/// Number of distinct gimbal axes (X, Y, Z). Each ring is assigned one of
/// these by `index % GIMBAL_COUNT`.
const GIMBAL_COUNT: u32 = 3;

/// Per-gimbal rate multiplier on the base `(10 - i)/20 × power` rotation speed.
/// Different multipliers per axis desynchronise the gimbals so the three
/// nested rotations never lock into a periodic pattern — the resulting motion
/// reads as "gyroscope precessing" rather than "10 rings doing the same thing".
///
/// Index = gimbal axis. `[X, Y, Z]`.
const GIMBAL_RATE: [f32; 3] = [1.0, 0.73, 1.31];

/// Built-in y/x scale ratio for the gimbal-2 (Z-axis) rings. A pure circle
/// rotating around its own normal is invisible, so we squash these rings into
/// an ellipse — the rotation then reads clearly as the long axis sweeping
/// around. `0.55` is enough to make the spin obvious without making the ring
/// look broken.
const Z_GIMBAL_ELLIPSE_RATIO: f32 = 0.55;

/// Small per-ring phase offset (radians per ring index) applied at spawn so
/// the gimbal-0 and gimbal-1 rings within the same gimbal don't all hit
/// edge-on simultaneously. Multiplied by the ring's index to spread starting
/// phases across `[0, ~π)` over the 10 rings.
const PHI_PHASE_PER_INDEX: f32 = 0.31;

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

/// Compute the per-ring base scale (the unmodulated radius factor).
fn ring_base_scale(index_f: f32) -> f32 {
    1.0 + (index_f / RING_SCALE_INDEX_DIVISOR).powi(2) * RING_SCALE_MULTIPLIER
}

/// Apply a ring's gimbal-specific transform given its current `phi` and base
/// scale. Pure function so the spawn-time initial transform and the per-frame
/// animation share one source of truth.
fn ring_transform_for_gimbal(gimbal: u8, phi: f32, base: f32) -> Transform {
    match gimbal {
        // X-axis: vertical extent oscillates between 0 (edge-on) and base (face-on).
        0 => Transform::from_scale(Vec3::new(base, base * phi.cos().abs(), base)),
        // Y-axis: horizontal extent oscillates.
        1 => Transform::from_scale(Vec3::new(base * phi.cos().abs(), base, base)),
        // Z-axis: scaled to an ellipse, then rotated in-plane by phi.
        2 => Transform::from_scale(Vec3::new(base, base * Z_GIMBAL_ELLIPSE_RATIO, base))
            .with_rotation(Quat::from_rotation_z(phi)),
        // Unreachable in practice (gimbal is always set via `i % GIMBAL_COUNT`);
        // fall through to an unmodulated baseline scale so a future bug doesn't
        // produce a zero-scale ring that disappears silently.
        _ => Transform::from_scale(Vec3::splat(base)),
    }
}

/// World-space position + power for an [`AttractorSource`].
///
/// A `Hand` whose entity has gone (left the tracking volume) reports zero power
/// so its visual despawns on the next tick.
fn source_state(
    source: AttractorSource,
    mouse: &MouseAttractorState,
    hand_attractors: &Query<'_, '_, &LineHandAttractor>,
) -> (Vec2, f32) {
    match source {
        AttractorSource::Mouse => (Vec2::new(mouse.position[0], mouse.position[1]), mouse.power),
        AttractorSource::Hand(entity) => hand_attractors
            .get(entity)
            .map_or((Vec2::ZERO, 0.0), |attractor| {
                (attractor.position, attractor.power)
            }),
    }
}

/// Spawn the 10-ring gyroscope visual for every attractor that has power and
/// doesn't yet have a visual: the singleton mouse/touch attractor plus one per
/// grabbing Leap hand.
///
/// **Invariant:** a source is skipped if a visual already bound to it exists.
/// Without that guard this system would spawn 10 new ring entities every frame
/// the attractor is active, exhausting entity IDs and tanking the frame rate.
pub fn spawn_attractor_visual(
    mut commands: Commands<'_, '_>,
    mouse: Res<'_, MouseAttractorState>,
    hands: Query<'_, '_, (Entity, &LineHandAttractor), With<TrackedHand>>,
    existing: Query<'_, '_, &AttractorSource, With<AttractorVisual>>,
    mut meshes: ResMut<'_, Assets<Mesh>>,
    mut materials: ResMut<'_, Assets<ColorMaterial>>,
    line_root: Query<'_, '_, Entity, With<LineRoot>>,
) {
    let Some(root) = line_root.iter().next() else {
        // The sketch hasn't spawned yet (or has been torn down). Nothing to
        // parent the visual onto — try again next frame.
        return;
    };

    // Sources that already have a visual this frame, so we don't double-spawn.
    let mouse_has_visual = existing
        .iter()
        .any(|source| matches!(source, AttractorSource::Mouse));
    let hand_has_visual = |entity: Entity| {
        existing
            .iter()
            .any(|source| matches!(source, AttractorSource::Hand(e) if *e == entity))
    };

    if mouse.power > VISUAL_POWER_EPSILON && !mouse_has_visual {
        let pos = Vec2::new(mouse.position[0], mouse.position[1]);
        spawn_ring_group(
            &mut commands,
            root,
            AttractorSource::Mouse,
            pos,
            &mut meshes,
            &mut materials,
        );
    }

    for (entity, attractor) in &hands {
        if attractor.power > VISUAL_POWER_EPSILON && !hand_has_visual(entity) {
            spawn_ring_group(
                &mut commands,
                root,
                AttractorSource::Hand(entity),
                attractor.position,
                &mut meshes,
                &mut materials,
            );
        }
    }
}

/// Spawn one gyroscope group (parent + 10 ring children) for `source` at `pos`,
/// parented under the Line root. Shared by the mouse and per-hand spawn paths.
fn spawn_ring_group(
    commands: &mut Commands<'_, '_>,
    root: Entity,
    source: AttractorSource,
    pos: Vec2,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
) {
    // One mesh + one material shared across all 10 rings of this visual.
    // Per-ring `Transform` carries the index-dependent scale, gimbal axis,
    // and starting phase.
    let mesh_handle = meshes.add(build_polygonal_ring_mesh(
        RING_INNER_RADIUS,
        RING_OUTER_RADIUS,
        RING_SEGMENTS,
    ));
    let material_handle = materials.add(ColorMaterial::from(ATTRACTOR_RING_COLOR));

    let parent = commands
        .spawn((
            AttractorVisual,
            source,
            Transform::from_translation(Vec3::new(pos.x, pos.y, VISUAL_Z)),
            GlobalTransform::default(),
            Visibility::Visible,
        ))
        .id();
    commands.entity(root).add_child(parent);

    for i in 0..NUM_RINGS {
        #[allow(
            clippy::as_conversions,
            clippy::cast_precision_loss,
            reason = "i ∈ 0..NUM_RINGS=10; u32→f32 round-trip is lossless"
        )]
        let index_f = i as f32;
        let base = ring_base_scale(index_f);
        // gimbal = i % GIMBAL_COUNT. Cast through u8 because `i % 3` always
        // fits — clippy still demands explicit handling, so use try_from with
        // an unreachable fallback.
        let gimbal: u8 = u8::try_from(i % GIMBAL_COUNT).unwrap_or(0);
        // Deterministic per-ring phase stagger so same-gimbal rings don't all
        // reach edge-on / aligned simultaneously at spawn.
        let phi = index_f * PHI_PHASE_PER_INDEX;
        let initial = ring_transform_for_gimbal(gimbal, phi, base);
        let child = commands
            .spawn((
                AttractorRing {
                    index: i,
                    phi,
                    gimbal,
                },
                bevy::mesh::Mesh2d(mesh_handle.clone()),
                bevy::sprite_render::MeshMaterial2d(material_handle.clone()),
                initial,
                GlobalTransform::default(),
                Visibility::default(),
            ))
            .id();
        commands.entity(parent).add_child(child);
    }
}

/// Animate the gyroscope while attractor power is non-zero.
///
/// - Group translation tracks `MouseAttractorState.position`.
/// - Group scale tracks `sqrt(power) / 5` (uniform).
/// - Per-ring transform is recomputed from its (gimbal, phi, base scale)
///   triple via [`ring_transform_for_gimbal`]. `phi` advances per frame by
///   `(10 - index)/20 × GIMBAL_RATE[gimbal] × power`.
///
/// Per-frame (not per-second) rate is deliberate: it preserves the v4
/// reference rate at 60 FPS while remaining stable enough at higher refresh
/// rates that no visible artifacts appear. The trade-off is frame-rate
/// dependence (faster rotation at 120 FPS than at 60 FPS); this is acceptable
/// because the gyroscope reads as "spinning rapidly" regardless of the exact
/// rate, and the kiosk targets 60 FPS.
///
/// Despawn is handled by [`despawn_attractor_visual`] on the same tick the
/// power reaches zero; this system early-returns in that case so the rings
/// keep their final transform until they vanish.
#[allow(
    clippy::type_complexity,
    reason = "Bevy multi-component/multi-filter queries are inherently verbose; \
              factoring these into type aliases would obscure the system's data dependencies"
)]
pub fn animate_attractor_visual(
    mouse: Res<'_, MouseAttractorState>,
    hand_attractors: Query<'_, '_, &LineHandAttractor>,
    mut visuals: Query<
        '_,
        '_,
        (&AttractorSource, &mut Transform, &Children),
        (With<AttractorVisual>, Without<AttractorRing>),
    >,
    mut rings: Query<
        '_,
        '_,
        (&mut AttractorRing, &mut Transform),
        (With<AttractorRing>, Without<AttractorVisual>),
    >,
) {
    for (source, mut group_t, children) in &mut visuals {
        let (pos, power) = source_state(*source, &mouse, &hand_attractors);
        if power <= VISUAL_POWER_EPSILON {
            // Source is gone / released; `despawn_attractor_visual` removes this
            // group on the same tick. Leave the rings at their last transform.
            continue;
        }

        group_t.translation.x = pos.x;
        group_t.translation.y = pos.y;
        // Z stays at VISUAL_Z (set at spawn); only XY tracks the attractor.
        group_t.scale = Vec3::splat(power.sqrt() / GROUP_SCALE_DIVISOR);

        for child in children.iter() {
            let Ok((mut ring, mut ring_t)) = rings.get_mut(child) else {
                continue;
            };
            #[allow(
                clippy::as_conversions,
                clippy::cast_precision_loss,
                reason = "ring.index ∈ 0..NUM_RINGS=10; u32→f32 round-trip is lossless"
            )]
            let index_f = ring.index as f32;
            // Per-gimbal rate multiplier desynchronises the three axes so the
            // rings never lock into a periodic group pattern.
            let gimbal_idx = usize::from(ring.gimbal.min(2));
            let gimbal_rate = GIMBAL_RATE[gimbal_idx];
            let speed = (10.0 - index_f) / ROTATION_SPEED_DIVISOR * gimbal_rate * power;
            ring.phi += speed;

            let base = ring_base_scale(index_f);
            *ring_t = ring_transform_for_gimbal(ring.gimbal, ring.phi, base);
        }
    }
}

/// Despawn each gyroscope visual once its bound attractor's power drops back to
/// (near) zero — the mouse released, a hand stopped grabbing, or a hand left the
/// tracking volume (its `LineHandAttractor` lookup fails → zero power).
///
/// Bevy 0.18's `EntityCommands::despawn()` recursively despawns descendants
/// linked through the `ChildOf` relationship, so a single `despawn()` on the
/// `AttractorVisual` parent removes the 10 ring children too.
pub fn despawn_attractor_visual(
    mut commands: Commands<'_, '_>,
    mouse: Res<'_, MouseAttractorState>,
    hand_attractors: Query<'_, '_, &LineHandAttractor>,
    visuals: Query<'_, '_, (Entity, &AttractorSource), With<AttractorVisual>>,
) {
    for (entity, source) in &visuals {
        let (_pos, power) = source_state(*source, &mouse, &hand_attractors);
        if power <= VISUAL_POWER_EPSILON {
            commands.entity(entity).despawn();
        }
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

    /// Gimbal 0 (X-axis): at phi=0 the ring is face-on; scale.y should equal
    /// the base scale unmodulated.
    #[test]
    fn gimbal_x_face_on_at_phi_zero() {
        let t = ring_transform_for_gimbal(0, 0.0, 1.0);
        assert!((t.scale.y - 1.0).abs() < 1e-6);
        assert!((t.scale.x - 1.0).abs() < 1e-6);
    }

    /// Gimbal 0 (X-axis): at phi=π/2 the ring is edge-on; scale.y should
    /// collapse to (near) zero.
    #[test]
    fn gimbal_x_edge_on_at_phi_half_pi() {
        let t = ring_transform_for_gimbal(0, std::f32::consts::FRAC_PI_2, 1.0);
        assert!(
            t.scale.y.abs() < 1e-6,
            "scale.y should be ≈0; got {}",
            t.scale.y
        );
        assert!((t.scale.x - 1.0).abs() < 1e-6);
    }

    /// Gimbal 1 (Y-axis): at phi=π/2 the ring is edge-on; scale.x should
    /// collapse to (near) zero.
    #[test]
    fn gimbal_y_edge_on_at_phi_half_pi() {
        let t = ring_transform_for_gimbal(1, std::f32::consts::FRAC_PI_2, 1.0);
        assert!(
            t.scale.x.abs() < 1e-6,
            "scale.x should be ≈0; got {}",
            t.scale.x
        );
        assert!((t.scale.y - 1.0).abs() < 1e-6);
    }

    /// Gimbal 2 (Z-axis): scale stays at the elliptical baseline; rotation
    /// advances with phi.
    #[test]
    fn gimbal_z_rotates_in_plane() {
        let t = ring_transform_for_gimbal(2, std::f32::consts::FRAC_PI_4, 1.0);
        assert!((t.scale.x - 1.0).abs() < 1e-6);
        assert!((t.scale.y - Z_GIMBAL_ELLIPSE_RATIO).abs() < 1e-6);
        // Rotation should match the supplied phi.
        let expected = Quat::from_rotation_z(std::f32::consts::FRAC_PI_4);
        assert!(t.rotation.abs_diff_eq(expected, 1e-6));
    }
}
