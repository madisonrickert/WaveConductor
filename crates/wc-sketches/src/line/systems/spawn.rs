//! `OnEnter(AppState::Line)` spawn system.
//!
//! Allocates the particle storage buffer with the initial grid layout, builds
//! a flat quad mesh (`particle_count × 6` vertices for the vertex-index-driven
//! render shader), spawns the render entity under [`LineRoot`], inserts
//! [`crate::line::compute::LineSimParams`] for the render world, and seeds the
//! CPU mirror with the same particle state for Plan 9's `ParticleStats`.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "u32 ↔ usize ↔ f32 casts for particle count and mesh vertex sizing are intentional"
)]

use bevy::asset::RenderAssetUsages;
use bevy::mesh::PrimitiveTopology;
use bevy::prelude::*;
use bevy::render::storage::ShaderStorageBuffer;
use bytemuck::cast_slice;

use crate::line::compute::LineSimParams;
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
/// Allocates the particle storage buffer with an initial grid layout,
/// constructs a flat quad mesh (`particle_count × 6` vertices for the
/// vertex-index-driven render shader), spawns the render entity under
/// [`LineRoot`], inserts [`LineSimParams`] for the render world to extract
/// each frame, and seeds the CPU mirror with the same particle state.
pub fn spawn_line(
    mut commands: Commands<'_, '_>,
    settings: Res<'_, LineSettings>,
    mut buffers: ResMut<'_, Assets<ShaderStorageBuffer>>,
    mut materials: ResMut<'_, Assets<LineMaterial>>,
    mut meshes: ResMut<'_, Assets<Mesh>>,
) {
    let count = settings.particle_count.max(1);

    // Initial state: particles arranged in a square grid centered on origin.
    // sqrt().ceil() of a positive number is always non-negative; cast is safe.
    #[allow(
        clippy::cast_sign_loss,
        reason = "sqrt+ceil of positive u32→f32 is always ≥0"
    )]
    let side = (count as f32).sqrt().ceil() as u32;
    let spacing = 4.0_f32;
    let mut initial: Vec<Particle> = Vec::with_capacity(count as usize);
    for i in 0..count {
        let x = (i % side) as f32 - (side as f32 * 0.5);
        let y = (i / side) as f32 - (side as f32 * 0.5);
        initial.push(Particle {
            position: [x * spacing, y * spacing],
            velocity: [0.0, 0.0],
            original_xy: [x * spacing, y * spacing],
            alpha: 0.0,
            _pad: 0.0,
        });
    }

    // Upload particle data to a GPU storage buffer.
    // `ShaderStorageBuffer::new` takes raw bytes + usage flags.
    let particle_bytes = cast_slice::<Particle, u8>(&initial);
    let particles_handle = buffers.add(ShaderStorageBuffer::new(
        particle_bytes,
        RenderAssetUsages::RENDER_WORLD,
    ));

    let material_handle = materials.add(LineMaterial {
        particles: particles_handle.clone(),
    });

    // Build a flat mesh with particle_count * 6 vertices (all at origin).
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
