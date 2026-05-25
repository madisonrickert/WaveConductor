//! Line sketch main-world update systems.
//!
//! - [`spawn_line`] runs on `OnEnter(AppState::Line)`: allocates the particle
//!   storage buffer, builds the quad mesh, spawns the render entity under
//!   [`LineRoot`], and installs [`LineSimParams`] for the render world.
//! - [`update_sim_params`] runs every `Update` while the sketch is active:
//!   pushes the current pointer position + `LineSettings` values into
//!   [`LineSimParams`] so the render world extracts them each frame.

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
use wc_core::input::pointer::PointerState;

use super::compute::LineSimParams;
use super::material::LineMaterial;
use super::particle::{Particle, SimParams};
use super::settings::LineSettings;

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
/// [`LineRoot`], and inserts [`LineSimParams`] for the render world to
/// extract each frame.
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

    // Install LineSimParams — the render world extracts this each frame.
    commands.insert_resource(LineSimParams {
        params: SimParams::default(),
        particles_handle,
        particle_count: count,
    });

    tracing::info!(count, "spawned Line sketch");
}

/// `Update` — gated by `sketch_active(AppState::Line)`.
///
/// Pushes the current pointer position and `LineSettings` constants into
/// [`LineSimParams`]. The render world extracts this resource each frame so
/// the compute shader sees up-to-date values.
///
/// # Pointer mapping
///
/// [`PointerState::primary`] is in window logical coordinates (top-left
/// origin, +y down). The sketch world is centered with +y up, so we
/// invert Y when the window size is available. When no pointer is active
/// (`primary.is_none()`), the attractor is disabled (`attractor_enabled = 0.0`).
pub fn update_sim_params(
    time: Res<'_, Time>,
    settings: Res<'_, LineSettings>,
    pointer: Res<'_, PointerState>,
    windows: Query<'_, '_, &Window>,
    mut sim: ResMut<'_, LineSimParams>,
    mut diag_timer: Local<'_, f32>,
) {
    let (attractor_pos, attractor_enabled) = match pointer.primary {
        Some(cursor_window) => {
            // Convert window coords (top-left +y-down) to world coords (center +y-up).
            let (cx, cy) = if let Some(window) = windows.iter().next() {
                let w = window.width();
                let h = window.height();
                let wx = cursor_window.x - w * 0.5;
                let wy = -(cursor_window.y - h * 0.5);
                (wx, wy)
            } else {
                (cursor_window.x, cursor_window.y)
            };
            ([cx, cy], 1.0_f32)
        }
        None => ([0.0_f32, 0.0_f32], 0.0_f32),
    };

    sim.params = SimParams {
        // Cap dt to 50 ms to avoid velocity blow-up after pauses or tab switches.
        dt: time.delta_secs().min(0.05),
        drag: settings.drag,
        attractor_pos,
        attractor_radius: settings.attractor_radius,
        gravity_constant: settings.gravity_constant,
        attractor_enabled,
        _pad: 0.0,
    };

    // Diagnostic: once per second, log the pointer state and computed
    // attractor params so failures are observable in the console. Remove
    // when visual tuning is locked in (tracked as a Plan 7 carry-forward).
    *diag_timer += time.delta_secs();
    if *diag_timer >= 1.0 {
        *diag_timer = 0.0;
        tracing::info!(
            pointer_primary = ?pointer.primary,
            pointer_source = ?pointer.source,
            attractor_pos = ?attractor_pos,
            attractor_enabled,
            "line sim params (1Hz)"
        );
    }
}
