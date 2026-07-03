//! Name -> fractal generation: v4's `stringHash`, PRNG, and `randomBranch*`
//! ported with f64 arithmetic so the same name produces the same fractal as
//! v4 (JS numbers are f64; `*`, `+`, `%` are IEEE-exact in both languages).
//!
//! Also owns the CPU mirror of the WGSL kernel's affine tables and variation
//! functions (`AFFINE_MATS`/`AFFINE_OFFSETS`, [`apply_variation_cpu`],
//! [`apply_branch_cpu`]). Kernel parity discipline: this file and
//! `assets/shaders/flame/simulate.wgsl` change together term-for-term.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::excessive_precision,
    reason = "v4 parity: JS ToInteger / Number->float narrowings (gen % 7 -> \
              usize, substring index truncation, f64 hash math -> f32) are \
              intentional and documented inline; the affine coefficients carry \
              the exact digits whose f32 rounding reproduces v4's output"
)]
#![allow(
    clippy::doc_markdown,
    clippy::manual_midpoint,
    clippy::many_single_char_names,
    reason = "the affine/variation docs name v4 transform identifiers as prose, \
              and the parity spot-check tests deliberately use single-letter \
              math-probe bindings and v4's (x + k) / 2 formula shapes"
)]

/// The default name shown as the input placeholder and used when the input is
/// empty. v4: `FlameNameInput.DEFAULT_NAME`.
pub const DEFAULT_NAME: &str = "who are you?";

/// Maximum branch count. `numBranches = ceil(1 + len%5 + floor(len/5))` with
/// `len` in 1..=20 peaks at 8 (len = 19).
pub const MAX_BRANCHES: usize = 8;

/// v4's PRNG modulus: 2^31 - 1.
const GEN_DIVISOR: f64 = 2_147_483_647.0;

/// The seven affine maps from v4 `transforms.ts::AFFINES`, decomposed into a
/// row-major 3x3 matrix plus offset (every v4 affine is linear + constant).
/// Order matches v4's object-key order; `affine_idx` indexes both tables.
///
/// 0 TowardsOriginNegativeBias  1 TowardsOrigin2  2 Swap  3 SwapSub
/// 4 Negate                     5 NegateSwap      6 Up1
pub const AFFINE_MATS: [[f32; 9]; 7] = [
    [0.5, 0.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.5],
    [0.5, 0.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.5],
    [0.0, 0.4, 0.4, 0.4, 0.0, 0.4, 0.4, 0.4, 0.0],
    [0.0, 0.5, -0.5, -0.5, 0.0, 0.5, 0.5, -0.5, 0.0],
    [-1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, -1.0],
    [
        -0.476_190_48,
        0.476_190_48,
        0.476_190_48,
        0.476_190_48,
        -0.476_190_48,
        0.476_190_48,
        0.476_190_48,
        0.476_190_48,
        -0.476_190_48,
    ],
    [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
];

/// Constant offsets paired with [`AFFINE_MATS`].
pub const AFFINE_OFFSETS: [[f32; 3]; 7] = [
    [-0.25, -0.5, 0.0],
    [0.5, -0.6, 0.4],
    [0.0, 0.0, 0.0],
    [0.0, 0.0, 0.0],
    [0.0, 0.0, 0.0],
    [0.0, 0.0, 0.0],
    [0.0, 0.0, 1.0],
];

/// The seven nonlinear variations from v4 `transforms.ts::VARIATIONS`,
/// in object-key order. The u32 repr is the WGSL kernel's switch key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum VariationId {
    /// Identity.
    Linear = 0,
    /// Component-wise sine.
    Sin = 1,
    /// `p / |p|^2` (zero-safe).
    Spherical = 2,
    /// `(atan2(y,x)/pi, |p| - 1, atan2(z,x))`.
    Polar = 3,
    /// Rotation-like mix by `sin/cos(|p|^2)`.
    Swirl = 4,
    /// `p / |p|` (zero-safe, THREE `normalize`).
    Normalize = 5,
    /// `setLength(exp(-|p|^2))` (zero-safe).
    Shrink = 6,
}

