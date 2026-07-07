//! GPU-side POD mirrors for the Flame IFS compute pass, plus the extract
//! resource the render world reads.
//!
//! Layout contract with `assets/shaders/flame/simulate.wgsl` (kernel parity
//! discipline: change both together, term for term). All structs are
//! 16-byte-multiple sized, compile-time asserted.

#![allow(
    clippy::as_conversions,
    reason = "the variation/mode enums carry a documented #[repr(u32)] WGSL \
              switch key, so `enum as u32` is the intended narrowing; the \
              stride/count `as u64`/`as usize` conversions are on bounded \
              small values (MAX_LEVELS, branch count) and documented inline"
)]

use bevy::prelude::*;
use bevy::render::extract_resource::ExtractResource;
use bevy::render::storage::ShaderBuffer;
use bytemuck::{Pod, Zeroable};

use crate::flame::branches::{FlameSpec, AFFINE_MATS, AFFINE_OFFSETS};
use crate::flame::levels::{LevelLayout, MAX_LEVELS};

/// One IFS node: position + accumulated color. 32 bytes, matching WGSL
/// `struct FlameNode { pos: vec3<f32>, _pad0: f32, color: vec3<f32>, _pad1: f32 }`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct FlameNodeGpu {
    /// World-space position (pre camera/model transform).
    pub pos: [f32; 3],
    /// Padding (vec3 alignment).
    _pad0: f32,
    /// Accumulated additive color (can exceed `[0,1]`).
    pub color: [f32; 3],
    /// Padding.
    _pad1: f32,
}

/// One branch: row-major affine (rows in `mat_x/y/z.xyz`), constant offset,
/// additive color, and the variation switch keys. 96 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct FlameBranchGpu {
    /// Affine matrix row 0 (`.w` unused).
    pub mat_x: [f32; 4],
    /// Affine matrix row 1.
    pub mat_y: [f32; 4],
    /// Affine matrix row 2.
    pub mat_z: [f32; 4],
    /// Affine constant offset (`.w` unused).
    pub offset: [f32; 4],
    /// Additive per-application color (`.w` unused).
    pub color: [f32; 4],
    /// Primary variation id (`VariationId` repr).
    pub var_a: u32,
    /// Secondary variation id (== `var_a` for Single mode).
    pub var_b: u32,
    /// Combinator mode (`VariationMode` repr).
    pub mode: u32,
    /// Padding to 96 bytes.
    _pad: u32,
}

/// Frame-constant sim uniform: the branch table plus the per-frame attractor
/// drivers. 800 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct FlameSimParamsGpu {
    /// Up to 8 branches (see `branches::MAX_BRANCHES`); unused slots zeroed.
    pub branches: [FlameBranchGpu; 8],
    /// Per-frame attractor offset added to x/y after the base affine:
    /// `(cX/5 + cDx, cY/5 + cDy)` — v4's time oscillation + pointer/hand warp.
    pub warp: [f32; 2],
    /// Position lerp factor (v4: 0.8).
    pub lerp_pos: f32,
    /// Color lerp factor (v4: 0.75).
    pub lerp_col: f32,
    /// Live branch count (2..=8).
    pub branch_count: u32,
    /// Padding to 800 bytes.
    _pad: [u32; 3],
}

/// Per-level dispatch parameters, one 256-byte dynamic-offset slot per
/// dispatched level (the Cymatics stride pattern).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct FlameLevelParamsGpu {
    /// First node slot of this level.
    pub level_start: u32,
    /// Node count in this level.
    pub node_count: u32,
    /// First node slot of the parent level.
    pub parent_start: u32,
    /// Node count of the parent level (branch-major divisor).
    pub parent_count: u32,
    /// Padding to the 256-byte dynamic-offset stride.
    _pad: [u32; 60],
}

/// Dynamic-offset stride: `min_uniform_buffer_offset_alignment` is <= 256 on
/// every WebGPU target (verified at pipeline init, as Cymatics does).
pub const LEVEL_PARAMS_STRIDE: u64 = 256;

/// Default per-frame position lerp toward each node's target (v4: 0.8) — the
/// snappy live-tracking / per-keystroke reform rate. The screensaver name-morph
/// overrides `lerp_pos` with a slower, time-based value (see
/// `crate::flame::screensaver::drive_flame_attract_sim`); the live writer
/// (`crate::flame::systems::sim_params::update_flame_sim`) resets it here so a
/// mid-morph value cannot leak into Active mode on wake.
pub const LERP_POS_DEFAULT: f32 = 0.8;
/// Default per-frame color lerp (v4: 0.75). See [`LERP_POS_DEFAULT`].
pub const LERP_COL_DEFAULT: f32 = 0.75;

