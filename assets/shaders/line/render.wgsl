// Line particle render — one textured quad per particle, driven by vertex_index.
//
// Bindings (Bevy Material2d convention, group 2):
//   @binding(0): particle storage buffer (read-only)
//   @binding(1): star sprite texture (Texture2D<f32>)
//   @binding(2): star sprite sampler

#import bevy_sprite::mesh2d_view_bindings::view

struct Particle {
    position: vec2<f32>,
    velocity: vec2<f32>,
    original_xy: vec2<f32>,
    alpha: f32,
    _pad: f32,
};

@group(2) @binding(0) var<storage, read> particles: array<Particle>;
@group(2) @binding(1) var star_texture: texture_2d<f32>;
@group(2) @binding(2) var star_sampler: sampler;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) brightness: f32,
    @location(2) alpha: f32,
};

// Quad half-size in world units. Plan 10 may tune this; v4's screen-space
// 13px sprite is approximated here in world space.
const QUAD_HALF: f32 = 8.0;

// One corner of the triangle-list quad: world-space offset plus the UV
// coordinate that samples the star sprite. UVs are laid out so v=1 lies at
// the bottom and v=0 at the top, matching Bevy's image UV convention.
struct Corner {
    pos: vec2<f32>,
    uv:  vec2<f32>,
};

fn quad_corner(corner: u32) -> Corner {
    var c: Corner;
    switch corner {
        case 0u: { c.pos = vec2<f32>(-QUAD_HALF, -QUAD_HALF); c.uv = vec2<f32>(0.0, 1.0); }
        case 1u: { c.pos = vec2<f32>( QUAD_HALF, -QUAD_HALF); c.uv = vec2<f32>(1.0, 1.0); }
        case 2u: { c.pos = vec2<f32>( QUAD_HALF,  QUAD_HALF); c.uv = vec2<f32>(1.0, 0.0); }
        case 3u: { c.pos = vec2<f32>(-QUAD_HALF, -QUAD_HALF); c.uv = vec2<f32>(0.0, 1.0); }
        case 4u: { c.pos = vec2<f32>( QUAD_HALF,  QUAD_HALF); c.uv = vec2<f32>(1.0, 0.0); }
        default: { c.pos = vec2<f32>(-QUAD_HALF,  QUAD_HALF); c.uv = vec2<f32>(0.0, 0.0); }
    }
    return c;
}

@vertex
fn vertex(
    @builtin(vertex_index) vertex_index: u32,
    @location(0) local_pos: vec3<f32>,
) -> VertexOutput {
    let particle_index = vertex_index / 6u;
    let corner_index   = vertex_index % 6u;

    let p = particles[particle_index];
    let c = quad_corner(corner_index);
    let world_pos = vec4<f32>(p.position + c.pos, 0.0, 1.0);

    var out: VertexOutput;
    out.clip_position = view.clip_from_world * world_pos;
    out.uv = c.uv;
    // velocity-driven warm-color brightness — same ramp as the pre-texture
    // path, matching v4 starMaterial's color tint logic.
    out.brightness = clamp(length(p.velocity) * 0.005, 0.05, 1.0);
    out.alpha = p.alpha;
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(star_texture, star_sampler, in.uv);
    let b = in.brightness;
    // Warm-tinted velocity color (red > green > blue) modulated by the star
    // sprite's RGB. Final alpha = sprite-alpha × particle-alpha so quad
    // corners fade out smoothly.
    let color = vec3<f32>(b, b * 0.85, b * 0.6);
    return vec4<f32>(color * texel.rgb, texel.a * in.alpha);
}
