// Radiance silhouette fill — a window-filling quad under the particles,
// sampling the 256x256 R8Unorm person mask: a smoothstep-edged dark glassy
// body fill (deep translucent vertical gradient + audio-shimmered value
// noise) and a thin emissive rim in the mask's edge band. The rim color is
// HDR (palette-derived, scaled by rim glow) so it blooms; the fill is dark
// and mostly occludes via ordinary alpha blending.
//
// Bindings (group 2):
//   @binding(0)/(1): mask texture + sampler (R8Unorm is filterable).
//   @binding(2): fill_params — x fill intensity, y rim glow, z mask
//                threshold, w mirror (1 = flip x).
//   @binding(3): effect_params — x elapsed seconds, y shimmer amount
//                (highs-driven), z raw-mask debug (1 = draw the mask
//                grayscale), w fit-to-height aspect (window_w/window_h; 1 =
//                full-window stretch).
//   @binding(4): fill_color — deep glassy base (linear).
//   @binding(5): rim_color — emissive rim (linear HDR).

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

@group(2) @binding(0) var mask_tex: texture_2d<f32>;
@group(2) @binding(1) var mask_samp: sampler;
@group(2) @binding(2) var<uniform> fill_params: vec4<f32>;
@group(2) @binding(3) var<uniform> effect_params: vec4<f32>;
@group(2) @binding(4) var<uniform> fill_color: vec4<f32>;
@group(2) @binding(5) var<uniform> rim_color: vec4<f32>;

// 2D hash -> [0, 1) (Hoskins-style, texture-free).
fn hash21(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.xyx) * 0.1031);
    p3 = p3 + dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

// Smooth bilinear value noise over the hash lattice.
fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash21(i);
    let b = hash21(i + vec2<f32>(1.0, 0.0));
    let c = hash21(i + vec2<f32>(0.0, 1.0));
    let d = hash21(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // Quad UV -> mask UV. The mirror flip matches the kernel's mask_uv_to_world
    // so fill, rim, and particle spawns agree. effect_params.w is the fit-to-
    // height aspect factor (window_w/window_h; 1.0 = full-window stretch):
    // scaling u about the centre maps the square mask to a centred, height-tall
    // square so the dancer keeps its proportions on non-square displays.
    var uv = in.uv;
    if (fill_params.w > 0.5) {
        uv.x = 1.0 - uv.x;
    }
    uv.x = 0.5 + (uv.x - 0.5) * effect_params.w;
    // Outside the mask reads as background — the pillarbox on a wide screen. On
    // a portrait screen u stays within [0,1], so the dancer fills the height and
    // the aura is cropped at the sides instead.
    if (uv.x < 0.0 || uv.x > 1.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    let m = textureSample(mask_tex, mask_samp, uv).r;

    // Dev isolation: raw mask grayscale (mask_debug_overlay).
    if (effect_params.z > 0.5) {
        return vec4<f32>(m, m, m, 1.0);
    }

    let th = fill_params.z;
    // Soft body coverage around the threshold; the 256^2 mask is
    // impressionistic by design (aura, not cutout).
    let body = smoothstep(th - 0.06, th + 0.06, m);

    // Dark glassy fill: deep base hue, brighter toward the top (a glass
    // sheen), shimmered by slow-scrolling value noise whose amplitude rides
    // the high-band audio drive.
    let noise = value_noise(uv * 9.0 + vec2<f32>(0.0, effect_params.x * 0.15));
    let shimmer = 1.0 + effect_params.y * 0.5 * (noise - 0.5);
    let glass = fill_color.rgb * mix(1.25, 0.55, uv.y) * shimmer;

    // Emissive rim: peaks where coverage crosses the threshold band
    // (body*(1-body) is a soft bump centered on the edge).
    let rim = body * (1.0 - body) * 4.0;

    let rgb = glass * fill_params.x * body + rim_color.rgb * (rim * fill_params.y);
    // The fill occludes (alpha ~= body); the rim contribution rides the same
    // alpha-blended draw, made visible by its HDR magnitude.
    return vec4<f32>(rgb, clamp(body * 0.9, 0.0, 1.0));
}