/// Per-frame settle lerp `(lerp_pos, lerp_col)`, blended between the crisp v4
/// defaults and the gentle screensaver name-morph rate by `fade_alpha`.
///
/// `fade_alpha` is the `ScreensaverFade` envelope: `0.0` fully awake, `1.0` fully
/// in attract mode, ramping over ~1.5 s at each transition. At `0.0` this returns
/// [`LERP_POS_DEFAULT`] / [`LERP_COL_DEFAULT`] (crisp live tracking + snappy
/// per-keystroke reform); at `1.0` it returns the slow exponential
/// `1 - exp(-dt / morph_seconds)` that eases each carousel name in. Because the
/// live writer (`update_flame_sim`) and the screensaver writer
/// (`drive_flame_attract_sim`) both blend on this same envelope, the sleep and
/// wake transitions ease smoothly between crisp and slow instead of snapping the
/// lerp on the frame the activity flips. `morph_seconds <= 0` disables the slow
/// end (always crisp).
#[must_use]
pub fn settle_lerp(morph_seconds: f32, dt: f32, fade_alpha: f32) -> (f32, f32) {
    if morph_seconds <= 0.0 {
        return (LERP_POS_DEFAULT, LERP_COL_DEFAULT);
    }
    let a = fade_alpha.clamp(0.0, 1.0);
    let slow = 1.0 - (-dt / morph_seconds).exp();
    (
        LERP_POS_DEFAULT + (slow - LERP_POS_DEFAULT) * a,
        LERP_COL_DEFAULT + (slow - LERP_COL_DEFAULT) * a,
    )
}

const _: () = assert!(std::mem::size_of::<FlameNodeGpu>() == 32);
const _: () = assert!(std::mem::size_of::<FlameBranchGpu>() == 96);
const _: () = assert!(std::mem::size_of::<FlameSimParamsGpu>() == 800);
const _: () = assert!(std::mem::size_of::<FlameLevelParamsGpu>() as u64 == LEVEL_PARAMS_STRIDE);

/// Extract resource mirrored into the render world each frame. POD fields +
/// one `Handle` so the `ExtractResourcePlugin` clone is a memcpy (no heap —
/// the Cymatics F2 lesson).
#[derive(Resource, Clone, ExtractResource)]
pub struct FlameSimParams {
    /// Frame-constant sim uniform contents.
    pub params: FlameSimParamsGpu,
    /// Per-level slots; `levels[i]` is tree level `i + 1` (root never
    /// dispatched). Only `level_count` slots are meaningful.
    pub levels: [FlameLevelParamsGpu; MAX_LEVELS],
    /// Levels to dispatch this frame. `0` freezes the fractal (Idle), the
    /// ember prefix lowers it during the screensaver.
    pub level_count: u32,
    /// The node storage buffer (owned here; the render material clones it).
    pub nodes: Handle<ShaderBuffer>,
}

/// Pack a [`FlameSpec`] into the GPU branch table. Warp starts at zero; the
/// per-frame writer overwrites it every frame.
#[must_use]
pub fn encode_branches(spec: &FlameSpec) -> FlameSimParamsGpu {
    let mut branches = [FlameBranchGpu::zeroed(); 8];
    for (slot, b) in branches.iter_mut().zip(&spec.branches) {
        let m = &AFFINE_MATS[b.affine_idx];
        let o = &AFFINE_OFFSETS[b.affine_idx];
        slot.mat_x = [m[0], m[1], m[2], 0.0];
        slot.mat_y = [m[3], m[4], m[5], 0.0];
        slot.mat_z = [m[6], m[7], m[8], 0.0];
        slot.offset = [o[0], o[1], o[2], 0.0];
        slot.color = [b.color[0], b.color[1], b.color[2], 0.0];
        slot.var_a = b.var_a as u32;
        slot.var_b = b.var_b as u32;
        slot.mode = b.mode as u32;
    }
    FlameSimParamsGpu {
        branches,
        warp: [0.0, 0.0],
        lerp_pos: LERP_POS_DEFAULT,
        lerp_col: LERP_COL_DEFAULT,
        branch_count: u32::try_from(spec.branches.len()).unwrap_or(2),
        _pad: [0; 3],
    }
}

/// Fill the per-level slots from a layout (tree level `i + 1` into slot `i`)
/// and return the total dispatchable level count.
pub fn encode_levels(layout: &LevelLayout, out: &mut [FlameLevelParamsGpu; MAX_LEVELS]) -> u32 {
    let mut n = 0_u32;
    for (slot, level) in out.iter_mut().zip(layout.levels.iter().skip(1)) {
        slot.level_start = level.start;
        slot.node_count = level.count;
        slot.parent_start = level.parent_start;
        slot.parent_count = level.parent_count;
        n += 1;
    }
    n
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
#[allow(
    clippy::float_cmp,
    reason = "the affine table rows are exact 0.0/-1.0 values, so bit-exact \
              array comparison against the encoded layout is intended"
)]
mod tests {
    use super::*;
    use crate::flame::branches::build_flame_spec;
    use crate::flame::levels::LevelLayout;

