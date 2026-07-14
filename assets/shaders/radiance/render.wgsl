// Radiance aura render — one additive soft-disc billboard per particle,
// driven by vertex_index (6 vertices per particle; the mesh's own vertex
// data is unused). Additive (One, One) blending is set by
// RadianceMaterial::specialize (flame's recipe): overlapping discs
// accumulate into luminous HDR cores and the global bloom + tonemap supply
// the radiance. No post-process pass.
//
// Bindings (Bevy Material2d convention, group 2):
//   @binding(0): particle storage buffer (read-only)
//   @binding(1): params_a — x master intensity (HDR), y quad half-size px,
//                z palette shift 0..1, w sparkle 0..1
//   @binding(2..4): gradient stops a, b, c (linear HDR)
//   @binding(5): params_b — x elapsed seconds, y/z/w reserved
//
// Struct parity: Particle mirrors RadianceParticle (offset_of! tested).

#import bevy_sprite::mesh2d_view_bindings::view

struct Particle {
    position: vec2<f32>,
    velocity: vec2<f32>,
    age: f32,
    lifespan: f32,
    seed: f32,
    _pad: f32,
};

@group(2) @binding(0) var<storage, read> particles: array<Particle>;
@group(2) @binding(1) var<uniform> params_a: vec4<f32>;
@group(2) @binding(2) var<uniform> color_a: vec4<f32>;
@group(2) @binding(3) var<uniform> color_b: vec4<f32>;
@group(2) @binding(4) var<uniform> color_c: vec4<f32>;
@group(2) @binding(5) var<uniform> params_b: vec4<f32>;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) alpha: f32,
    // Gradient coordinate and flicker are per-particle constants; flat
    // interpolation preserves them exactly (provoking vertex).
    @location(2) @interpolate(flat) t: f32,
    @location(3) @interpolate(flat) flicker: f32,
};

// One corner of the two-triangle quad: offset (xy) + uv (zw).
fn quad_corner(corner: u32, half: f32) -> vec4<f32> {
    switch corner {
        case 0u: { return vec4<f32>(-half, -half, 0.0, 1.0); }
        case 1u: { return vec4<f32>( half, -half, 1.0, 1.0); }
        case 2u: { return vec4<f32>( half,  half, 1.0, 0.0); }
        case 3u: { return vec4<f32>(-half, -half, 0.0, 1.0); }
        case 4u: { return vec4<f32>( half,  half, 1.0, 0.0); }
        default: { return vec4<f32>(-half,  half, 0.0, 0.0); }
    }
}

// Lifetime alpha envelope: ramp in over the first 12% of life, hold, fade
// out over the last 45%. Dead (age >= lifespan, incl. the zeroed spawn
// state) yields exactly 0.
fn life_alpha(age: f32, lifespan: f32) -> f32 {
    if (lifespan <= 0.0 || age >= lifespan) {
        return 0.0;
    }
    let lf = age / lifespan;
    return smoothstep(0.0, 0.12, lf) * (1.0 - smoothstep(0.55, 1.0, lf));
}

@vertex
fn vertex(
    @builtin(vertex_index) vertex_index: u32,
    @location(0) local_pos: vec3<f32>,
) -> VertexOutput {
    let particle_index = vertex_index / 6u;
    let corner_index = vertex_index % 6u;

    let p = particles[particle_index];
    let alpha = life_alpha(p.age, p.lifespan);
    // Collapse dead particles to a zero-area quad: the rasterizer culls
    // them and they cost no fill (particles/render.wgsl idiom).
    let live = f32(alpha > 0.0);
    let c = quad_corner(corner_index, params_a.y);
    let world_pos = vec4<f32>(p.position + c.xy * live, 0.0, 1.0);

    var out: VertexOutput;
    out.clip_position = view.clip_from_world * world_pos;
    out.uv = c.zw;
    out.alpha = alpha;
    // Audio shifts the gradient coordinate; fract wraps it along the ramp.
    out.t = fract(p.seed + params_a.z);
    // Sparkle: a per-particle deterministic flicker, amplitude = highs drive.
    out.flicker = 1.0 + params_a.w * 0.6 * sin(params_b.x * 21.0 + p.seed * 6.2831853);
    return out;
}

// Three-stop gradient a -> b -> c.
fn gradient(t: f32) -> vec3<f32> {
    if (t < 0.5) {
        return mix(color_a.rgb, color_b.rgb, t * 2.0);
    }
    return mix(color_b.rgb, color_c.rgb, (t - 0.5) * 2.0);
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // Procedural soft disc: quadratic falloff squared — a tight bright core
    // with soft skirts, no texture fetch.
    let d = length(in.uv - vec2<f32>(0.5)) * 2.0;
    let disc = pow(max(1.0 - d * d, 0.0), 2.0);
    // Additive (One, One): the fragment multiplies its own envelope in; the
    // alpha lane is ignored by the blend.
    let rgb = gradient(in.t) * (params_a.x * in.flicker * disc * in.alpha);
    return vec4<f32>(rgb, 1.0);
}
