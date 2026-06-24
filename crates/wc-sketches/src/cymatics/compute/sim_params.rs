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
/// 62 padding floats are never read by the shader.
///
/// Field order is load-bearing and must match the WGSL `struct IterParams`:
/// `time` at offset 0, `wave_signal` at offset 4. `wave_signal` is the
/// per-sub-step `amplitude·sin(phase)` oscillator value (amplitude from the
/// `source_amplitude` setting, v4 default `2.0`), precomputed CPU-side so the
/// shader does not recompute the same transcendental for every grid cell.
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct IterParamsGpu {
    /// `iGlobalTime` for this sub-step.
    pub time: f32,
    /// Precomputed wave-source oscillator `amplitude·sin(phase)` for this
    /// sub-step (uniform across the dispatch; hoisted out of the per-cell shader).
    pub wave_signal: f32,
    /// Padding to 256 bytes (dynamic-offset alignment). Never read by the shader.
    _pad: [f32; 62],
}

impl Default for IterParamsGpu {
    fn default() -> Self {
        Self {
            time: 0.0,
            wave_signal: 0.0,
            _pad: [0.0; 62],
        }
    }
}

// Compile-time guard: `IterParamsGpu` must be exactly the dynamic-offset stride.
const _: () = assert!(std::mem::size_of::<IterParamsGpu>() == 256);

/// Handles to the two ping-pong textures. Tagged on [`CymaticsRoot`] and
/// mirrored into [`CymaticsSimParams`] for the render world.
///
/// The odd-N continuity refresh guarantees texture A holds the latest field at
/// the end of every frame, so the render material samples A directly — there is
/// no separate display texture to blit into.
#[derive(Component, Clone)]
pub struct CymaticsTextures {
    /// Ping-pong texture A. Holds the latest field at frame end (the material
    /// samples it directly).
    pub a: Handle<Image>,
    /// Ping-pong texture B.
    pub b: Handle<Image>,
}

/// Per-frame sim state extracted into the render world each frame.
///
/// [`ExtractResource`] clones this into the render world so the compute plugin
/// (C6) can build its bind group without touching main-world resources.
///
/// The per-iteration phase is carried as two scalars (`phase_base`,
/// `phase_dt`) rather than a `Vec<f32>` of pre-multiplied times: sub-step `i`'s
/// time is `phase_base + i·phase_dt`, recomputed where it is written into the
/// GPU buffer. This keeps the whole resource POD, so the per-frame
/// `ExtractResource` clone is a cheap field copy (plus two `Handle` ref-count
/// bumps) with **no heap allocation** — a `Vec` field would otherwise force a
/// `Vec::clone` (alloc + free) every frame the resource changes, which is every
/// frame on the steady-state path.
#[derive(Resource, Clone, ExtractResource)]
pub struct CymaticsSimParams {
    /// Constant-per-frame uniform.
    pub params: SimParamsGpu,
    /// Base phase for sub-step 0 (v4 `simulationTime` at frame start). Sub-step
    /// `i`'s time is `phase_base + i·phase_dt`.
    pub phase_base: f32,
    /// Per-sub-step phase increment (`cycles·2π / iterations`).
    pub phase_dt: f32,
    /// Wave-source injection amplitude (`source_amplitude` setting, v4 `2.0`).
    /// Applied CPU-side in the compute prepare step: each sub-step's
    /// `wave_signal = source_amplitude · sin(phase)`. Kept here (not in the GPU
    /// `SimParamsGpu` uniform) because it is consumed while packing the
    /// per-iteration buffer, never read by the shader directly.
    pub source_amplitude: f32,
    /// Sub-steps this frame.
    pub iterations: u32,
    /// Ping-pong texture A. Holds the latest field at frame end and is the
    /// texture the render material samples directly (no display blit).
    pub tex_a: Handle<Image>,
    /// Ping-pong texture B.
    pub tex_b: Handle<Image>,
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

    /// `IterParamsGpu` field offsets must match the WGSL `struct IterParams`
    /// (`time` @0, `wave_signal` @4). A mismatch would silently bind the wrong
    /// f32 to each shader field — the C5/C6-style POD↔WGSL parity hazard.
    #[test]
    fn iter_params_field_offsets_match_wgsl() {
        assert_eq!(std::mem::offset_of!(IterParamsGpu, time), 0);
        assert_eq!(std::mem::offset_of!(IterParamsGpu, wave_signal), 4);
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
        assert!(
            (p.center[0] - 0.5).abs() < f32::EPSILON && (p.center[1] - 0.5).abs() < f32::EPSILON
        );
        assert!(
            (p.center2[0] - 0.5).abs() < f32::EPSILON && (p.center2[1] - 0.5).abs() < f32::EPSILON
        );
        assert!((p.active_radius - 0.1).abs() < f32::EPSILON);
        assert!((p.force_multiplier - FORCE_MULTIPLIER).abs() < f32::EPSILON);
        assert!((p.velocity_decay - VELOCITY_DECAY_FACTOR).abs() < f32::EPSILON);
        assert!((p.height_decay - HEIGHT_DECAY_FACTOR).abs() < f32::EPSILON);
        assert!(
            (p.accumulated_height_decay - ACCUMULATED_HEIGHT_DECAY_FACTOR).abs() < f32::EPSILON
        );
    }
}
