//! POD uniform types shared with `assets/shaders/cymatics/simulate.wgsl`, plus
//! the `ExtractResource` that carries per-frame sim state into the render world.
//!
//! Field order in [`SimParamsGpu`] must match the WGSL `struct SimParams`
//! exactly; `#[repr(C)]` + `bytemuck` produces the byte sequence. The
//! per-iteration phase is a dynamic-offset uniform array of [`IterParamsGpu`]
//! (256-byte stride, the `min_uniform_buffer_offset_alignment`).

use bevy::prelude::*;
use bevy::render::extract_resource::ExtractResource;
use bytemuck::{Pod, Zeroable};

/// Max sim sub-steps per frame (the `iterations` Dev setting cap).
pub const MAX_ITERATIONS: usize = 120;

/// Dynamic-offset stride for the per-iteration uniform (WebGPU min alignment).
pub const ITER_PARAMS_STRIDE: u64 = 256;

/// Constant-per-frame simulation uniform. Mirrors `simulate.wgsl::SimParams`.
///
/// Field order is load-bearing: `#[repr(C)]` lays fields out in declaration
/// order, matching the WGSL `struct SimParams` byte-for-byte. Any reorder
/// silently corrupts every dispatch's uniforms.
///
/// Total: 2+2+2+1+1+1+1+1+1 × 4 bytes = 48 bytes (a 16-byte multiple).
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct SimParamsGpu {
    /// Primary wave-source centre, UV [0,1].
    pub center: [f32; 2],
    /// Secondary wave-source centre, UV [0,1].
    pub center2: [f32; 2],
    /// Sim grid size in texels (w, h).
    pub resolution: [u32; 2],
    /// Alive-mask radius around the centres.
    pub active_radius: f32,
    /// Neighbour-force scale (v4 `FORCE_MULTIPLIER = 0.25`).
    pub force_multiplier: f32,
    /// Velocity damping (v4 `0.99818`).
    pub velocity_decay: f32,
    /// Height damping (v4 `0.9999`).
    pub height_decay: f32,
    /// Accumulated-height decay (v4 `0.999`).
    pub accumulated_height_decay: f32,
    /// Pad to a 16-byte multiple (header = 48 bytes).
    _pad: f32,
}

/// Neighbour-force scale in the wave integrator (v4 `FORCE_MULTIPLIER`).
pub const FORCE_MULTIPLIER: f32 = 0.25;
/// Per-substep velocity damping (v4 `VELOCITY_DECAY_FACTOR`).
pub const VELOCITY_DECAY_FACTOR: f32 = 0.99818;
/// Per-substep height damping (v4 `HEIGHT_DECAY_FACTOR`).
pub const HEIGHT_DECAY_FACTOR: f32 = 0.9999;
/// Accumulated-height decay (v4 `ACCUMULATED_HEIGHT_DECAY_FACTOR`).
pub const ACCUMULATED_HEIGHT_DECAY_FACTOR: f32 = 0.999;

impl SimParamsGpu {
    /// Build the constant-per-frame uniform with the v4 resting physics
    /// constants, the given grid `resolution` (texels), and `active_radius`.
    ///
    /// The centres are seeded at the UV centre `[0.5, 0.5]` in **top-left**
    /// origin (Bevy-native, no v4-style `y = 1 − y` flip) and are overwritten
    /// each frame by the sim-params bridge. This constructor lives here rather
    /// than at the call site because the `_pad` field is module-private; it
    /// mirrors how `render::spawn_cymatics_quad` packs the private pad of
    /// [`crate::cymatics::render::CymaticsRenderParams`].
    #[must_use]
    pub fn with_resting_physics(resolution: [u32; 2], active_radius: f32) -> Self {
        Self {
            center: [0.5, 0.5],
            center2: [0.5, 0.5],
            resolution,
            active_radius,
            force_multiplier: FORCE_MULTIPLIER,
            velocity_decay: VELOCITY_DECAY_FACTOR,
            height_decay: HEIGHT_DECAY_FACTOR,
            accumulated_height_decay: ACCUMULATED_HEIGHT_DECAY_FACTOR,
            _pad: 0.0,
        }
    }
}