impl VariationId {
    /// Variation table in v4 object-key order; `gen % 7` indexes this.
    const TABLE: [Self; 7] = [
        Self::Linear,
        Self::Sin,
        Self::Spherical,
        Self::Polar,
        Self::Swirl,
        Self::Normalize,
        Self::Shrink,
    ];
}

/// How `var_a`/`var_b` combine, from v4 `createInterpolatedVariation` /
/// `createRouterVariation`. The u32 repr is the WGSL switch key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum VariationMode {
    /// Apply `var_a` only.
    Single = 0,
    /// `mix(var_a(p), var_b(p), 0.5)` (v4's constant interpolation fn).
    Interpolated = 1,
    /// `if p.z < 0 { var_a(p) } else { var_b(p) }`.
    Router = 2,
}

/// One IFS branch: affine + variation combinator + additive color.
#[derive(Debug, Clone, PartialEq)]
pub struct BranchSpec {
    /// Index into [`AFFINE_MATS`]/[`AFFINE_OFFSETS`].
    pub affine_idx: usize,
    /// Primary variation.
    pub var_a: VariationId,
    /// Secondary variation (== `var_a` when `mode` is `Single`).
    pub var_b: VariationId,
    /// Combinator mode.
    pub mode: VariationMode,
    /// Additive per-application color (can exceed `[0,1]`; additive blending
    /// and the HDR camera absorb it, as in v4).
    pub color: [f32; 3],
}

/// Name-derived audio character (v4 `configureForName` + the density
/// approximation replacing the box-count visitor).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NameAudioConfig {
    /// Lowpass cutoff for the DC-osc voice, Hz. v4: map(hash2, 120..400).
    pub filter_freq: f32,
    /// Lowpass resonance. v4: map(hash3, 5..8).
    pub filter_q: f32,
    /// Velocity-to-noise scale. v4: map(hash2*hash3 % 100, 0.5..1).
    pub noise_gain_scale: f32,
    /// Major/minor chord flavor. v4: hash2 % 2 == 0 (a float quirk makes this
    /// true for all but very short names; ported faithfully).
    pub is_major: bool,
    /// Whether the white-noise voice is active. v4: hash3 % 100 >= 50.
    pub has_noise: bool,
    /// Hash-derived stand-in for v4's box-count density, in ~`[1, 3.2]`. See
    /// `pseudo_density` for the formula and the PARITY fallback seam.
    pub pseudo_density: f32,
    /// Chord register: v4's `clamp(floor(map(density, 1, 3, 0, 24)), 0, 48)`.
    pub chord_degree: f32,
}

/// Everything derived from a name: the branch set plus scalar drivers.
#[derive(Debug, Clone, PartialEq)]
pub struct FlameSpec {
    /// 2..=8 branches (see [`normalize_name`]).
    pub branches: Vec<BranchSpec>,
    /// Name-hash attractor offset, v4 `cY` in [-2.5, 2.5].
    pub c_y: f32,
    /// Name-derived audio character.
    pub audio: NameAudioConfig,
}

/// Trim the raw input; empty falls back to [`DEFAULT_NAME`]. Mirrors v4's
/// `FlameNameInput` (`trimmed || DEFAULT_NAME`), which is what makes a
/// 1-branch fractal unreachable.
#[must_use]
pub fn normalize_name(raw: &str) -> &str {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        DEFAULT_NAME
    } else {
        trimmed
    }
}

/// v4 `stringHash`, bit-for-bit: an i32-wrapping polynomial over UTF-16 code
/// units, then `hash *= hash * 31` in f64 (which overflows f64 precision for
/// long names — deliberately kept, the quirks are part of v4's output).
#[must_use]
pub fn string_hash(s: &str) -> f64 {
    let mut hash: i32 = 0;
    let mut any = false;
    for unit in s.encode_utf16() {
        any = true;
        // JS: hash = (hash * 31 + char) | 0  — i32 wrapping semantics.
        hash = hash.wrapping_mul(31).wrapping_add(i32::from(unit));
    }
    if !any {
        return 0.0;
    }
    let h = f64::from(hash);
    // JS: hash *= hash * 31 — pure f64 from here on.
    h * (h * 31.0)
}

