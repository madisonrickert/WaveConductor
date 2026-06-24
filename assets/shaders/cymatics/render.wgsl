// Cymatics fullscreen render -- ports v4 renderCymatics.frag.
//
// Samples the display cell texture (rgba32float) via textureLoad (integer
// texel coordinates, no sampler) to avoid the float32-filterable WebGPU
// feature that linear sampling of 32-bit-float textures would require.
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
// .zw = 0.
@group(2) @binding(1) var<uniform> skew: vec4<f32>;
// Display texture (rgba32float): channel x = height, y = velocity,
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

    // Convert [0, 1] UV to integer texel, clamped inside the grid.
    let t = clamp_texel(vec2<i32>(uv * sim_res), dims);

    // Centre texel: height in channel x.
    let height = textureLoad(cell_tex, t, 0).x;

    // Neighbour absolute heights for central-difference gradient.
    // v4 reads abs(.xz) (both height and accumulated), then uses only .x.
    let hpx = abs(textureLoad(cell_tex, clamp_texel(t + vec2<i32>( 1,  0), dims), 0).x);
    let hmx = abs(textureLoad(cell_tex, clamp_texel(t + vec2<i32>(-1,  0), dims), 0).x);
    let hpy = abs(textureLoad(cell_tex, clamp_texel(t + vec2<i32>( 0,  1), dims), 0).x);
    let hmy = abs(textureLoad(cell_tex, clamp_texel(t + vec2<i32>( 0, -1), dims), 0).x);

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
    let col = mix(pow(cymatics, vec3<f32>(mix(0.8, 1.0, vignette))), bg, vignette);
    // skew.y = master_brightness (User setting, default 1.0 = no-op). Applied
    // after the vignette blend so it uniformly scales the whole output frame.
    return vec4<f32>(col * skew.y, 1.0);
}
