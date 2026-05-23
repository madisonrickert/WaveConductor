//! GPU-side particle and uniform layouts.
//!
//! Both structs are `#[repr(C)]` and `Pod + Zeroable` so they can be uploaded
//! verbatim to a WGSL storage buffer / uniform buffer. The layouts MUST stay
//! in sync with `assets/shaders/line/simulate.wgsl` and `assets/shaders/line/render.wgsl`.

use bytemuck::{Pod, Zeroable};

/// Per-particle state. Position + velocity in 2D world-space (centered on origin).
///
/// 16-byte aligned (4 × f32), matching the WGSL `struct Particle` layout.
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct Particle {
    /// World-space X/Y position.
    pub position: [f32; 2],
    /// X/Y velocity in world units per second.
    pub velocity: [f32; 2],
}

/// Compute kernel uniforms pushed every frame by [`crate::line::systems::update_sim_params`].
///
/// Field order matches the WGSL `struct SimParams` in `simulate.wgsl` exactly;
/// the Rust layout is `#[repr(C)]` so `bytemuck::bytes_of` produces the correct
/// byte sequence for a `BufferInitDescriptor`.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct SimParams {
    /// Frame time in seconds (capped to 50 ms to avoid blow-up on pauses).
    pub dt: f32,
    /// Per-frame velocity damping factor (clamped 0..1 by the shader).
    pub drag: f32,
    /// Pointer attractor position in world space.
    pub attractor_pos: [f32; 2],
    /// Soft attractor radius in world units.
    pub attractor_radius: f32,
    /// Gravity constant — acceleration toward the attractor per unit distance.
    pub gravity_constant: f32,
    /// 1.0 when the attractor is active, 0.0 when the pointer is absent.
    pub attractor_enabled: f32,
    /// Padding to maintain 16-byte struct alignment for WGSL uniform buffers.
    #[allow(
        clippy::pub_underscore_fields,
        reason = "GPU struct layout padding must be pub for bytemuck"
    )]
    pub _pad: f32,
}
