//! GPU-side POD mirrors for the Radiance particle kernel, plus the extract
//! resource the render world reads.
//!
//! Layout contract with `assets/shaders/radiance/simulate.wgsl` and
//! `assets/shaders/radiance/render.wgsl` (kernel parity discipline: change
//! all copies together, field for field). All structs are 16-byte-multiple
//! sized, compile-time asserted, and locked by `offset_of!` tests below.

use bevy::prelude::*;
use bevy::render::extract_resource::ExtractResource;
use bevy::render::storage::ShaderBuffer;
use bytemuck::{Pod, Zeroable};

/// One aura particle. 32 bytes, matching the WGSL `struct Particle` in both
/// radiance shaders.
///
/// A particle is **dead** when `age >= lifespan`; `Zeroable::zeroed()` (age 0,
/// lifespan 0) is therefore dead, so the spawn-time buffer needs no CPU
/// seeding — the kernel's edge-respawn path births every particle.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct RadianceParticle {
    /// World-space X/Y position (Camera2d units: 1 unit = 1 px, origin center).
    pub position: [f32; 2],
    /// X/Y velocity in world px/s.
    pub velocity: [f32; 2],
    /// Seconds since this particle's last respawn.
    pub age: f32,
    /// Seconds this particle lives; kernel-assigned at respawn from a hash in
    /// `[lifespan_min, lifespan_max]` so deaths stagger instead of pulsing.
    pub lifespan: f32,
    /// Deterministic per-respawn hash in `0..=1`: the render shader's flicker
    /// phase and per-particle variation seed.
    pub seed: f32,
    /// Body slot index (`0..4`) this particle spawned from, stored as `f32`
    /// (the struct is homogeneous f32; the render shader rounds it back to an
    /// index into the per-slot color array). Doubles as the layout padding
    /// that rounds the struct to a multiple of its 8-byte alignment (the WGSL
    /// storage-address-space array-stride rule); a zeroed buffer reads slot 0,
    /// which is harmless because zeroed particles are dead.
    pub slot: f32,
}

/// One limb impulse slot — the fixed-slot idiom of the shared particle
/// engine's `Attractor[8]`. 32 bytes, matching WGSL `struct Impulse`.
///
/// `gain == 0.0` means inactive; the kernel skips zero-gain entries.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct RadianceImpulse {
    /// World-space X/Y position of the limb.
    pub position: [f32; 2],
    /// Limb velocity in world px/s — particles near the limb inherit a
    /// locally-weighted share of it, so a fast limb sheds a burst.
    pub velocity: [f32; 2],
    /// Influence radius in world px; the coupling fades to zero by `radius`.
    pub radius: f32,
    /// Coupling gain `0..=1` (CPU-derived from limb speed).
    pub gain: f32,
    /// Padding to a 32-byte (16-multiple) stride for the WGSL uniform array.
    #[allow(
        clippy::pub_underscore_fields,
        reason = "GPU struct layout padding must be pub for bytemuck"
    )]
    pub _pad: [f32; 2],
}

/// Maximum simultaneous limb impulses. Seven landmark slots are used today
/// (nose, wrists, hips, ankles — see `systems::sim_params::IMPULSE_LANDMARKS`);
/// the eighth is headroom, same shape as the particle engine's
/// `MAX_ATTRACTORS`.
pub const MAX_IMPULSES: usize = 8;

