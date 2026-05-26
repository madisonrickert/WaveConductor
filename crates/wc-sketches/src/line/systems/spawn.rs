//! `OnEnter(AppState::Line)` spawn system.
//!
//! Allocates the particle storage buffer with the initial particle layout
//! (horizontal-line default; heatmap-image sampler when
//! [`crate::line::settings::LineSettings::spawn_template`] is non-empty),
//! builds a flat quad mesh (`count × 6` vertices for the vertex-index-driven
//! render shader), spawns the render entity under [`LineRoot`], inserts
//! [`crate::line::compute::LineSimParams`] for the render world, and seeds
//! the CPU mirror with the same particle state for Plan 9's `ParticleStats`.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "u32 ↔ usize ↔ f32 casts for particle count and mesh vertex sizing are intentional"
)]

use std::path::Path;

use bevy::asset::RenderAssetUsages;
use bevy::image::Image;
use bevy::mesh::PrimitiveTopology;
use bevy::prelude::*;
use bevy::render::storage::ShaderStorageBuffer;
use bytemuck::cast_slice;

use crate::line::compute::LineSimParams;
use crate::line::heatmap::sample_from_heatmap;
use crate::line::material::LineMaterial;
use crate::line::particle::{Particle, SimParams};
use crate::line::settings::LineSettings;
use crate::line::sim_cpu::LineCpuMirror;

/// Marker component placed on every entity owned by the Line sketch.
///
/// `OnExit(AppState::Line)` despawns everything tagged with this marker
/// via [`wc_core::sketch::despawn_with`].
#[derive(Component)]
pub struct LineRoot;

/// `OnEnter(AppState::Line)`.
///
/// Allocates the particle storage buffer, constructs a flat quad mesh
/// (`count × 6` vertices for the vertex-index-driven render shader), spawns
/// the render entity under [`LineRoot`], inserts [`LineSimParams`] for the
/// render world to extract each frame, and seeds the CPU mirror with the
/// same particle state.
///
/// Particle layout depends on [`LineSettings::spawn_template`]:
///
/// - Empty (default) — horizontal-line layout at mid-Y with five-strand
///   sawtooth Y-jitter (the v5-line-sim baseline).
/// - Non-empty — PNG path passed to [`sample_from_heatmap`]; particle
///   density follows the image's luminance × alpha. A missing or
///   undecodable file falls back to the horizontal-line layout.
///
/// The particle count is derived from `settings.particle_density × window.width`
/// (v4 parity: `particleDensity = 10` per canvas-pixel of width yields ~12,800
/// particles at 1280px), clamped to `[100, 100_000]` so a sudden resize spike
/// does not catastrophically allocate.
pub fn spawn_line(
    mut commands: Commands<'_, '_>,
    settings: Res<'_, LineSettings>,
    window: Single<'_, '_, &Window>,
    asset_server: Res<'_, AssetServer>,
    mut buffers: ResMut<'_, Assets<ShaderStorageBuffer>>,
    mut materials: ResMut<'_, Assets<LineMaterial>>,
    mut meshes: ResMut<'_, Assets<Mesh>>,
) {
    let w = window.width();
    let win_h = window.height();
    let half_w = w * 0.5;
    let half_h = win_h * 0.5;
    // TODO(plan-12+): if a sketch needs the Line camera off-center, promote
    // mid_y to a setting.
    let mid_y = 0.0_f32; // window-centered world

    // v4 particleDensity = 10 per canvas-pixel of width. Derive count from
    // density × width, clamping to a sane range (avoids massive resize spikes).
    #[allow(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "density × width is positive and bounded by clamp"
    )]
    let count = ((settings.particle_density * w).round() as u32).clamp(100, 100_000);

    // Build particle positions: heatmap sampler if `spawn_template` is set,
    // else the default horizontal-line layout. The heatmap sampler returns
    // window-space (top-left origin, +y down); we convert to world-space
    // (centered, +y up) during Particle construction below.
    let initial: Vec<Particle> = if settings.spawn_template.is_empty() {
        let mut v: Vec<Particle> = Vec::with_capacity(count as usize);
        for i in 0..count {
            // Evenly space across the window width, centered on origin.
            let x = (i as f32 / count as f32) * w - half_w;
            // v4: subtle sawtooth Y-jitter `((i % 5) - 2) * 2` so particles sit on
            // five stacked horizontal strands rather than a single line.
            let jitter_strand = (i % 5) as f32 - 2.0;
            let y = mid_y + jitter_strand * 2.0;
            v.push(Particle {
                position: [x, y],
                velocity: [0.0, 0.0],
                original_xy: [x, y],
                alpha: 0.0,
                _pad: 0.0,
            });
        }
        v
    } else {
        // Window-space positions from the heatmap sampler.
        let path = Path::new(&settings.spawn_template);
        let positions = sample_from_heatmap(path, w, win_h, count as usize);
        positions
            .into_iter()
            .map(|win_pos| {
                // Convert window-space (top-left origin, +y down) to centered
                // world-space (+y up) — the coordinate system the rest of the
                // sketch uses.
                let x = win_pos.x - half_w;
                let y = -(win_pos.y - half_h);
                Particle {
                    position: [x, y],
                    velocity: [0.0, 0.0],
                    original_xy: [x, y],
                    alpha: 0.0,
                    _pad: 0.0,
                }
            })
            .collect()
    };

    // Upload particle data to a GPU storage buffer.
    // `ShaderStorageBuffer::new` takes raw bytes + usage flags.
    let particle_bytes = cast_slice::<Particle, u8>(&initial);
    let particles_handle = buffers.add(ShaderStorageBuffer::new(
        particle_bytes,
        RenderAssetUsages::RENDER_WORLD,
    ));

    // Star sprite for the particle quads (ported from v4's
    // `src/materials/starMaterial/star.png`). Loaded via `AssetServer` so
    // Bevy's image loader (`ImagePlugin`, included in `DefaultPlugins`)
    // decodes the PNG into a GPU texture asynchronously; the bind group
    // becomes valid once the asset finishes loading.
    let star_texture: Handle<Image> = asset_server.load("sketches/line/star.png");

    let material_handle = materials.add(LineMaterial {
        particles: particles_handle.clone(),
        star_texture,
    });

    // Build a flat mesh with `count * 6` vertices (all at origin).
    // The vertex shader derives particle position + quad corner from
    // @builtin(vertex_index), so the mesh only needs to exist to trigger
    // the draw call — its vertex data is unused.
    let vertex_count = count as usize * 6;
    let positions: Vec<[f32; 3]> = vec![[0.0, 0.0, 0.0]; vertex_count];
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);

    let mesh_handle = meshes.add(mesh);

    commands.spawn((
        LineRoot,
        bevy::mesh::Mesh2d(mesh_handle),
        bevy::sprite_render::MeshMaterial2d(material_handle),
        Transform::default(),
        GlobalTransform::default(),
        Visibility::default(),
    ));

    // Seed the CPU mirror with the same particle state. The `clone()` here is a
    // one-shot allocation at sketch entry — not per-frame — so the
    // no-allocations-in-hot-paths rule still holds.
    commands.insert_resource(LineCpuMirror {
        particles: initial.clone(),
    });

    // Install LineSimParams — the render world extracts this each frame.
    commands.insert_resource(LineSimParams {
        params: SimParams::default(),
        particles_handle,
        particle_count: count,
    });

    tracing::info!(count, "spawned Line sketch");
}
