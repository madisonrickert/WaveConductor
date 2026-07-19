// Radiance aura render — one additive billboard per particle, driven by
// vertex_index (6 vertices per particle; the mesh's own vertex data is
// unused). Additive (One, One) blending is set by
// RadianceMaterial::specialize (flame's recipe): overlapping quads accumulate
// into luminous HDR cores and the global bloom + tonemap supply the radiance.
// No post-process pass.
//
// Flame look, three cooperating mechanisms:
//  1. Temperature over lifetime — born near-white-hot (the body's color
//     lifted toward white, HDR), cooling through the body's saturated hue
//     mid-life, dying into a deep translucent ember. Computed per particle in
//     the vertex shader and passed flat.
//  2. Velocity-stretched quads — fast particles (onset ejecta, limb sheds)
//     elongate along their velocity into streaks and gain brightness; slow
//     drifting embers stay round. Area is roughly conserved (across shrinks
//     as along grows) so streaks read as motion, not as bigger blobs.
//  3. Per-body color identity — each particle carries the slot it spawned
//     from; slot_colors[] gives every tracked dancer a distinct hue.
//
// Bindings (Bevy Material2d convention, group 2):
//   @binding(0): particle storage buffer (read-only)
//   @binding(1): AuraUniform —
//     params:      x master intensity (HDR), y quad half-size px,
//                  z highs sparkle 0..1, w elapsed seconds
//     slot_colors: per-body-slot linear-HDR color identity
//
// Struct parity: Particle mirrors RadianceParticle (offset_of! tested);
// AuraUniform mirrors RadianceAuraUniform in radiance/render.rs.

#import bevy_sprite::mesh2d_view_bindings::view

struct Particle {
    position: vec2<f32>,
    velocity: vec2<f32>,
    age: f32,
    lifespan: f32,
    seed: f32,
    slot: f32,
};

struct AuraUniform {
    params: vec4<f32>,
    slot_colors: array<vec4<f32>, 4>,
};

@group(2) @binding(0) var<storage, read> particles: array<Particle>;
@group(2) @binding(1) var<uniform> u: AuraUniform;

const TAU: f32 = 6.2831853;
// Speed (world px/s) at which the velocity stretch reaches 1x extra length.
const STRETCH_REF_SPEED: f32 = 340.0;
// Stretch cap: a particle can elongate at most this many extra lengths.
const STRETCH_MAX: f32 = 3.0;
// Extra brightness per unit stretch factor (fast ejecta burn brighter).
const SPEED_BRIGHTNESS: f32 = 0.5;
// Life fraction where the white-hot birth phase hands over to the body hue.
const HOT_END: f32 = 0.16;
// Life fraction where cooling toward the ember tail begins.
const COOL_START: f32 = 0.34;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) alpha: f32,
    // Per-particle constants; flat interpolation preserves them exactly
    // (provoking vertex).
    @location(2) @interpolate(flat) rgb: vec3<f32>,
    @location(3) @interpolate(flat) flicker: f32,
};

// One corner of the two-triangle quad: unit offset (xy in ±1) + uv (zw).
fn quad_corner(corner: u32) -> vec4<f32> {
    switch corner {
        case 0u: { return vec4<f32>(-1.0, -1.0, 0.0, 1.0); }
        case 1u: { return vec4<f32>( 1.0, -1.0, 1.0, 1.0); }
        case 2u: { return vec4<f32>( 1.0,  1.0, 1.0, 0.0); }
        case 3u: { return vec4<f32>(-1.0, -1.0, 0.0, 1.0); }
        case 4u: { return vec4<f32>( 1.0,  1.0, 1.0, 0.0); }
        default: { return vec4<f32>(-1.0,  1.0, 0.0, 0.0); }
    }
}

// Lifetime alpha envelope: ramp in over the first 10% of life, hold, fade
// out over the last half. Dead (age >= lifespan, incl. the zeroed spawn
// state) yields exactly 0.
fn life_alpha(age: f32, lifespan: f32) -> f32 {
    if (lifespan <= 0.0 || age >= lifespan) {
        return 0.0;
    }
    let lf = age / lifespan;
    return smoothstep(0.0, 0.10, lf) * (1.0 - smoothstep(0.50, 1.0, lf));
}

// Temperature-over-lifetime color for one particle: white-hot birth ->
// body hue -> deep ember death. `body` is the slot's linear-HDR identity.
fn flame_color(body: vec3<f32>, lf: f32) -> vec3<f32> {
    // Hot birth: the body color lifted toward white — but only partway, so
    // the additive stacking of many young particles does the final push to
    // white at the silhouette edge and lone particles keep their hue.
    let hot = body * 0.9 + vec3<f32>(0.55, 0.50, 0.42);
    // Deep ember: the hue cooled into a dim translucent red-brown glow.
    let ember = body * vec3<f32>(0.30, 0.16, 0.10) + vec3<f32>(0.02, 0.004, 0.0);
    if (lf < HOT_END) {
        return mix(hot, body, smoothstep(0.0, HOT_END, lf));
    }
    return mix(body, ember, smoothstep(COOL_START, 0.95, lf));
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
    let c = quad_corner(corner_index);
    let half = u.params.y;

    // Velocity stretch: elongate along the motion direction, thin across it
    // (roughly area-conserving), so fast ejecta draw as streaks.
    let speed = length(p.velocity);
    let sfac = min(speed / STRETCH_REF_SPEED, STRETCH_MAX);
    var dir = vec2<f32>(0.0, 1.0);
    if (speed > 1e-3) {
        dir = p.velocity / speed;
    }
    let perp = vec2<f32>(-dir.y, dir.x);
    let along = half * (1.0 + sfac * 1.1);
    let across = half / (1.0 + sfac * 0.45);
    let offset = dir * (c.x * along) + perp * (c.y * across);
    let world_pos = vec4<f32>(p.position + offset * live, 0.0, 1.0);

    var out: VertexOutput;
    out.clip_position = view.clip_from_world * world_pos;
    out.uv = c.zw;
    out.alpha = alpha;
    // Per-slot color identity through the temperature ramp; fast particles
    // burn brighter (the streaks must clear the bloom knee).
    let slot = min(u32(p.slot + 0.5), 3u);
    let lf = p.age / max(p.lifespan, 1e-4);
    out.rgb = flame_color(u.slot_colors[slot].rgb, lf) * (1.0 + sfac * SPEED_BRIGHTNESS);
    // Sparkle: a per-particle deterministic flicker, amplitude = highs drive
    // (u.params.z), phase from the respawn seed.
    out.flicker = 1.0 + u.params.z * 0.6 * sin(u.params.w * 21.0 + p.seed * TAU);
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // Procedural soft disc: quadratic falloff squared — a tight bright core
    // with soft skirts, no texture fetch. The quad stretch in the vertex
    // stage elongates this footprint into the streak shape.
    let d = length(in.uv - vec2<f32>(0.5)) * 2.0;
    let disc = pow(max(1.0 - d * d, 0.0), 2.0);
    // Additive (One, One): the fragment multiplies its own envelope in; the
    // alpha lane is ignored by the blend.
    let rgb = in.rgb * (u.params.x * in.flicker * disc * in.alpha);
    return vec4<f32>(rgb, 1.0);
}
