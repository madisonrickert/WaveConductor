// Cymatics fullscreen render -- ports v4 renderCymatics.frag.
//
// Samples the ping-pong cell texture A (rgba32float) via textureLoad (integer
// texel coordinates, no sampler) to avoid the float32-filterable WebGPU
// feature that linear sampling of 32-bit-float textures would require. Linear
// smoothing is reproduced by hand in `sample_height_bilinear` (a 2x2 textureLoad
// + lerp), matching the bilinear v4 got from its LinearFilter sampler without
// binding one. A holds the latest field at frame end (the compute node's odd-N
// continuity refresh), so the material samples it directly with no separate
// display texture.
//
// Builds a height-gradient surface normal (central difference of abs(height)
// at ±1 texel, scaled to UV space; matches v4 halfTexelScaleX/Y) and applies
// two directional lights with power-8 specular. Mixes BASE_COL / BASE_BODY_COL
// by abs(height)*3, adds a vignette and radial background, and the
// skewIntensity body push. All v4 constants reproduced exactly.
//
// Mesh UV [0, 1] is used as the screen coordinate (v4 used gl_FragCoord /
// resolution). Y-convention: Bevy mesh UV is top-left origin, matching
// simulate.wgsl's top-left origin. The final vertical orientation relative to
// v4 is confirmed in the Stage 8 visual capture -- no pre-correction here.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

// Packed uniforms following the flat-field idiom in particles/material.rs.
// .xy = screen_resolution (px, for AR correction and vignette),
// .zw = sim_resolution (texels, for UV-to-texel conversion and gradient scale).
@group(2) @binding(0) var<uniform> resolution: vec4<f32>;
// .x = skewIntensity (v4 body-colour push toward white),
// .y = master_brightness (post-render multiplier; 1.0 = no-op, default),
// .z = user gamma (per-channel display gamma; 1.0 = identity, default),
// .w = reserved (0).
@group(2) @binding(1) var<uniform> skew: vec4<f32>;
// Cell texture A (rgba32float): channel x = height, y = velocity,
// z = accumulated_height, w = unused (simulate.wgsl write contract).
// Read via textureLoad only -- no sampler binding declared.
@group(2) @binding(2) var cell_tex: texture_2d<f32>;

// v4 verbatim colour constants (no rounding).
const BASE_COL: vec3<f32>      = vec3<f32>(4.0, 32.0, 55.0) / 255.0;
const BASE_BODY_COL: vec3<f32> = vec3<f32>(235.0, 89.0, 56.0) / 255.0;
const LIGHT_1_COL: vec3<f32>   = vec3<f32>(254.0, 253.0, 255.0) / 255.0;
const LIGHT_2_COL: vec3<f32>   = vec3<f32>(170.0, 89.0, 57.0) / 255.0;
const LIGHT_1_BRIGHTNESS: f32  = 0.6;
const LIGHT_2_BRIGHTNESS: f32  = 0.3;
// v4: normalize(vec3(-1, -1, 0.3)) and normalize(vec3(-0.7, -1, 0.4)).
const LIGHT_1_DIR: vec3<f32> = normalize(vec3<f32>(-1.0, -1.0, 0.3));
const LIGHT_2_DIR: vec3<f32> = normalize(vec3<f32>(-0.7, -1.0, 0.4));

// Clamp integer texel coords to [0, dims - 1] (v4 used ClampToEdge wrap).
fn clamp_texel(t: vec2<i32>, dims: vec2<i32>) -> vec2<i32> {
    return clamp(t, vec2<i32>(0), dims - vec2<i32>(1));
}

