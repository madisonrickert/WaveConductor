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
    age: f32,
    lifespan: f32,
    spawn_hash: f32,
    // Packed RGB8 spawn colour (`pack_rgb8`, 0x00RRGGBB), recovered below.
    spawn_color: f32,
    _pad: f32,
};

@group(2) @binding(0) var<storage, read> particles: array<Particle>;
@group(2) @binding(1) var star_texture: texture_2d<f32>;
@group(2) @binding(2) var star_sampler: sampler;
// Debug solid-particle override (linear RGBA). a > 0 => return the flat colour
// instead of the star texel. Set from `LineMaterial.solid_color`
// (WC_DEBUG_SOLID_PARTICLES). Vec4(0) means "off" in normal runs and release.
@group(2) @binding(3) var<uniform> solid_color: vec4<f32>;
// Attract-mode velocity-color params. x = tint strength 0..1; y = brightness
// lift (extra multiplier on the final rgb, so `rgb *= 1 + y`). Both are driven
// from the screensaver fade envelope by `drive_attract_color`; z/w reserved
// (zero). x = 0 makes the velocity tint a bit-exact no-op (mix(rgb, _, 0.0) ==
// rgb) and y = 0 makes the brightness lift a bit-exact no-op (rgb * 1.0 == rgb),
// so the Active-mode value Vec4::ZERO leaves live rendering unchanged.
@group(2) @binding(4) var<uniform> attract_color: vec4<f32>;
// Per-image colour-influence params. x = blend strength 0..1 (the active
// template's `color_influence`), driven by `drive_color_influence`; y/z/w
// reserved. Strength 0 makes the per-particle image tint a bit-exact no-op:
// mix(rgb, rgb*img, 0.0) == rgb.
@group(2) @binding(5) var<uniform> template_color: vec4<f32>;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) alpha: f32,
    // Particle speed (|velocity|, world px/s) for the attract velocity tint.
    @location(2) speed: f32,
    // Packed RGB8 spawn colour, carried per-particle to the fragment stage.
    // `@interpolate(flat)` is REQUIRED: this is an opaque bit pattern, not a real
    // number. Default perspective interpolation does FP arithmetic on the bits
    // (and may flush denormals — dark-red colours decode as denormals), which
    // would corrupt the colour. Flat = provoking-vertex value, bit-preserved.
    @location(3) @interpolate(flat) spawn_color: f32,
};

// Velocity band for the attract tint, in world px/s. Below LO the particle is
// "calm" and renders exactly as today (warm star-sprite white); the tint
// reaches full strength at HI. The slow noise turbulence drifts well below LO,
// so the calm field stays untinted; only particles a wandering pulse has
// stirred up (peak accel ~PULSE_PEAK_POWER x gravity_constant) move fast enough
// to pick up the cool tint, so colour traces the perturbances.
const WAKE_SPEED_LO: f32 = 30.0;
const WAKE_SPEED_HI: f32 = 180.0;

// Tint direction at full strength: a desaturated cool pull (red down, a touch
// of extra blue). The artwork is warm white/amber with blue/red chromatic
// smear fringes; multiplying the texel keeps the sprite's luminance shape, so
// the wake reads as the same star points shifted cyan, not a new colour layer.
const WAKE_TINT: vec3<f32> = vec3<f32>(0.60, 0.95, 1.12);

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
    // Alpha-0 particles (attract-mode fraction kills, fresh respawns before
    // their first fade tick) contribute nothing through the alpha blend
    // (src_alpha = 0 leaves dst untouched), so collapse their quad to a point:
    // the rasterizer culls the zero-area triangles and the dead particles
    // cost no fragment work instead of ~13x13 px of no-op blending each.
    let live = f32(p.alpha > 0.0);
    let world_pos = vec4<f32>(p.position + c.pos * live, 0.0, 1.0);

    var out: VertexOutput;
    out.clip_position = view.clip_from_world * world_pos;
    out.uv = c.uv;
    out.alpha = p.alpha;
    out.speed = length(p.velocity);
    out.spawn_color = p.spawn_color;
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
    // directly. Velocity-based *dimming* was NOT present in v4 and caused
    // stationary particles to render at 5% brightness instead of the correct
    // ~89% (the star.png centre pixel is RGBA(228,221,222,237)).
    //
    // Attract-only velocity tint ("tripper trap", kept subtle): fast-moving
    // particles (those a wandering pulse has stirred up) pull toward a
    // desaturated cool tint, so colour literally traces the perturbances while
    // the calm field keeps the warm-white personality. `wake` is zero when
    // attract_color.x is zero
    // (Active mode) OR the particle is calm (speed < WAKE_SPEED_LO), and
    // mix(rgb, _, 0.0) returns rgb bit-exactly — live rendering is unchanged.
    // Per-image colour influence: tint the star toward the particle's source
    // image colour (multiply, preserving the sprite's luminance shape like the
    // wake tint). template_color.x == 0 (no template / influence 0%) makes this
    // a bit-exact no-op: mix(texel.rgb, _, 0.0) == texel.rgb.
    let packed = bitcast<u32>(in.spawn_color);
    let img_rgb = vec3<f32>(
        f32((packed >> 16u) & 0xFFu),
        f32((packed >> 8u) & 0xFFu),
        f32(packed & 0xFFu)) / 255.0;
    let base = mix(texel.rgb, texel.rgb * img_rgb, template_color.x);
    // Attract-only velocity tint applies on top of the image-coloured base.
    let wake = smoothstep(WAKE_SPEED_LO, WAKE_SPEED_HI, in.speed) * attract_color.x;
    let tinted = mix(base, base * WAKE_TINT, wake);
    // Attract-mode brightness lift: the calm screensaver field never drives
    // pixels past the AgX tonemapper's white knee, so its whites read as dim
    // grey. Scaling the particle rgb up during attract pushes the bright cores
    // (and the gravity smear that samples them) back into AgX's white region,
    // so whites stay white. `attract_color.y == 0` (Active) is a bit-exact
    // no-op; the lift ramps in/out with the screensaver fade.
    let rgb = tinted * (1.0 + attract_color.y);
    // Final alpha = sprite-alpha × particle-alpha so quad corners fade smoothly.
    return vec4<f32>(rgb, texel.a * in.alpha);
}