/// One PRNG step: `gen = (gen * 4194303 + 127) % (2^31 - 1)` in f64,
/// matching JS exactly (both `*` and `%` are IEEE-exact / fmod).
pub(crate) fn prng_next(gen: &mut f64) -> f64 {
    *gen = (*gen * 4_194_303.0 + 127.0) % GEN_DIVISOR;
    *gen
}

/// v4 `map` (unclamped linear map) in f64.
fn map_f64(x: f64, x0: f64, x1: f64, y0: f64, y1: f64) -> f64 {
    y0 + (y1 - y0) * ((x - x0) / (x1 - x0))
}

/// JS `String.prototype.substring` over UTF-16 units with fractional f64
/// bounds: ToInteger truncation, clamp to length, swap if start > end.
fn substring_utf16(units: &[u16], start: f64, end: f64) -> Vec<u16> {
    let len = units.len();
    let to_index = |x: f64| -> usize {
        if x.is_nan() || x <= 0.0 {
            0
        } else {
            (x.trunc() as usize).min(len)
        }
    };
    let (mut a, mut b) = (to_index(start), to_index(end));
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    units[a..b].to_vec()
}

/// `string_hash` over pre-decoded UTF-16 units (substring seeds).
fn string_hash_units(units: &[u16]) -> f64 {
    if units.is_empty() {
        return 0.0;
    }
    let mut hash: i32 = 0;
    for &unit in units {
        hash = hash.wrapping_mul(31).wrapping_add(i32::from(unit));
    }
    let h = f64::from(hash);
    h * (h * 31.0)
}

/// v4 `randomBranch`, ported draw-for-draw. The PRNG draw ORDER is part of
/// the contract: skip loop, (affine reads gen without a draw), varA draw,
/// combinator draw(s) — the router probe draw happens ONLY when numWraps > 2
/// (JS `&&` short-circuit) — then three color draws.
fn random_branch(
    idx: usize,
    substring: &[u16],
    num_branches: usize,
    num_wraps: usize,
) -> BranchSpec {
    let mut gen = string_hash_units(substring);
    // Skip 5 + idx*numWraps draws (v4's per-branch decorrelation).
    for _ in 0..(5 + idx * num_wraps) {
        prng_next(&mut gen);
    }
    // Affine: uses gen as left by the skip loop (no extra draw).
    let affine_idx = (gen % 7.0) as usize;
    // varA: one draw, then gen % 7.
    prng_next(&mut gen);
    let var_a = VariationId::TABLE[(gen % 7.0) as usize];

    let mut mode = VariationMode::Single;
    let mut var_b = var_a;
    // Combinator selection, preserving v4's draw order and short-circuit.
    prng_next(&mut gen);
    let interp_roll = gen / GEN_DIVISOR;
    if interp_roll < num_wraps as f64 * 0.25 {
        mode = VariationMode::Interpolated;
        prng_next(&mut gen);
        var_b = VariationId::TABLE[(gen % 7.0) as usize];
    } else if num_wraps > 2 {
        prng_next(&mut gen);
        let router_roll = gen / GEN_DIVISOR;
        if router_roll < 0.2 {
            mode = VariationMode::Router;
            prng_next(&mut gen);
            var_b = VariationId::TABLE[(gen % 7.0) as usize];
        }
    }

    // Three color draws in [-0.05, 0.05), focus channel +0.2, scaled.
    let mut color = [0.0_f64; 3];
    for c in &mut color {
        prng_next(&mut gen);
        *c = (gen / GEN_DIVISOR) * 0.1 - 0.05;
    }
    color[idx % 3] += 0.2;
    let scale = num_branches as f64 / 3.5;
    BranchSpec {
        affine_idx,
        var_a,
        var_b,
        mode,
        color: [
            (color[0] * scale) as f32,
            (color[1] * scale) as f32,
            (color[2] * scale) as f32,
        ],
    }
}

