//! `OnEnter(AppState::Dots)` spawn system.
//!
//! Allocates the particle storage buffer with a full-screen grid layout,
//! builds a flat quad mesh (`count × 6` vertices for the vertex-index-driven
//! render shader), spawns the render entity under [`DotsRoot`], inserts
//! [`crate::particles::compute::ParticleSimParams`] for the render world, and
//! seeds the CPU mirror with the same particle state.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "u32 ↔ usize ↔ f32 casts for particle count and mesh vertex sizing are intentional; \
              f32-to-usize casts for grid dimensions are always non-negative (spacing > 0, range > 0)"
)]

use bevy::asset::RenderAssetUsages;
use bevy::mesh::PrimitiveTopology;
use bevy::prelude::*;
use bevy::render::storage::ShaderBuffer;
use bytemuck::cast_slice;

use crate::dots::settings::DotsSettings;
use crate::particles::compute::ParticleSimParams;
use crate::particles::material::ParticleMaterial;
use crate::particles::particle::{Particle, SimParams};
use crate::particles::sim_cpu::CpuMirror;

/// Grid-bleed cell count: extra grid cells beyond the canvas edge on each side.
///
/// Matches v4 `EXTENT = 10` — ensures no gap between the window border
/// and the grid when particles spring back from off-screen positions.
const EXTENT: i32 = 10;

/// Packed white spawn colour: `0x00FFFFFF` bit-cast to `f32`.
///
/// The render shader recovers it via `bitcast<u32>` as RGB8 (`r = bits >> 16`
/// etc.). White (`0x00FFFFFF`) means no colour tint. This is the same bit
/// pattern that `crate::line::template_adjustments::pack_rgb8([255, 255, 255])`
/// produces; Line's helper is Line-private so we reproduce the bit pattern
/// directly here.
///
/// D6 will seed the attract-mode fields (`age`, `lifespan`, `spawn_hash`,
/// `spawn_color`) with live values; for D2 we use inert defaults.
const SPAWN_COLOR_WHITE: f32 = f32::from_bits(0x00FF_FFFF);

/// Marker component placed on every entity owned by the Dots sketch.
///
/// `OnExit(AppState::Dots)` despawns everything tagged with this marker
/// via [`wc_core::sketch::despawn_with`].
#[derive(Component)]
pub struct DotsRoot;

/// Build the flat grid of [`Particle`]s for a canvas of `(w × h)` pixels
/// with the given `spacing`.
///
/// The grid spans `x ∈ [-EXTENT·spacing, w + EXTENT·spacing)` (exclusive end,
/// step `spacing`) and the same range for `y`, matching v4 Dots
/// `for x from -EXTENT*spacing to width + EXTENT*spacing step spacing`.
/// Each point is converted to centered world space: subtract `half_w` / `half_h`,
/// flip y to +up. Particles are seeded with zero velocity and `alpha = 0`
/// (fade-in governed by `SimParams.fade_duration`).
///
/// Attract-mode fields are inert defaults: `age = 0`, `lifespan = 0`,
/// `spawn_hash = 0`, `spawn_color = white`. D6 seeds them with live values.
///
/// The returned `Vec` length is clamped to `[100, 200_000]`.
#[must_use]
pub fn build_grid_particles(w: f32, h: f32, spacing: f32) -> Vec<Particle> {
    let half_w = w * 0.5;
    let half_h = h * 0.5;

    // Guard against degenerate spacing (DotsSettings::dot_spacing min is 4 px;
    // this floor protects direct test calls with arbitrary inputs).
    let spacing = spacing.max(1.0);

    // v4 window-space grid bounds.
    let x_start = -(EXTENT as f32) * spacing;
    let x_end = w + (EXTENT as f32) * spacing;
    let y_start = -(EXTENT as f32) * spacing;
    let y_end = h + (EXTENT as f32) * spacing;

    // Number of columns / rows (exclusive upper bound, matching v4's `<`).
    let cols = ((x_end - x_start) / spacing).ceil() as usize;
    let rows = ((y_end - y_start) / spacing).ceil() as usize;

    // Clamp particle count. Dense grids on wide canvases (e.g. spacing=4 at 4K)
    // can exceed 200 k; the upper bound is the live cap. The lower bound of 100
    // is currently unreachable (with EXTENT=10 the grid is always ≥ ~400 cells)
    // and is a `with_capacity` floor, NOT a fill guarantee — the loop only pushes
    // `cols*rows` particles, so if EXTENT ever shrank below the floor, the
    // returned Vec could be shorter than 100.
    let count = cols.saturating_mul(rows).clamp(100, 200_000);

    let mut particles = Vec::with_capacity(count);

    // Outer loop: columns (x), inner: rows (y) — matching v4's for-x / for-y
    // nesting so index → screen position is reproducible.
    'outer: for col in 0..cols {
        let wx = x_start + col as f32 * spacing;
        // Convert window-space x to centered world space.
        let world_x = wx - half_w;

        for row in 0..rows {
            if particles.len() >= count {
                break 'outer;
            }
            let wy = y_start + row as f32 * spacing;
            // Flip y to +up (world origin at center, window origin at top-left).
            let world_y = -(wy - half_h);

            // D2 inert defaults for attract-mode fields. D6 will seed
            // age/lifespan/spawn_hash with per-particle hashed values and
            // spawn_color from the palette.
            particles.push(Particle {
                position: [world_x, world_y],
                velocity: [0.0, 0.0],
                original_xy: [world_x, world_y],
                alpha: 0.0,
                age: 0.0,
                lifespan: 0.0,
                spawn_hash: 0.0,
                spawn_color: SPAWN_COLOR_WHITE,
                _pad: 0.0,
            });
        }
    }

    particles
}