    /// WGSL layout contract: sizes are exact and 16-byte aligned; the level
    /// slot equals the dynamic-offset stride.
    #[test]
    fn pod_sizes_match_wgsl_layout() {
        assert_eq!(std::mem::size_of::<FlameNodeGpu>(), 32);
        assert_eq!(std::mem::size_of::<FlameBranchGpu>(), 96);
        assert_eq!(std::mem::size_of::<FlameSimParamsGpu>(), 800);
        assert_eq!(
            std::mem::size_of::<FlameLevelParamsGpu>() as u64,
            LEVEL_PARAMS_STRIDE
        );
    }

    /// Encoding packs the affine tables row-for-row and the variation
    /// ids/modes as their u32 reprs; unused branch slots stay zeroed.
    #[test]
    fn encode_branches_packs_v4_tables() {
        let spec = build_flame_spec("who are you?"); // 5 branches (F1 golden)
        let gpu = encode_branches(&spec);
        assert_eq!(gpu.branch_count, 5);
        assert!((gpu.lerp_pos - 0.8).abs() < f32::EPSILON);
        assert!((gpu.lerp_col - 0.75).abs() < f32::EPSILON);
        // Branch 0 golden: affine Negate(4) -> -I, varA Spherical(2),
        // mode Interpolated(1), varB Sin(1).
        let b0 = &gpu.branches[0];
        assert_eq!(b0.mat_x, [-1.0, 0.0, 0.0, 0.0]);
        assert_eq!(b0.mat_y, [0.0, -1.0, 0.0, 0.0]);
        assert_eq!(b0.mat_z, [0.0, 0.0, -1.0, 0.0]);
        assert_eq!(b0.offset, [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(b0.var_a, 2);
        assert_eq!(b0.var_b, 1);
        assert_eq!(b0.mode, 1);
        assert!((b0.color[0] - spec.branches[0].color[0]).abs() < f32::EPSILON);
        // Slot 5..8 unused -> zeroed.
        assert_eq!(gpu.branches[5].mode, 0);
        assert_eq!(gpu.branches[7].mat_x, [0.0; 4]);
    }

    /// Level encoding fills dispatched levels only (tree level i+1 in slot i)
    /// and returns the dispatch count = levels - 1 (root is never dispatched).
    #[test]
    fn encode_levels_skips_root() {
        let layout = LevelLayout::build(5, 100_000.0);
        let mut slots = [FlameLevelParamsGpu::zeroed(); crate::flame::levels::MAX_LEVELS];
        let n = encode_levels(&layout, &mut slots);
        assert_eq!(n as usize, layout.levels.len() - 1);
        // Slot 0 = tree level 1: 5 nodes starting at 1, parented on the root.
        assert_eq!(slots[0].level_start, 1);
        assert_eq!(slots[0].node_count, 5);
        assert_eq!(slots[0].parent_start, 0);
        assert_eq!(slots[0].parent_count, 1);
        // Last slot = deepest level.
        let deepest = layout.levels.last().expect("levels");
        let last = &slots[(n - 1) as usize];
        assert_eq!(last.level_start, deepest.start);
        assert_eq!(last.node_count, deepest.count);
    }

    /// `settle_lerp` blends crisp↔slow across the fade, so sleep/wake ease.
    #[test]
    fn settle_lerp_blends_crisp_to_slow_on_fade() {
        let dt = 1.0 / 60.0;
        // Awake (fade 0) -> crisp v4 defaults.
        let (p0, c0) = settle_lerp(5.0, dt, 0.0);
        assert!((p0 - LERP_POS_DEFAULT).abs() < f32::EPSILON);
        assert!((c0 - LERP_COL_DEFAULT).abs() < f32::EPSILON);
        // Fully in attract (fade 1) -> slow, both channels at the same rate.
        let (p1, c1) = settle_lerp(5.0, dt, 1.0);
        assert!(
            p1 < LERP_POS_DEFAULT,
            "attract pos lerp slower than default"
        );
        assert!((p1 - c1).abs() < f32::EPSILON, "pos and col ease together");
        // Mid-transition sits strictly between slow and crisp (smooth ease).
        let (pm, _) = settle_lerp(5.0, dt, 0.5);
        assert!(
            pm > p1 && pm < LERP_POS_DEFAULT,
            "mid-fade between slow and crisp"
        );
        // Disabled -> always crisp regardless of fade.
        let (pd, cd) = settle_lerp(0.0, dt, 1.0);
        assert!((pd - LERP_POS_DEFAULT).abs() < f32::EPSILON);
        assert!((cd - LERP_COL_DEFAULT).abs() < f32::EPSILON);
    }
}
