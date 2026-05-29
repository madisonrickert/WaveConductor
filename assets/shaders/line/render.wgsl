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
// Debug solid-particle override (linear RGBA). a > 0 => return the flat colour
// instead of the star texel. Set from `LineMaterial.solid_color`
// (WC_DEBUG_SOLID_PARTICLES). Vec4(0) means "off" in normal runs and release.
@group(2) @binding(3) var<uniform> solid_color: vec4<f32>;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) alpha: f32,
};

// Quad half-size in world units. Bevy Camera2d default projection is 1 world
// unit = 1 screen pixel, so 6.5 gives a 13×13 px quad — matching v4's
// `THREE.PointsMaterial { size: 13, sizeAttenuation: false }`.
const QUAD_HALF: f32 = 6.5;

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
    out.alpha = p.alpha;
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // Debug isolation: when a solid override colour is set (alpha > 0), render
    // every particle as that flat colour, modulated only by per-particle alpha.
    // Separates particle geometry from the star texture / smear contribution.
    if (solid_color.a > 0.0) {
        return vec4<f32>(solid_color.rgb, solid_color.a * in.alpha);
    }
    let texel = textureSample(star_texture, star_sampler, in.uv);
    // v4 uses THREE.PointsMaterial with vertexColors:true and a vertex color
    // of (1, 1, 1). The texture RGB is multiplied by the vertex color, which
    // is a no-op — the star sprite's own RGB (near-white at centre) is used
    // directly. Velocity-based dimming was NOT present in v4 and caused
    // stationary particles to render at 5% brightness instead of the correct
    // ~89% (the star.png centre pixel is RGBA(228,221,222,237)).
    // Final alpha = sprite-alpha × particle-alpha so quad corners fade smoothly.
    return vec4<f32>(texel.rgb, texel.a * in.alpha);
}
