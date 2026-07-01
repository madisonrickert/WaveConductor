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
// instead of the star texel. Set from `ParticleMaterial.solid_color`
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
// Psychedelic palette params (ParticleMaterial::palette_params). x = mode index
// (0 Off / 1 Velocity / 2 Spectrum); y = crossfade strength 0..1; z = spread
// (controls heatmap width for Velocity, peak sharpness for Spectrum); w unused.
// x = 0 (Vec4(0), the Active/no-palette value) skips the palette branch below,
// so color is the pre-palette path bit-exactly.
@group(2) @binding(6) var<uniform> palette_params: vec4<f32>;
// Render params (ParticleMaterial::render_params). x = master_brightness, the
// per-sketch User exposure knob (Line/Dots), applied as a final linear multiply
// on the particle rgb BEFORE the post-process gamma — the same brightness-then-
// gamma order Cymatics uses. `1.0` is a bit-exact no-op (rgb * 1.0 == rgb).
// y/z/w reserved (zero).
@group(2) @binding(7) var<uniform> render_params: vec4<f32>;

// HDR emissive headroom. The star sprite's centre texel tops out at ~0.89
// (RGBA(228,221,222,237)/255), so un-multiplied particle cores never reach 1.0
// and the camera's tonemap (which reserves the top of the displayable range for
// scene values above 1.0) rolls them off into dim, washed highlights. Scaling
// the rgb by this constant lifts the bright cores above 1.0 into real HDR
// highlights — they bloom and survive the tonemap shoulder — while the soft
// falloff edges stay sub-1.0, so only the cores glow (the "neon" look). This is
// fixed art (the always-on baseline that makes particles HDR-native);
// master_brightness above is the live exposure trim layered on top. Tune via
// capture review.
const PARTICLE_EMISSIVE: f32 = 1.5;

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
    // Normalized creation index (particle buffer index / (count-1)), 0..1, for
    // the Spectrum palette. Flat: a per-particle constant.
    @location(4) @interpolate(flat) index_norm: f32,
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
    // arrayLength gives the live particle count; guard the (count-1) divide.
    let count = f32(arrayLength(&particles));
    out.index_norm = f32(particle_index) / max(count - 1.0, 1.0);
    return out;
}

// Turbo colormap (Anton Mikhailov / Google), degree-6 polynomial approximation —
// texture-free, blue (t=0) -> green (t=0.5) -> red (t=1). Output clamped to 0..1.
fn turbo(t: f32) -> vec3<f32> {
    let x = clamp(t, 0.0, 1.0);
    let c0 = vec3<f32>(0.1140890109226559, 0.06288340699912215, 0.2248337216805064);
    let c1 = vec3<f32>(6.716419496985708, 3.182286745507602, 7.571581586103393);
    let c2 = vec3<f32>(-66.09402360453038, -4.9279827041226, -10.09439367561635);
    let c3 = vec3<f32>(228.7660791526501, 25.04986699771073, -91.54105330182436);
    let c4 = vec3<f32>(-334.8351565777451, -69.31749712757485, 288.5858850615712);
    let c5 = vec3<f32>(218.7637218434795, 67.52150567819112, -305.2045772184957);
    let c6 = vec3<f32>(-52.88903478218835, -21.54527364654712, 110.5174647748972);
    let rgb = c0 + x * (c1 + x * (c2 + x * (c3 + x * (c4 + x * (c5 + x * c6)))));
    return clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
}

// Value-normalize: divide by the max channel so the palette supplies HUE only,
// never brightness. Turbo's dark cool end (~(0.19,0.07,0.23)) becomes a bright
// blue, so the star keeps supplying brightness and no particle crushes to dark.
fn value_normalize(c: vec3<f32>) -> vec3<f32> {
    let m = max(c.r, max(c.g, c.b));
    return c / max(m, 1e-4);
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
    // img_base is the pre-palette path: image color-influence tint over the star.
    let img_base = mix(texel.rgb, texel.rgb * img_rgb, template_color.x);
    // Psychedelic palette (uniform-mode branch). palette_params is a uniform --
    // constant across the whole draw -- so every fragment takes the same branch
    // (no warp divergence) and the Off case never runs the turbo math.
    var base = img_base;
    if (palette_params.x > 0.5) {
        let strength = palette_params.y;
        let spread = palette_params.z;
        var t: f32;
        if (palette_params.x < 1.5) {
            // Velocity: clamped cool->hot; ~180/spread px/s maps to full hot.
            t = clamp(in.speed * spread / 180.0, 0.0, 1.0);
        } else {
            // Spectrum: center-peak tent over creation index, sharpened by spread.
            let tent = 1.0 - abs(2.0 * in.index_norm - 1.0);
            t = pow(tent, max(spread, 1e-4));
        }
        // Palette = hue only (value-normalized); star supplies brightness.
        let pal_base = texel.rgb * value_normalize(turbo(t));
        base = mix(img_base, pal_base, strength);
    }
    // Attract-only velocity tint applies on top of the (palette-or-image) base.
    let wake = smoothstep(WAKE_SPEED_LO, WAKE_SPEED_HI, in.speed) * attract_color.x;
    let tinted = mix(base, base * WAKE_TINT, wake);
    // Attract-mode brightness lift: the calm screensaver field never drives
    // pixels past the AgX tonemapper's white knee, so its whites read as dim
    // grey. Scaling the particle rgb up during attract pushes the bright cores
    // (and the gravity smear that samples them) back into AgX's white region,
    // so whites stay white. `attract_color.y == 0` (Active) is a bit-exact
    // no-op; the lift ramps in/out with the screensaver fade.
    //
    // PARTICLE_EMISSIVE (fixed HDR headroom) and render_params.x
    // (master_brightness, live exposure; 1.0 = no-op) are the final linear
    // multiplies, applied here at the source so the gravity-smear / explode
    // post-process and the camera tonemap all see the lifted cores.
    let rgb = tinted * (1.0 + attract_color.y) * (PARTICLE_EMISSIVE * render_params.x);
    // Final alpha = sprite-alpha × particle-alpha so quad corners fade smoothly.
    return vec4<f32>(rgb, texel.a * in.alpha);
}
