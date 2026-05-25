// Line particle render — one quad per particle, driven by vertex_index.
//
// Particle storage buffer at @group(2) @binding(0) (Bevy Material2d convention).

#import bevy_sprite::mesh2d_view_bindings::view

struct Particle {
    position: vec2<f32>,
    velocity: vec2<f32>,
    original_xy: vec2<f32>,
    alpha: f32,
    _pad: f32,
};

@group(2) @binding(0) var<storage, read> particles: array<Particle>;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) brightness: f32,
    @location(1) alpha: f32,
};

// Half-size of each quad in world units.
const QUAD_HALF: f32 = 1.5;

fn quad_corner(corner: u32) -> vec2<f32> {
    switch corner {
        case 0u: { return vec2<f32>(-QUAD_HALF, -QUAD_HALF); }
        case 1u: { return vec2<f32>( QUAD_HALF, -QUAD_HALF); }
        case 2u: { return vec2<f32>( QUAD_HALF,  QUAD_HALF); }
        case 3u: { return vec2<f32>(-QUAD_HALF, -QUAD_HALF); }
        case 4u: { return vec2<f32>( QUAD_HALF,  QUAD_HALF); }
        default: { return vec2<f32>(-QUAD_HALF,  QUAD_HALF); }
    }
}

@vertex
fn vertex(
    @builtin(vertex_index) vertex_index: u32,
    @location(0) local_pos: vec3<f32>,
) -> VertexOutput {
    let particle_index = vertex_index / 6u;
    let corner_index   = vertex_index % 6u;

    let p = particles[particle_index];
    let corner = quad_corner(corner_index);
    let world_pos = vec4<f32>(p.position + corner, 0.0, 1.0);

    var out: VertexOutput;
    out.clip_position = view.clip_from_world * world_pos;
    out.brightness = clamp(length(p.velocity) * 0.005, 0.05, 1.0);
    out.alpha = p.alpha;
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let b = in.brightness;
    return vec4<f32>(b, b * 0.85, b * 0.6, in.alpha);
}
