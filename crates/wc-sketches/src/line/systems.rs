//! Line sketch main-world update systems.
//!
//! - [`spawn_line`] runs on `OnEnter(AppState::Line)`: allocates the particle
//!   storage buffer, builds the quad mesh, spawns the render entity under
//!   [`LineRoot`], and installs [`LineSimParams`] for the render world.
//! - [`update_mouse_attractor`] tracks pointer button transitions and updates
//!   [`MouseAttractorState`] (power = 10 on press, position follows the cursor).
//! - [`decay_mouse_attractor`] decays the mouse attractor power geometrically
//!   each frame so the pull fades smoothly after release.
//! - [`update_sim_params`] runs every `Update` while the sketch is active:
//!   pushes the attractor array, drag constants, and constrain-to-box bounds
//!   into [`LineSimParams`] so the render world extracts them each frame.

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
use super::particle::{Attractor, Particle, SimParams, MAX_ATTRACTORS};
use super::settings::LineSettings;

/// Marker component placed on every entity owned by the Line sketch.
///
/// `OnExit(AppState::Line)` despawns everything tagged with this marker
/// via [`wc_core::sketch::despawn_with`].
#[derive(Component)]
pub struct LineRoot;

/// Lifecycle state for the mouse attractor — power that activates on click and
/// decays geometrically while held or after release. Matches v4's behavior:
/// `power=10` on press; each frame `power = floor + (power - floor) * 0.9`
/// down to `power < floor + epsilon`, then zero.
#[derive(Resource, Debug, Clone, Copy)]
pub struct MouseAttractorState {
    /// Current power. `0.0` = inactive.
    pub power: f32,
    /// World-space position (followed every frame the cursor moves).
    pub position: [f32; 2],
}

impl Default for MouseAttractorState {
    fn default() -> Self {
        Self {
            power: 0.0,
            position: [0.0, 0.0],
        }
    }
}

/// v4 `MOUSE_ATTRACTOR_POWER_DECAY_SPEED = 0.9`.
pub const MOUSE_POWER_DECAY: f32 = 0.9;
/// v4 `MOUSE_ATTRACTOR_POWER_DECAY_FLOOR = 2.0`. Power below `floor + ε` zeros.
pub const MOUSE_POWER_FLOOR: f32 = 2.0;
/// v4 `enableMouseAttractor`: `power = 10` on click.
pub const MOUSE_POWER_PRESS: f32 = 10.0;

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

/// Tracks pointer button transitions and updates [`MouseAttractorState`].
///
/// - Just-pressed → set `power = MOUSE_POWER_PRESS`, position = cursor.
/// - Held / moving → update position only.
/// - Released → start decay (handled in [`decay_mouse_attractor`]).
pub fn update_mouse_attractor(
    pointer: Res<'_, PointerState>,
    mouse_buttons: Res<'_, bevy::input::ButtonInput<bevy::input::mouse::MouseButton>>,
    touches: Res<'_, bevy::input::touch::Touches>,
    window: Single<'_, '_, &Window>,
    mut state: ResMut<'_, MouseAttractorState>,
) {
    let just_pressed = mouse_buttons.just_pressed(bevy::input::mouse::MouseButton::Left)
        || touches.iter_just_pressed().next().is_some();
    let held = mouse_buttons.pressed(bevy::input::mouse::MouseButton::Left)
        || touches.iter().next().is_some();

    if let Some(cursor_window) = pointer.primary {
        let w = window.width();
        let h = window.height();
        let wx = cursor_window.x - w * 0.5;
        let wy = -(cursor_window.y - h * 0.5);
        state.position = [wx, wy];

        if just_pressed {
            state.power = MOUSE_POWER_PRESS;
        } else if held && state.power < MOUSE_POWER_PRESS {
            // Keep power topped up while holding (matches v4's setGravityFocalPoint
            // running every mousemove that re-asserts the attractor).
            state.power = MOUSE_POWER_PRESS;
        }
    }
}