/// Compute-kernel uniforms pushed every frame.
///
/// Field order matches the WGSL `struct SimParams` in `simulate.wgsl`
/// exactly; the layout is `#[repr(C)]` so `bytemuck::bytes_of` produces the
/// correct byte sequence. The scalar header totals 144 bytes — a 16-byte
/// multiple — so the `impulses` array (16-byte-aligned per WGSL uniform
/// rules, 32-byte stride) begins aligned at offset 144. Total size 400.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct RadianceSimParamsGpu {
    /// Frame time in seconds (capped to 50 ms to avoid blow-up on pauses).
    pub dt: f32,
    /// Elapsed virtual time in seconds — scrolls the curl field and salts the
    /// respawn hash per frame.
    pub time: f32,
    /// Per-dead-particle respawn probability THIS FRAME (already
    /// `rate × dt`-baked and clamped to `0..=1` by the CPU baker). `0.0`
    /// freezes emission (the Idle hook's write).
    pub emission_prob: f32,
    /// Live entries in the edge storage buffer. `0` = no silhouette this
    /// frame → the respawn path is skipped entirely.
    pub edge_count: u32,
    /// Particle buffer length; the kernel also guards with `arrayLength`.
    pub particle_count: u32,
    /// Spawn offset along the outward normal, world px.
    pub spawn_offset: f32,
    /// Initial speed along the outward normal, world px/s.
    pub spawn_speed: f32,
    /// Extra outward speed from the onset burst envelope, world px/s.
    pub burst_speed: f32,
    /// Upward acceleration, world px/s² (bass-pulsed by the baker).
    pub buoyancy: f32,
    /// Curl-flow advection speed, world px/s (highs-scaled by the baker).
    pub flow_strength: f32,
    /// Curl spatial frequency, radians per world px.
    pub curl_scale: f32,
    /// Curl octave count, clamped `1..=3` in the kernel.
    pub curl_octaves: u32,
    /// Per-frame velocity retention, baked CPU-side as
    /// `DRAG_PER_SECOND.powf(dt)` so drag is framerate-independent.
    pub drag_baked: f32,
    /// Respawn lifespan range, seconds (kernel hashes within it).
    pub lifespan_min: f32,
    /// See `lifespan_min`.
    pub lifespan_max: f32,
    /// `1` = mirror horizontally (flip mask-UV x); `0` = as-captured.
    pub mirror: u32,
    /// Mask-UV → world scale: `world = ((u - 0.5) * x, (0.5 - v) * y)`.
    /// The CPU-side twin is `systems::sim_params::mask_uv_to_world`; both
    /// sides must stay term-for-term identical.
    pub uv_to_world: [f32; 2],
    /// Live impulse slots (`impulses[0..impulse_count]`), capped at
    /// [`MAX_IMPULSES`].
    pub impulse_count: u32,
    /// Monotonic frame counter, CPU-incremented (`wrapping_add`) once per bake.
    /// Salts the kernel's per-frame respawn hash so a losing dead particle
    /// re-rolls fresh every frame. Replaces the shader's old
    /// `u32(time * 60.0)` derivation, which aliased whenever two bakes landed
    /// in the same 1/60 s virtual-time bucket (or when `time` was pinned, e.g.
    /// the screensaver clock).
    pub frame: u32,
    /// Per-slot start offset into the edge storage buffer (`SilhouetteEdges`
    /// concatenates slots in ascending order; these are the prefix sums of
    /// `slot_counts`). WGSL mirrors this as a `vec4<u32>`.
    pub slot_start: [u32; 4],
    /// Per-slot live edge count (`SilhouetteEdges::slot_counts`, clamped so
    /// `start + count` never exceeds `edge_count`). WGSL `vec4<u32>`.
    pub slot_count: [u32; 4],
    /// Per-slot emission CDF over the fade-weighted spawn shares (see
    /// `systems::sim_params::emission_slot_weights`): monotone, last live
    /// entry 1.0. All-zero = no live slot → the kernel spawns nothing. The
    /// kernel picks the spawn slot as the first `i` with `rand < cdf[i]`, so
    /// the shared particle budget is apportioned by fade — density stays
    /// constant as dancers come and go. WGSL `vec4<f32>`.
    pub slot_cdf: [f32; 4],
    /// Probability that a spawn becomes a fast "ejecta" streak this frame
    /// (onset-driven; see the baker). `0..=1`.
    pub ejecta_prob: f32,
    /// Outward launch speed of ejecta spawns, world px/s.
    pub ejecta_speed: f32,
    /// Flame-tongue amplitude: buoyancy varies by `±tongue_amp` with a
    /// two-sine noise along world x (tongues of rising flame instead of a
    /// uniform sheet). `0..~1.2`.
    pub tongue_amp: f32,
    /// Tongue spatial frequency, radians per world px (~300 px wavelength at
    /// the default).
    pub tongue_freq: f32,
    /// Impulse slots; entries past `impulse_count` are zero-gain and ignored.
    pub impulses: [RadianceImpulse; MAX_IMPULSES],
}

const _: () = {
    assert!(std::mem::size_of::<RadianceParticle>().is_multiple_of(16));
    assert!(std::mem::size_of::<RadianceImpulse>().is_multiple_of(16));
    assert!(std::mem::size_of::<RadianceSimParamsGpu>().is_multiple_of(16));
};

