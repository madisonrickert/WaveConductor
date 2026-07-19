// Radiance beat pulses — waves of additive HDR light radiating outward from
// the dancer's silhouette edge (fullscreen quad, fragment only; the default
// Material2d vertex shader supplies UVs). Each live slot lights the band
// where distance-from-body ≈ its wave radius, sampled from the CPU chamfer
// distance field — so the front is an iso-distance contour of the
// silhouette that detaches from the outline and travels outward keeping the
// body's shape (nested silhouettes of light, not circles). At age 0 the
// band sits at distance 0: the body itself flashes on the beat.
// Additive (One, One) blending is set by RadiancePulseMaterial::specialize;
// the global bloom turns the HDR crest into the glow.
//
// Bindings (group 2):
//   @binding(0)/(1): distance-field texture + sampler (R8Unorm 256²;
//     0..1 = 0..mapping.w texels from the silhouette, body interior = 0).
//   @binding(2): PulseUniform —
//     pulses[i]: x = age s, y = strength (0 = dead)
//     colors[i]: rgb = linear-HDR wave color
//     params:    x = master intensity, y = expansion speed px/s,
//                z = base band half-width px, w = lifetime s
//     mapping:   x = mirror (1 = flip), y = fit-to-height aspect
//                (window_w/window_h; 1 = full-window stretch),
//                z = world px per mask texel, w = texel denormalization
//
// Struct parity: mirrors RadiancePulseUniform in radiance/pulse.rs
// (MAX_PULSES = 6). The UV remap matches silhouette.wgsl term for term so
// the wave agrees per-pixel with the rendered fill.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

struct PulseUniform {
    pulses: array<vec4<f32>, 6>,
    colors: array<vec4<f32>, 6>,
    params: vec4<f32>,
    mapping: vec4<f32>,
};

@group(2) @binding(0) var dist_tex: texture_2d<f32>;
@group(2) @binding(1) var dist_samp: sampler;
@group(2) @binding(2) var<uniform> pulses: PulseUniform;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // Quad UV -> mask UV: the same mirror flip + fit-to-height aspect remap
    // as silhouette.wgsl, so distance lookups align with the drawn fill.
    var uv = in.uv;
    if (pulses.mapping.x > 0.5) {
        uv.x = 1.0 - uv.x;
    }
    uv.x = 0.5 + (uv.x - 0.5) * pulses.mapping.y;

    // Distance from the silhouette, in mask texels. Outside the mask square
    // (the pillarbox on a wide screen) the clamped sample freezes at the
    // boundary texel; extend it analytically in quadrature with the
    // horizontal overshoot so the wavefront keeps travelling off the square
    // instead of halting at an invisible vertical line.
    let over_texels = max(0.0, abs(uv.x - 0.5) - 0.5) * 256.0;
    let sampled = textureSample(dist_tex, dist_samp, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0))).r;
    let d_tex = sampled * pulses.mapping.w;
    let dist_px = sqrt(d_tex * d_tex + over_texels * over_texels) * pulses.mapping.z;

    let speed = pulses.params.y;
    let base_width = pulses.params.z;
    let lifetime = pulses.params.w;
    var rgb = vec3<f32>(0.0);
    for (var i = 0u; i < 6u; i = i + 1u) {
        let p = pulses.pulses[i];
        let strength = p.y;
        if (strength <= 0.0) {
            continue;
        }
        let age = p.x;
        // The front travels outward; its band widens as it ages so the
        // light disperses like a wave rather than staying razor-thin.
        let radius = speed * age;
        let width = base_width * (1.0 + 0.9 * age);
        let x = (dist_px - radius) / width;
        let band = exp(-4.0 * x * x);
        // Brightness: exponential decay with age, windowed to zero over the
        // final 0.3 s so the slot's hard lifetime cutoff is invisible.
        let tail = clamp((lifetime - age) / 0.3, 0.0, 1.0);
        let env = strength * exp(-age * 1.8) * tail;
        rgb += pulses.colors[i].rgb * (band * env);
    }
    // Additive (One, One): alpha is ignored by the blend.
    return vec4<f32>(rgb * pulses.params.x, 1.0);
}