/// Decays the mouse attractor power each frame regardless of input state.
///
/// v4 runs this in the sketch's `animate()` regardless of idle state, so the
/// attractor's visual decay completes even after the user has stopped
/// interacting. Plan 8 will add the visual mesh; here only the physical power
/// matters.
pub fn decay_mouse_attractor(mut state: ResMut<'_, MouseAttractorState>) {
    if state.power <= 0.0 {
        return;
    }
    state.power = MOUSE_POWER_FLOOR + (state.power - MOUSE_POWER_FLOOR) * MOUSE_POWER_DECAY;
    if state.power < MOUSE_POWER_FLOOR + 1e-2 {
        state.power = 0.0;
    }
}

/// `Update` — gated by `sketch_active(AppState::Line)`.
///
/// Populates the attractor array (mouse at index 0 when active), bakes the
/// pulling/inertial drag constants against the v4 fixed-dt, derives the
/// size-scaled gravity multiplier from the window width, and writes the
/// constrain-to-box bounds. The render world extracts [`LineSimParams`] each
/// frame so the compute shader sees up-to-date values.
pub fn update_sim_params(
    time: Res<'_, Time>,
    settings: Res<'_, LineSettings>,
    window: Single<'_, '_, &Window>,
    mouse: Res<'_, MouseAttractorState>,
    mut sim: ResMut<'_, LineSimParams>,
) {
    // --- Attractor list -------------------------------------------------
    let mut attractors = [Attractor::default(); MAX_ATTRACTORS];
    let mut attractor_count = 0_u32;
    if mouse.power > 0.0 {
        attractors[0] = Attractor {
            position: mouse.position,
            // Bake `gravity_constant` into the attractor's `power` so the
            // WGSL kernel can treat power uniformly across attractor sources.
            power: mouse.power * settings.gravity_constant,
            _pad: 0.0,
        };
        attractor_count = 1;
    }

    // --- Drag baking ----------------------------------------------------
    // v4 uses fixed_dt = 0.016 * 2 = 0.032. We bake against the same constant
    // so the per-frame drag matches v4 regardless of the actual render dt.
    let fixed_dt = 0.032_f32;
    // Constants reproduced verbatim from v4's PARTICLE_SYSTEM_PARAMS — keep the
    // full precision so drag parity is bit-identical to v4. The `excessive_precision`
    // lint would otherwise trim digits an f32 can't represent; that's harmless in
    // isolation but loses the audit trail back to v4's source.
    #[allow(
        clippy::excessive_precision,
        clippy::unreadable_literal,
        reason = "v4 PARTICLE_SYSTEM_PARAMS drag constants — preserved verbatim for parity"
    )]
    let pulling_drag_baked = 0.93075095702_f32.powf(fixed_dt);
    #[allow(
        clippy::excessive_precision,
        clippy::unreadable_literal,
        reason = "v4 PARTICLE_SYSTEM_PARAMS drag constants — preserved verbatim for parity"
    )]
    let inertial_drag_baked = 0.53913643334_f32.powf(fixed_dt);

    // --- Size scaling (matches v4 sizeScaledGravityConstant) ------------
    let w = window.width();
    let size_scale = (2.0_f32.powf(w / 836.0 - 1.0)).min(1.0);

    // --- Constrain-to-box bounds (centered on origin, matching spawn) ---
    let h = window.height();
    let half_w = w * 0.5;
    let half_h = h * 0.5;
    let constrain_min = [-half_w, -half_h];
    let constrain_max = [half_w, half_h];

    sim.params = SimParams {
        dt: time.delta_secs().min(0.05),
        attractor_count,
        pulling_drag_baked,
        inertial_drag_baked,
        size_scale,
        fade_duration: 3.0, // v4 PARTICLE_SYSTEM_PARAMS.FADE_DURATION
        constrain_min,
        constrain_max,
        _pad: [0.0; 2],
        attractors,
    };
}
