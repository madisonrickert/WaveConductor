//! GPU-side particle and uniform layouts.
//!
//! Both structs are `#[repr(C)]` and `Pod + Zeroable` so they can be uploaded
//! verbatim to a WGSL storage buffer / uniform buffer. The layouts MUST stay
//! in sync with `assets/shaders/line/simulate.wgsl` and `assets/shaders/line/render.wgsl`.

use bytemuck::{Pod, Zeroable};

/// Per-particle state. Position + velocity in 2D world-space (centered on
/// origin), plus the original spawn position (for constrain-to-box reset) and
/// the fade-in α.
///
/// 32-byte aligned (8 × f32, the trailing `_pad` brings the struct to a
/// 16-byte multiple) — see the WGSL `struct Particle` in `simulate.wgsl` and
/// `render.wgsl`.
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct Particle {
    /// World-space X/Y position (current).
    pub position: [f32; 2],
    /// X/Y velocity in world units per second.
    pub velocity: [f32; 2],
    /// Spawn position; OOB particles teleport here.
    pub original_xy: [f32; 2],
    /// Fade-in alpha, ramps 0 → 1 over `SimParams.fade_duration` seconds.
    pub alpha: f32,
    /// Padding to keep the struct multiple-of-16 aligned for WGSL storage rules.
    #[allow(
        clippy::pub_underscore_fields,
        reason = "GPU struct layout padding must be pub for bytemuck"
    )]
    pub _pad: f32,
}

/// One gravitational attractor — position in world space + power (force scale).
///
/// `power == 0.0` means inactive; the simulate kernel skips zero-power entries.
/// 16-byte aligned (4 × f32) matching the WGSL `struct Attractor` layout.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct Attractor {
    /// World-space X/Y position.
    pub position: [f32; 2],
    /// Force scale. Mouse attractor uses power=10 at press, decays geometrically.
    pub power: f32,
    /// Padding to keep the struct 16-byte aligned (WGSL std140/storage rules).
    #[allow(
        clippy::pub_underscore_fields,
        reason = "GPU struct layout padding must be pub for bytemuck"
    )]
    pub _pad: f32,
}

/// Maximum simultaneous attractors. Index 0 is the mouse; indices 1..=N are
/// reserved for future Leap-tracked hands (Plan 11+).
// TODO(plan-11): consider dynamic-sized storage buffer if MAX_ATTRACTORS > ~16
pub const MAX_ATTRACTORS: usize = 8;

/// Compute kernel uniforms pushed every frame.
///
/// Field order matches the WGSL `struct SimParams` in `simulate.wgsl` exactly;
/// the Rust layout is `#[repr(C)]` so `bytemuck::bytes_of` produces the
/// correct byte sequence. WGSL alignment for arrays-of-structs requires the
/// header fields ahead of the array to total a multiple of 16 bytes.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct SimParams {
    /// Frame time in seconds (capped to 50 ms to avoid blow-up on pauses).
    pub dt: f32,
    /// Number of attractors with `power > 0` to process. Capped at
    /// [`MAX_ATTRACTORS`]; bytes beyond `attractor_count` in `attractors` are
    /// ignored by the kernel.
    pub attractor_count: u32,
    /// Pulling drag baked via `pow(PULLING_DRAG_CONSTANT, fixed_dt)`. Active
    /// when at least one attractor has `power > 0`.
    pub pulling_drag_baked: f32,
    /// Inertial drag baked via `pow(INERTIAL_DRAG_CONSTANT, fixed_dt)`. Active
    /// when no attractors are active.
    pub inertial_drag_baked: f32,
    /// Multiplier on `gravity_constant` derived from canvas width. v4 uses
    /// `min(2^(width/836 - 1), 1)`; identical here.
    pub size_scale: f32,
    /// Per-particle fade-in duration in seconds.
    pub fade_duration: f32,
    /// Lower world-space bounds (`x_min`, `y_min`) for the constrain-to-box reset.
    pub constrain_min: [f32; 2],
    /// Upper world-space bounds (`x_max`, `y_max`).
    pub constrain_max: [f32; 2],
    /// Padding to bring the header to a 16-byte boundary before the array.
    /// The header above totals 40 bytes (six 4-byte scalars plus two 8-byte
    /// `vec2`s); we need 8 more to reach 48 (a multiple of 16) so the
    /// `attractors` array begins aligned.
    #[allow(
        clippy::pub_underscore_fields,
        reason = "GPU struct layout padding must be pub for bytemuck"
    )]
    pub _pad: [f32; 2],
    /// Attractor list. Entries `[0..attractor_count]` are live; the rest are
    /// zero-power and ignored.
    pub attractors: [Attractor; MAX_ATTRACTORS],
}

const _: () = {
    assert!(std::mem::size_of::<SimParams>() % 16 == 0);
    assert!(std::mem::size_of::<Attractor>() % 16 == 0);
    assert!(std::mem::size_of::<Particle>() % 16 == 0);
};