/// Per-iteration phase uniform, padded to the dynamic-offset stride.
///
/// Sized to exactly 256 bytes so each entry in the per-frame iteration buffer
/// lands on a `min_uniform_buffer_offset_alignment`-aligned boundary. The
/// 63 padding floats are never read by the shader.
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct IterParamsGpu {
    /// `iGlobalTime` for this sub-step.
    pub time: f32,
    /// Padding to 256 bytes (dynamic-offset alignment). Never read by the shader.
    _pad: [f32; 63],
}

impl Default for IterParamsGpu {
    fn default() -> Self {
        Self {
            time: 0.0,
            _pad: [0.0; 63],
        }
    }
}

// Compile-time guard: `IterParamsGpu` must be exactly the dynamic-offset stride.
const _: () = assert!(std::mem::size_of::<IterParamsGpu>() == 256);

/// Handles to the ping-pong + display textures. Tagged on [`CymaticsRoot`] and
/// mirrored into [`CymaticsSimParams`] for the render world.
#[derive(Component, Clone)]
pub struct CymaticsTextures {
    /// Ping-pong texture A.
    pub a: Handle<Image>,
    /// Ping-pong texture B.
    pub b: Handle<Image>,
    /// Stable display texture (final blit target; sampled by the material).
    pub display: Handle<Image>,
}

/// Per-frame sim state extracted into the render world each frame.
///
/// [`ExtractResource`] clones this into the render world so the compute plugin
/// (C6) can build its bind group without touching main-world resources.
///
/// `iter_times` is pre-allocated to `MAX_ITERATIONS` and refilled with
/// `clear()` + `push` each frame — no per-frame heap allocation on the
/// steady-state path.
#[derive(Resource, Clone, ExtractResource)]
pub struct CymaticsSimParams {
    /// Constant-per-frame uniform.
    pub params: SimParamsGpu,
    /// Per-iteration phase times (`base + i·dt`); length == `iterations`,
    /// capacity pre-allocated to `MAX_ITERATIONS` and refilled with `clear()`.
    pub iter_times: Vec<f32>,
    /// Sub-steps this frame.
    pub iterations: u32,
    /// Ping-pong texture A.
    pub tex_a: Handle<Image>,
    /// Ping-pong texture B.
    pub tex_b: Handle<Image>,
    /// Display texture (blit target).
    pub display: Handle<Image>,
    /// Sim resolution in texels.
    pub resolution: UVec2,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sim_params_header_is_16_byte_aligned() {
        assert!(std::mem::size_of::<SimParamsGpu>().is_multiple_of(16));
    }

    #[test]
    fn iter_params_is_256_bytes() {
        assert_eq!(std::mem::size_of::<IterParamsGpu>(), 256);
        assert_eq!(ITER_PARAMS_STRIDE, 256);
    }

    #[test]
    fn default_sim_params_round_trips_through_bytemuck() {
        let p = SimParamsGpu::default();
        let bytes = bytemuck::bytes_of(&p);
        assert_eq!(bytes.len(), std::mem::size_of::<SimParamsGpu>());
    }

    #[test]
    fn with_resting_physics_carries_v4_constants() {
        let p = SimParamsGpu::with_resting_physics([640, 480], 0.1);
        assert_eq!(p.resolution, [640, 480]);
        // Top-left UV convention, no y-flip.
        assert!((p.center[0] - 0.5).abs() < f32::EPSILON && (p.center[1] - 0.5).abs() < f32::EPSILON);
        assert!(
            (p.center2[0] - 0.5).abs() < f32::EPSILON && (p.center2[1] - 0.5).abs() < f32::EPSILON
        );
        assert!((p.active_radius - 0.1).abs() < f32::EPSILON);
        assert!((p.force_multiplier - FORCE_MULTIPLIER).abs() < f32::EPSILON);
        assert!((p.velocity_decay - VELOCITY_DECAY_FACTOR).abs() < f32::EPSILON);
        assert!((p.height_decay - HEIGHT_DECAY_FACTOR).abs() < f32::EPSILON);
        assert!((p.accumulated_height_decay - ACCUMULATED_HEIGHT_DECAY_FACTOR).abs() < f32::EPSILON);
    }
}