/// Hash-derived stand-in for v4's box-count density (see the spec's Audio
/// section). Branch count dominates; contractive variations raise it, spread
/// variations lower it. Ear-tunable; the documented fallback seam is a
/// one-shot ~2k-point CPU evaluation + box-count at name-change only.
fn pseudo_density(branches: &[BranchSpec]) -> f32 {
    // Per-variation "contractiveness" weight, judged from the maps' effect on
    // typical |p| ~ 1 points: Shrink and Spherical pull hard toward compact
    // clusters; Polar and Normalize spread onto shells/sheets.
    fn weight(v: VariationId) -> f32 {
        match v {
            VariationId::Shrink => 1.0,
            VariationId::Spherical => 0.9,
            VariationId::Sin => 0.7,
            VariationId::Linear | VariationId::Swirl => 0.5,
            VariationId::Polar => 0.4,
            VariationId::Normalize => 0.3,
        }
    }
    let contract: f32 = branches
        .iter()
        .map(|b| match b.mode {
            VariationMode::Single => weight(b.var_a),
            _ => (weight(b.var_a) + weight(b.var_b)) * 0.5,
        })
        .sum::<f32>()
        / branches.len() as f32;
    let b = branches.len() as f32;
    // 2 branches, contract 0.3 -> 1.18 ; 8 branches, contract 1.0 -> 3.0.
    1.0 + 1.4 * ((b - 2.0) / 6.0) + 0.6 * contract
}

/// Build the full spec for a (pre-normalized or raw) name. Applies
/// [`normalize_name`] internally so callers can pass raw input.
#[must_use]
pub fn build_flame_spec(name: &str) -> FlameSpec {
    let name = normalize_name(name);
    let units: Vec<u16> = name.encode_utf16().collect();
    let len = units.len();
    let num_wraps = len / 5;
    // ceil() of an integer-valued f64 is itself; kept for v4 shape.
    let num_branches = (1.0 + (len % 5) as f64 + num_wraps as f64).ceil() as usize;

    let mut branches = Vec::with_capacity(num_branches);
    for i in 0..num_branches {
        let start = map_f64(i as f64, 0.0, num_branches as f64, 0.0, len as f64);
        let end = map_f64((i + 1) as f64, 0.0, num_branches as f64, 0.0, len as f64);
        let sub = substring_utf16(&units, start, end);
        branches.push(random_branch(i, &sub, num_branches, num_wraps));
    }
    debug_assert!(
        (2..=MAX_BRANCHES).contains(&branches.len()),
        "normalize_name guarantees 2..=8 branches"
    );

    // Audio character, v4 `updateName` + `configureForName` in f64.
    let hash = string_hash(name);
    let hash_norm = (hash % 1024.0) / 1024.0;
    let hash2 = hash * hash + hash * 31.0 + 9.0;
    let hash3 = hash2 * hash2 + hash2 * 31.0 + 9.0;
    let density = pseudo_density(&branches);
    // v4: clamp(floor(map(density, 1, 3, 0, 24)), 0, 48).
    let chord_degree = map_f64(f64::from(density), 1.0, 3.0, 0.0, 24.0)
        .floor()
        .clamp(0.0, 48.0) as f32;
    let audio = NameAudioConfig {
        filter_freq: map_f64((hash2 % 2e12) / 2e12, 0.0, 1.0, 120.0, 400.0) as f32,
        filter_q: map_f64((hash3 % 2e12) / 2e12, 0.0, 1.0, 5.0, 8.0) as f32,
        noise_gain_scale: map_f64(((hash2 * hash3) % 100.0) / 100.0, 0.0, 1.0, 0.5, 1.0) as f32,
        is_major: hash2 % 2.0 == 0.0,
        has_noise: (hash3 % 100.0) >= 50.0,
        pseudo_density: density,
        chord_degree,
    };

    FlameSpec {
        branches,
        c_y: map_f64(hash_norm, 0.0, 1.0, -2.5, 2.5) as f32,
        audio,
    }
}