// Manual bilinear height fetch reproducing v4's LinearFilter sampler.
//
// Texture A is `rgba32float`; linear-filtering 32-bit-float textures needs the
// `float32-filterable` WebGPU feature this project does not depend on, so we
// cannot bind a real linear sampler. We instead do the 2x2 fetch + lerp by hand
// with `textureLoad`, which is exactly the bilinear v4 got for free from its
// `LinearFilter` sampler (v4 index.ts:176-180). The half-texel shift (`- 0.5`)
// places the four taps on the surrounding texel *centres* so the interpolation
// is gradient-continuous; nearest `textureLoad` snaps to texel boundaries and
// shimmers the tightly-packed rings across the fullscreen quad.
//
// Reads channel x (height), matching every height tap in `cymatics_color`.
// ClampToEdge via `clamp_texel` mirrors v4's wrap mode at the grid border.
fn sample_height_bilinear(uv: vec2<f32>, sim_res: vec2<f32>, dims: vec2<i32>) -> f32 {
    let p = uv * sim_res - vec2<f32>(0.5);
    let base = floor(p);
    let f = p - base;
    let b = vec2<i32>(base);
    let h00 = textureLoad(cell_tex, clamp_texel(b + vec2<i32>(0, 0), dims), 0).x;
    let h10 = textureLoad(cell_tex, clamp_texel(b + vec2<i32>(1, 0), dims), 0).x;
    let h01 = textureLoad(cell_tex, clamp_texel(b + vec2<i32>(0, 1), dims), 0).x;
    let h11 = textureLoad(cell_tex, clamp_texel(b + vec2<i32>(1, 1), dims), 0).x;
    return mix(mix(h00, h10, f.x), mix(h01, h11, f.x), f.y);
}

// Height-gradient surface normal, two-light power-8 specular, and body mix.
// Ports v4 `color()` verbatim. All arithmetic matches v4 line-for-line.
//
// Normal derivation: central difference of abs(height) at ±1 texel, scaled
// to UV space. v4: gradHeightAccX/Y using halfTexelScaleX = 0.5/cellOffset.x
// = 0.5 * cellStateResolution.x; here ±1 texel offset, so the same scale.
//
// Specular: max(0, dot(N, L))^8 via 3 squarings (v4 comment: "optimized
// version vs using pow()"). Each light multiplied by its brightness constant.
//
// Body colour: mix(BASE_COL, mix(BASE_BODY_COL, vec3(1), skewIntensity),
//              abs(height) * 3).
fn cymatics_color(uv: vec2<f32>) -> vec3<f32> {
    let sim_res = resolution.zw;
    let dims = vec2<i32>(i32(sim_res.x), i32(sim_res.y));

    // One-texel step in UV space (v4's neighbour offset was ±1 texel).
    let texel = vec2<f32>(1.0) / sim_res;

    // Centre height (channel x), bilinear-sampled to match v4's LinearFilter.
    let height = sample_height_bilinear(uv, sim_res, dims);

    // Neighbour absolute heights for the central-difference gradient, each
    // bilinear-sampled at uv ± one texel. v4 read abs(.xz) (height and
    // accumulated) but used only .x; we read .x and abs *after* interpolation,
    // exactly as v4 did on top of its filtered fetch.
    let hpx = abs(sample_height_bilinear(uv + vec2<f32>(texel.x, 0.0), sim_res, dims));
    let hmx = abs(sample_height_bilinear(uv - vec2<f32>(texel.x, 0.0), sim_res, dims));
    let hpy = abs(sample_height_bilinear(uv + vec2<f32>(0.0, texel.y), sim_res, dims));
    let hmy = abs(sample_height_bilinear(uv - vec2<f32>(0.0, texel.y), sim_res, dims));

    // Gradient scale: v4 halfTexelScaleX = 0.5 / cellOffset.x = 0.5 * resolution.x.
    // ±1 texel = 1/sim_res in UV, central difference / (2 * 1/sim_res) = sim_res * 0.5.
    let half_scale = sim_res * 0.5;
    let grad_x = (hpx - hmx) * half_scale.x;
    let grad_y = (hpy - hmy) * half_scale.y;

    // Surface normal: height gradient in x/y, z = 1 faces the viewer.
    // v4: normalize(vec3(gradHeightX, gradHeightY, 1.0)).
    let normal = normalize(vec3<f32>(grad_x, grad_y, 1.0));

    // Light 1: cool white at 0.6 brightness.
    var s1 = max(0.0, dot(normal, LIGHT_1_DIR));
    s1 *= s1; s1 *= s1; s1 *= s1;   // ^8: 3 squarings
    s1 *= LIGHT_1_BRIGHTNESS;

    // Light 2: warm orange at 0.3 brightness.
    var s2 = max(0.0, dot(normal, LIGHT_2_DIR));
    s2 *= s2; s2 *= s2; s2 *= s2;   // ^8
    s2 *= LIGHT_2_BRIGHTNESS;

    // v4 heightFactor = abs(height) * 3; bodyColor = mix(BASE_BODY_COL, vec3(1), skewIntensity).
    let height_factor = abs(height) * 3.0;
    let body = mix(BASE_BODY_COL, vec3<f32>(1.0), skew.x);
    var col = mix(BASE_COL, body, height_factor);
    col += s1 * LIGHT_1_COL;
    col += s2 * LIGHT_2_COL;
    return clamp(col, vec3<f32>(0.0), vec3<f32>(1.0));
}