/// Extract resource mirrored into the render world each frame. POD fields +
/// one `Handle`, so the `ExtractResourcePlugin` clone is a memcpy (no heap —
/// the Cymatics F2 lesson).
///
/// The `paused` / `frozen_secs` pair lives on the extract copy only — it is
/// **not** part of [`RadianceSimParamsGpu`], so the 400-byte uniform layout
/// and its parity tests are untouched.
#[derive(Resource, Clone, ExtractResource)]
pub struct RadianceSimParams {
    /// Per-frame kernel uniforms (baked by `systems::sim_params`).
    pub params: RadianceSimParamsGpu,
    /// The particle storage buffer (owned here; the render material clones it).
    pub particles: Handle<ShaderBuffer>,
    /// Particle buffer length — determines dispatch size.
    pub particle_count: u32,
    /// True once emission has been zero long enough that every particle is
    /// deterministically dead (`systems::sim_params::step_radiance_pause`);
    /// the render world maps it to a dispatch size of 0 and the main world
    /// hides the billboard entity, so a long Idle stops paying the compute +
    /// 6-verts-per-particle draw for an all-dead field.
    pub paused: bool,
    /// Simulated seconds emission has been continuously zero. Advances by
    /// `params.dt` per `Update` — the exact amount the kernel ages particles
    /// per dispatch (one dispatch per app update), so this clock matches GPU
    /// particle aging even under Idle frame throttling. Clamped at the pause
    /// bound so a settled Idle frame stops dirtying the resource.
    pub frozen_secs: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `RadianceParticle` field offsets must match the WGSL `struct Particle`
    /// in `simulate.wgsl` / `render.wgsl` exactly.
    #[test]
    fn particle_field_offsets_match_wgsl() {
        assert_eq!(std::mem::offset_of!(RadianceParticle, position), 0);
        assert_eq!(std::mem::offset_of!(RadianceParticle, velocity), 8);
        assert_eq!(std::mem::offset_of!(RadianceParticle, age), 16);
        assert_eq!(std::mem::offset_of!(RadianceParticle, lifespan), 20);
        assert_eq!(std::mem::offset_of!(RadianceParticle, seed), 24);
        assert_eq!(std::mem::offset_of!(RadianceParticle, slot), 28);
        assert_eq!(std::mem::size_of::<RadianceParticle>(), 32);
    }

    /// A zeroed particle is dead (age 0 >= lifespan 0): the spawn buffer
    /// needs no CPU seeding.
    #[test]
    fn zeroed_particle_is_dead() {
        let p = RadianceParticle::zeroed();
        assert!(p.age >= p.lifespan);
    }

    /// `RadianceImpulse` offsets must match WGSL `struct Impulse`.
    #[test]
    fn impulse_field_offsets_match_wgsl() {
        assert_eq!(std::mem::offset_of!(RadianceImpulse, position), 0);
        assert_eq!(std::mem::offset_of!(RadianceImpulse, velocity), 8);
        assert_eq!(std::mem::offset_of!(RadianceImpulse, radius), 16);
        assert_eq!(std::mem::offset_of!(RadianceImpulse, gain), 20);
        assert_eq!(std::mem::offset_of!(RadianceImpulse, _pad), 24);
        assert_eq!(std::mem::size_of::<RadianceImpulse>(), 32);
    }

    /// `RadianceSimParamsGpu` offsets must match the WGSL `struct SimParams`
    /// in `simulate.wgsl` exactly; a reorder silently corrupts every
    /// dispatch's uniforms.
    #[test]
    fn sim_params_field_offsets_match_wgsl() {
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, dt), 0);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, time), 4);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, emission_prob), 8);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, edge_count), 12);
        assert_eq!(
            std::mem::offset_of!(RadianceSimParamsGpu, particle_count),
            16
        );
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, spawn_offset), 20);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, spawn_speed), 24);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, burst_speed), 28);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, buoyancy), 32);
        assert_eq!(
            std::mem::offset_of!(RadianceSimParamsGpu, flow_strength),
            36
        );
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, curl_scale), 40);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, curl_octaves), 44);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, drag_baked), 48);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, lifespan_min), 52);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, lifespan_max), 56);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, mirror), 60);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, uv_to_world), 64);
        assert_eq!(
            std::mem::offset_of!(RadianceSimParamsGpu, impulse_count),
            72
        );
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, frame), 76);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, slot_start), 80);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, slot_count), 96);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, slot_cdf), 112);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, ejecta_prob), 128);
        assert_eq!(
            std::mem::offset_of!(RadianceSimParamsGpu, ejecta_speed),
            132
        );
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, tongue_amp), 136);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, tongue_freq), 140);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, impulses), 144);
    }

    /// Locks the "header 144 bytes, total 400" claim to the real const, so a
    /// change to `MAX_IMPULSES` cannot silently shift the size expectations.
    #[test]
    fn sim_params_size_tracks_max_impulses() {
        const HEADER_BYTES: usize = 144;
        const IMPULSE_STRIDE: usize = 32;
        assert_eq!(
            std::mem::size_of::<RadianceSimParamsGpu>(),
            HEADER_BYTES + MAX_IMPULSES * IMPULSE_STRIDE
        );
    }
}