/// CPU mirror of the WGSL variation switch. Zero-length guards match
/// THREE.js (`normalize`/`setLength` divide by `length || 1`).
#[must_use]
pub fn apply_variation_cpu(id: VariationId, p: [f32; 3]) -> [f32; 3] {
    let len_sq = p[0] * p[0] + p[1] * p[1] + p[2] * p[2];
    match id {
        VariationId::Linear => p,
        VariationId::Sin => [p[0].sin(), p[1].sin(), p[2].sin()],
        VariationId::Spherical => {
            if len_sq == 0.0 {
                p
            } else {
                [p[0] / len_sq, p[1] / len_sq, p[2] / len_sq]
            }
        }
        VariationId::Polar => [
            p[1].atan2(p[0]) / std::f32::consts::PI,
            len_sq.sqrt() - 1.0,
            p[2].atan2(p[0]),
        ],
        VariationId::Swirl => {
            let (s, c) = (len_sq.sin(), len_sq.cos());
            [
                p[2] * s - p[1] * c,
                p[0] * c + p[2] * s,
                p[0] * s - p[1] * s,
            ]
        }
        VariationId::Normalize => {
            if len_sq == 0.0 {
                p
            } else {
                let inv = 1.0 / len_sq.sqrt();
                [p[0] * inv, p[1] * inv, p[2] * inv]
            }
        }
        VariationId::Shrink => {
            if len_sq == 0.0 {
                p
            } else {
                let scale = (-len_sq).exp() / len_sq.sqrt();
                [p[0] * scale, p[1] * scale, p[2] * scale]
            }
        }
    }
}