// v4 udRoundBox: unsigned distance to a rounded rectangle (negative inside).
fn ud_round_box(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    return length(max(abs(p) - b, vec2<f32>(0.0))) - r;
}

// Accurate piecewise sRGB EOTF (sRGB display value -> linear).
//
// Why this is the final output step: this material renders through the global
// HDR `Camera2d`, whose intermediate target is linear `Rgba16Float`. Bevy's
// tonemapping pass (here `Tonemapping::None`, bypassed for Cymatics) treats its
// input as a *linear* stimulus and writes to the sRGB swapchain, where the
// hardware applies the sRGB OETF (linear -> sRGB) on store at present time.
//
// But the colour constants above (`BASE_COL = vec3(4, 32, 55) / 255`, etc.) are
// authored as sRGB *display* bytes -- the exact values v4 wrote straight to its
// (already-sRGB) canvas. Writing those sRGB-authored values into the linear HDR
// target and letting the present-time OETF re-encode them would sRGB-encode the
// colour a SECOND time, lifting and washing out the blacks (deep blue -> pale
// steel-blue). Applying this EOTF as the very last op makes the round-trip an
// identity -- OETF(EOTF(c)) == c -- so the presented pixels equal v4's display
// colours exactly. Must stay the final step, after every display-referred trim.
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // Shift mesh UV [0, 1] to screen-centred [-0.5, 0.5].
    // v4: screenCoord = gl_FragCoord.xy / resolution - vec2(0.5).
    let screen_coord = in.uv - vec2<f32>(0.5);

    // Aspect-correct to sim UV space.
    // v4: normCoord = screenCoord * vec2(screenAR / simAR, 1.0).
    let screen_ar = resolution.x / resolution.y;
    let sim_ar    = resolution.z / resolution.w;
    let norm_coord = screen_coord * vec2<f32>(screen_ar / sim_ar, 1.0);

    // Re-centre to [0, 1] sim UV for the texture lookup.
    // v4: uv = normCoord + vec2(0.5).
    let uv = norm_coord + vec2<f32>(0.5);
    let cymatics = cymatics_color(uv);

    // Vignette: 0 at centre (full cymatics), 1 at border (full background).
    // v4: 1 - clamp(-udRoundBox(screenCoord, vec2(0.45), 0.05) * 40, 0, 1).
    let vignette = 1.0 - clamp(
        -ud_round_box(screen_coord, vec2<f32>(0.45), 0.05) * 40.0,
        0.0, 1.0,
    );

    // Radial dark background: v4 colBg = vec3(0.25 - length(normCoord) * 0.2).
    let bg = vec3<f32>(0.25 - length(norm_coord) * 0.2);

    // v4: mix(pow(cymaticsColor, vec3(mix(0.8, 1., vignetteAmount))), colBg, vignetteAmount).
    // Gamma 0.8 brightens the centre; gamma 1.0 at the edge before bg blend.
    var col = mix(pow(cymatics, vec3<f32>(mix(0.8, 1.0, vignette))), bg, vignette);
    // skew.y = master_brightness (User setting, default 1.0 = no-op). Applied
    // after the vignette blend so it uniformly scales the whole output frame.
    col = col * skew.y;
    // skew.z = user gamma (User setting; default 1.0 = identity, mirrors
    // Line/Dots). Display-referred, so it applies after master_brightness and
    // before the sRGB linearise. `gamma` is a uniform, so this is a uniform
    // branch (no warp divergence); at 1.0 pow is the identity, skip it. Clamp
    // >= 0 first so pow is well-defined on any underflowed negative.
    if skew.z != 1.0 {
        col = pow(max(col, vec3<f32>(0.0)), vec3<f32>(skew.z));
    }
    // Linearise the sRGB-authored colour as the final op so Bevy's present-time
    // sRGB encode round-trips to v4's exact display values (see srgb_to_linear).
    return vec4<f32>(srgb_to_linear(col), 1.0);
}