/// `OnEnter(AppState::Dots)`.
///
/// Allocates the particle storage buffer (full-screen grid), builds a flat
/// quad mesh (`count × 6` vertices for the vertex-index-driven render shader),
/// spawns the render entity under [`DotsRoot`], inserts [`ParticleSimParams`]
/// for the render world to extract each frame, and seeds the CPU mirror with
/// the same particle state.
///
/// Grid layout: [`DotsSettings::dot_spacing`] controls cell pitch; the grid
/// bleeds `EXTENT = 10` cells beyond each canvas edge. Particle count is
/// clamped to `[100, 200_000]`.
pub fn spawn_dots(
    mut commands: Commands<'_, '_>,
    settings: Res<'_, DotsSettings>,
    window: Single<'_, '_, &Window>,
    asset_server: Res<'_, AssetServer>,
    mut buffers: ResMut<'_, Assets<ShaderBuffer>>,
    mut materials: ResMut<'_, Assets<ParticleMaterial>>,
    mut meshes: ResMut<'_, Assets<Mesh>>,
) {
    let w = window.width();
    let h = window.height();

    let initial = build_grid_particles(w, h, settings.dot_spacing);

    #[allow(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "count is clamped to [100, 200_000] — fits safely in u32"
    )]
    let count = initial.len() as u32;

    // Upload particle data to a GPU storage buffer.
    let particle_bytes = cast_slice::<Particle, u8>(&initial);
    let particles_handle = buffers.add(ShaderBuffer::new(
        particle_bytes,
        RenderAssetUsages::RENDER_WORLD,
    ));

    // Star sprite shared with Line (no Dots-specific sprite in D2).
    let star_texture: Handle<Image> = asset_server.load("sketches/line/star.png");

    let material_handle = materials.add(ParticleMaterial {
        particles: particles_handle.clone(),
        star_texture,
        // All four feature uniforms at their off-sentinels at spawn.
        solid_color: ParticleMaterial::solid_off(),
        attract_color: ParticleMaterial::attract_color_off(),
        template_color: ParticleMaterial::template_color_off(),
        palette_params: ParticleMaterial::palette_off(),
    });

    // Flat mesh: count * 6 dummy vertices. The vertex shader derives particle
    // position and quad corner from @builtin(vertex_index) — the mesh data is
    // unused beyond triggering the draw call.
    let vertex_count = count as usize * 6;
    let positions: Vec<[f32; 3]> = vec![[0.0, 0.0, 0.0]; vertex_count];
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    let mesh_handle = meshes.add(mesh);

    commands.spawn((
        DotsRoot,
        bevy::mesh::Mesh2d(mesh_handle),
        bevy::sprite_render::MeshMaterial2d(material_handle),
        Transform::default(),
        GlobalTransform::default(),
        Visibility::default(),
    ));

    // Seed CPU mirror — one-shot allocation at sketch entry, not per-frame.
    // `initial.clone()` is intentional here (mirrors Line's pattern).
    commands.insert_resource(CpuMirror {
        particles: initial.clone(),
    });

    // Install ParticleSimParams — the render world extracts this each frame.
    commands.insert_resource(ParticleSimParams {
        params: SimParams::default(),
        particles_handle,
        particle_count: count,
    });

    tracing::info!(count, "spawned Dots sketch (grid)");
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_W: f32 = 1280.0;
    const TEST_H: f32 = 720.0;
    const TEST_SPACING: f32 = 20.0;

    #[test]
    fn grid_produces_nonzero_count() {
        let particles = build_grid_particles(TEST_W, TEST_H, TEST_SPACING);
        assert!(
            !particles.is_empty(),
            "grid must produce at least one particle"
        );
        assert!(
            particles.len() <= 200_000,
            "count {} must not exceed clamp upper bound",
            particles.len()
        );
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "original_xy and position are both set to the same computed value — bit-identical"
    )]
    fn original_xy_matches_position_for_every_particle() {
        let particles = build_grid_particles(TEST_W, TEST_H, TEST_SPACING);
        for (i, p) in particles.iter().enumerate() {
            assert_eq!(
                p.original_xy, p.position,
                "particle {i}: original_xy {:?} != position {:?}",
                p.original_xy, p.position
            );
        }
    }

    #[test]
    fn grid_extent_covers_bleed_range() {
        let spacing = TEST_SPACING;
        let particles = build_grid_particles(TEST_W, TEST_H, spacing);
        let half_w = TEST_W * 0.5;
        let half_h = TEST_H * 0.5;

        // Expected world-space x bounds.
        // Window x_start = -EXTENT*spacing  →  world_x = x_start - half_w
        let expected_x_min = -(EXTENT as f32) * spacing - half_w;
        // Window x_end (exclusive) = W + EXTENT*spacing; last grid point is one
        // spacing before it → world_x = (x_end - spacing) - half_w
        let expected_x_max = TEST_W + (EXTENT as f32) * spacing - half_w;

        let min_x = particles
            .iter()
            .map(|p| p.position[0])
            .fold(f32::MAX, f32::min);
        let max_x = particles
            .iter()
            .map(|p| p.position[0])
            .fold(f32::MIN, f32::max);

        // The leftmost particle sits exactly at the left bleed edge.
        assert!(
            min_x <= expected_x_min + spacing,
            "min x {min_x} should reach the left bleed bound {expected_x_min}"
        );
        // The rightmost particle is at most one spacing inside the exclusive bound.
        assert!(
            max_x >= expected_x_max - spacing,
            "max x {max_x} should reach within one spacing of the right bleed bound {expected_x_max}"
        );

        // y is flipped: positive world y is up, negative is down.
        let min_y = particles
            .iter()
            .map(|p| p.position[1])
            .fold(f32::MAX, f32::min);
        let max_y = particles
            .iter()
            .map(|p| p.position[1])
            .fold(f32::MIN, f32::max);
        let bleed = (EXTENT as f32) * spacing;
        assert!(
            min_y <= -half_h - bleed + spacing,
            "min y {min_y} should reach the bottom bleed"
        );
        assert!(
            max_y >= half_h + bleed - spacing,
            "max y {max_y} should reach the top bleed"
        );
    }

    #[test]
    fn spawn_color_is_white_for_every_particle() {
        let particles = build_grid_particles(200.0, 100.0, 10.0);
        for (i, p) in particles.iter().enumerate() {
            assert_eq!(
                p.spawn_color.to_bits(),
                SPAWN_COLOR_WHITE.to_bits(),
                "particle {i} must start with white spawn_color"
            );
        }
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "velocity and alpha are written as literal 0.0 — exact zero, bit-identical"
    )]
    fn velocity_and_alpha_are_zero_at_spawn() {
        let particles = build_grid_particles(200.0, 100.0, 10.0);
        for (i, p) in particles.iter().enumerate() {
            assert_eq!(p.velocity, [0.0, 0.0], "particle {i} velocity must be zero");
            assert_eq!(p.alpha, 0.0, "particle {i} alpha must be zero");
        }
    }

    #[test]
    fn dense_grid_clamps_to_maximum_count() {
        // Extremely small spacing on a normal window produces a very dense grid
        // (>> 200_000 cells). The upper clamp caps output at exactly 200_000.
        let particles = build_grid_particles(1280.0, 720.0, 1.0);
        assert_eq!(
            particles.len(),
            200_000,
            "dense grid should clamp to 200_000"
        );
    }
}