/// CPU mirror of the full per-node branch application: affine matrix+offset,
/// then the per-frame warp added to x/y, then the variation combinator.
/// Mirrors the WGSL kernel term-for-term (kernel parity discipline).
#[must_use]
pub fn apply_branch_cpu(spec: &BranchSpec, warp: [f32; 2], p: [f32; 3]) -> [f32; 3] {
    let m = &AFFINE_MATS[spec.affine_idx];
    let o = &AFFINE_OFFSETS[spec.affine_idx];
    let affine = [
        m[0] * p[0] + m[1] * p[1] + m[2] * p[2] + o[0] + warp[0],
        m[3] * p[0] + m[4] * p[1] + m[5] * p[2] + o[1] + warp[1],
        m[6] * p[0] + m[7] * p[1] + m[8] * p[2] + o[2],
    ];
    match spec.mode {
        VariationMode::Single => apply_variation_cpu(spec.var_a, affine),
        VariationMode::Interpolated => {
            let a = apply_variation_cpu(spec.var_a, affine);
            let b = apply_variation_cpu(spec.var_b, affine);
            [
                a[0] + (b[0] - a[0]) * 0.5,
                a[1] + (b[1] - a[1]) * 0.5,
                a[2] + (b[2] - a[2]) * 0.5,
            ]
        }
        VariationMode::Router => {
            if affine[2] < 0.0 {
                apply_variation_cpu(spec.var_a, affine)
            } else {
                apply_variation_cpu(spec.var_b, affine)
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "golden parity with v4 requires bit-exact f64/f32 comparison"
)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;

    /// PRNG golden: seed string "who ", first 8 draws. Values generated from
    /// the v4 source via docs/superpowers/plans/assets/2026-07-02-flame-goldens.mjs.
    #[test]
    fn prng_matches_v4_sequence() {
        assert_eq!(string_hash("who "), 412_668_525_337_596.0);
        let mut gen = string_hash("who ");
        let expected: [f64; 8] = [
            1_192_329_537.0,
            156_370_942.0,
            1_983_029_636.0,
            1_795_717_194.0,
            665_652_336.0,
            1_952_893_588.0,
            819_161_423.0,
            587_530_468.0,
        ];
        for want in expected {
            assert_eq!(prng_next(&mut gen), want);
        }
    }

    /// stringHash edge cases: empty string is 0; the int32 wrap and the final
    /// f64 squaring both match v4 exactly.
    #[test]
    fn string_hash_matches_v4() {
        assert_eq!(string_hash(""), 0.0);
        assert_eq!(string_hash("a"), 291_679.0);
        assert_eq!(string_hash("xy"), 457_351_711.0);
        assert_eq!(string_hash("who are you?"), 7_885_686_694_543_608_000.0);
    }

    #[test]
    fn normalize_name_trims_and_defaults() {
        assert_eq!(normalize_name("  madison  "), "madison");
        assert_eq!(normalize_name(""), DEFAULT_NAME);
        assert_eq!(normalize_name("   "), DEFAULT_NAME);
    }

    /// Full branch-generation golden for the default name. Asserts branch
    /// count, per-branch affine/variation selection, combinator mode, and
    /// bit-exact colors (f32-rounded from the v4 f64 values).
    #[test]
    fn default_name_branches_match_v4() {
        let spec = build_flame_spec("who are you?");
        assert_eq!(spec.branches.len(), 5);
        assert_eq!(spec.c_y, -2.5);

        let b0 = &spec.branches[0];
        assert_eq!(b0.affine_idx, 4); // Negate
        assert_eq!(b0.var_a, VariationId::Spherical);
        assert_eq!(b0.mode, VariationMode::Interpolated);
        assert_eq!(b0.var_b, VariationId::Sin);
        assert_eq!(
            b0.color,
            [
                0.294_986_865_121_665_55_f64 as f32,
                0.007_626_119_904_844_28_f64 as f32,
                -0.028_119_029_190_978_92_f64 as f32,
            ]
        );

        let b2 = &spec.branches[2];
        assert_eq!(b2.affine_idx, 1); // TowardsOrigin2
        assert_eq!(b2.var_a, VariationId::Polar);
        assert_eq!(b2.mode, VariationMode::Single);

        let b4 = &spec.branches[4];
        assert_eq!(b4.affine_idx, 5); // NegateSwap
        assert_eq!(b4.var_a, VariationId::Normalize);
    }

    /// The 19-char name exercises the router combinator (numWraps > 2) and the
    /// 8-branch maximum.
    #[test]
    fn nineteen_char_name_hits_router_and_max_branches() {
        let spec = build_flame_spec("abcdefghijklmnopqrs");
        assert_eq!(spec.branches.len(), 8);
        let b1 = &spec.branches[1]; // substring "cd"
        assert_eq!(b1.affine_idx, 5); // NegateSwap
        assert_eq!(b1.var_a, VariationId::Normalize);
        assert_eq!(b1.mode, VariationMode::Router);
        assert_eq!(b1.var_b, VariationId::Linear);
    }

    /// Branch counts are always 2..=8 for any non-empty trimmed name (the v4
    /// name input substitutes the default for empty, so 1 branch is unreachable).
    #[test]
    fn branch_count_bounds() {
        for name in [
            "a",
            "ab",
            "abcd",
            "abcde",
            "abcdefghij",
            "abcdefghijklmnopqrst",
        ] {
            let n = build_flame_spec(name).branches.len();
            assert!((2..=8).contains(&n), "{name}: {n} branches");
        }
    }

    /// Audio config goldens (f64 hash math), including the two v4 float
    /// quirks: cY collapses to -2.5 for long names, is_major true for all but
    /// tiny names.
    #[test]
    fn audio_config_matches_v4() {
        let who = build_flame_spec("who are you?").audio;
        assert_eq!(who.filter_freq, 173.668_575_313_92_f64 as f32);
        assert_eq!(who.filter_q, 5.278_918_295_552_f64 as f32);
        assert_eq!(who.noise_gain_scale, 0.7_f64 as f32);
        assert!(who.is_major);
        assert!(who.has_noise);

        let a = build_flame_spec("a").audio;
        assert!(!a.is_major, "short-name hash math is exact; 'a' is minor");
        assert!(!a.has_noise);

        let xiaohan = build_flame_spec("Xiaohan").audio;
        assert_eq!(xiaohan.filter_freq, 330.091_306_844_160_04_f64 as f32);
    }

    /// cY: short names get varied values (exact math), long names collapse to
    /// -2.5 (hash is a multiple of 1024 once the double exceeds ~2^53).
    #[test]
    fn c_y_matches_v4_including_float_quirk() {
        assert_eq!(build_flame_spec("a").c_y, 1.713_867_187_5);
        assert_eq!(build_flame_spec("xy").c_y, 0.151_367_187_5);
        assert_eq!(build_flame_spec("madison").c_y, -2.5);
        assert_eq!(build_flame_spec("who are you?").c_y, -2.5);
    }

    /// Affine tables must equal v4's closed forms. Spot-check each of the 7
    /// affines by applying matrix+offset to a probe point and comparing with
    /// the hand-derived v4 expression.
    #[test]
    fn affine_tables_match_v4_formulas() {
        let p = [0.3_f32, -0.7, 1.1];
        let apply = |idx: usize| -> [f32; 3] {
            let m = &AFFINE_MATS[idx];
            let o = &AFFINE_OFFSETS[idx];
            [
                m[0] * p[0] + m[1] * p[1] + m[2] * p[2] + o[0],
                m[3] * p[0] + m[4] * p[1] + m[5] * p[2] + o[1],
                m[6] * p[0] + m[7] * p[1] + m[8] * p[2] + o[2],
            ]
        };
        // 0 TowardsOriginNegativeBias: ((x-1)/2 + 0.25, (y-1)/2, z/2)
        let got = apply(0);
        assert!((got[0] - ((p[0] - 1.0) / 2.0 + 0.25)).abs() < 1e-7);
        assert!((got[1] - (p[1] - 1.0) / 2.0).abs() < 1e-7);
        assert!((got[2] - p[2] / 2.0).abs() < 1e-7);
        // 2 Swap: ((y+z)/2.5, (x+z)/2.5, (x+y)/2.5)
        let got = apply(2);
        assert!((got[0] - (p[1] + p[2]) / 2.5).abs() < 1e-7);
        assert!((got[1] - (p[0] + p[2]) / 2.5).abs() < 1e-7);
        assert!((got[2] - (p[0] + p[1]) / 2.5).abs() < 1e-7);
        // 3 SwapSub: ((y-z)/2, (z-x)/2, (x-y)/2)
        let got = apply(3);
        assert!((got[0] - (p[1] - p[2]) / 2.0).abs() < 1e-7);
        assert!((got[1] - (p[2] - p[0]) / 2.0).abs() < 1e-7);
        assert!((got[2] - (p[0] - p[1]) / 2.0).abs() < 1e-7);
        // 4 Negate: (-x, -y, -z)
        assert_eq!(apply(4), [-p[0], -p[1], -p[2]]);
        // 5 NegateSwap: ((-x+y+z)/2.1, (-y+x+z)/2.1, (-z+x+y)/2.1)
        let got = apply(5);
        assert!((got[0] - (-p[0] + p[1] + p[2]) / 2.1).abs() < 1e-7);
        // 6 Up1: (x, y, z+1)
        assert_eq!(apply(6), [p[0], p[1], p[2] + 1.0]);
        // 1 TowardsOrigin2: ((x+1)/2, (y-1)/2 - 0.1, (z+1)/2 - 0.1)
        let got = apply(1);
        assert!((got[0] - (p[0] + 1.0) / 2.0).abs() < 1e-7);
        assert!((got[1] - ((p[1] - 1.0) / 2.0 - 0.1)).abs() < 1e-7);
        // f32 rounding: the table evaluates z/2 + 0.4, the check evaluates
        // (z+1)/2 - 0.1; algebraically identical, ~1.2e-7 apart in f32.
        assert!((got[2] - ((p[2] + 1.0) / 2.0 - 0.1)).abs() < 1e-6);
    }

    /// CPU variation mirror matches v4's formulas, including the zero-length
    /// guards THREE.js applies (normalize/setLength of a zero vector is a no-op).
    #[test]
    fn variations_match_v4_formulas() {
        let p = [0.5_f32, -0.25, 0.75];
        // Sin
        let got = apply_variation_cpu(VariationId::Sin, p);
        assert_eq!(got, [p[0].sin(), p[1].sin(), p[2].sin()]);
        // Spherical: p / |p|^2, zero-safe
        let l2 = p[0] * p[0] + p[1] * p[1] + p[2] * p[2];
        let got = apply_variation_cpu(VariationId::Spherical, p);
        assert!((got[0] - p[0] / l2).abs() < 1e-7);
        assert_eq!(
            apply_variation_cpu(VariationId::Spherical, [0.0; 3]),
            [0.0; 3]
        );
        // Polar: (atan2(y,x)/pi, |p| - 1, atan2(z,x))
        let got = apply_variation_cpu(VariationId::Polar, p);
        assert!((got[0] - p[1].atan2(p[0]) / std::f32::consts::PI).abs() < 1e-7);
        assert!((got[1] - (l2.sqrt() - 1.0)).abs() < 1e-7);
        assert!((got[2] - p[2].atan2(p[0])).abs() < 1e-7);
        // Swirl
        let r2 = l2;
        let got = apply_variation_cpu(VariationId::Swirl, p);
        assert!((got[0] - (p[2] * r2.sin() - p[1] * r2.cos())).abs() < 1e-6);
        assert!((got[1] - (p[0] * r2.cos() + p[2] * r2.sin())).abs() < 1e-6);
        assert!((got[2] - (p[0] * r2.sin() - p[1] * r2.sin())).abs() < 1e-6);
        // Normalize, zero-safe
        let got = apply_variation_cpu(VariationId::Normalize, p);
        let len = l2.sqrt();
        assert!((got[0] - p[0] / len).abs() < 1e-7);
        assert_eq!(
            apply_variation_cpu(VariationId::Normalize, [0.0; 3]),
            [0.0; 3]
        );
        // Shrink: setLength(exp(-|p|^2)), zero-safe
        let got = apply_variation_cpu(VariationId::Shrink, p);
        let want_len = (-l2).exp();
        let got_len = (got[0] * got[0] + got[1] * got[1] + got[2] * got[2]).sqrt();
        assert!((got_len - want_len).abs() < 1e-6);
        assert_eq!(apply_variation_cpu(VariationId::Shrink, [0.0; 3]), [0.0; 3]);
        // Linear: identity
        assert_eq!(apply_variation_cpu(VariationId::Linear, p), p);
    }

    /// apply_branch_cpu = affine -> +warp on x/y -> variation, matching v4's
    /// randomBranch closure order (warp is added AFTER the base affine and
    /// BEFORE the variation).
    #[test]
    fn apply_branch_order_is_affine_warp_variation() {
        let spec = BranchSpec {
            affine_idx: 6, // Up1: identity + (0,0,1)
            var_a: VariationId::Sin,
            var_b: VariationId::Sin,
            mode: VariationMode::Single,
            color: [0.0; 3],
        };
        let warp = [0.4_f32, -0.3];
        let p = [0.2_f32, 0.5, -0.1];
        let got = apply_branch_cpu(&spec, warp, p);
        let expect = [
            (p[0] + warp[0]).sin(),
            (p[1] + warp[1]).sin(),
            (p[2] + 1.0).sin(),
        ];
        assert_eq!(got, expect);
    }

    /// Pseudo-density is deterministic per name, within `[1, 3.2]`, and ranks a
    /// dense many-branch name above a sparse two-branch one.
    #[test]
    fn pseudo_density_is_bounded_and_monotonic_in_branches() {
        let lo = build_flame_spec("a").audio.pseudo_density;
        let hi = build_flame_spec("abcdefghijklmnopqrs").audio.pseudo_density;
        assert!((1.0..=3.2).contains(&lo));
        assert!((1.0..=3.2).contains(&hi));
        assert!(hi > lo, "8 branches must read denser than 2");
        // chord_degree follows v4's mapping shape from density.
        let cd = build_flame_spec("madison").audio.chord_degree;
        assert!((0.0..=48.0).contains(&cd));
    }
}
