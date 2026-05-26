// Gravity-smear post-process — WGSL port of v4 src/sketches/line/shaders/gravity/fragment.glsl.
//
// Reads the scene texture (Core2d main pass output) and produces a
// chromatic-smeared output by ray-marching 11 steps of gravity-distorted
// UV samples, accumulating per-step color shifts to produce the signature
// concentric-trail look.
//
// Uniforms come from LinePostParams; the input scene texture is bound at
// @group(0) @binding(1) (sampler at @binding(2)).

struct PostParams {
    iResolution: vec2<f32>,
    iMouse: vec2<f32>,
    iMouseFactor: f32,
    iGlobalTime: f32,
    g_constant: f32,
    gamma: f32,
};

@group(0) @binding(0) var<uniform> params: PostParams;
@group(0) @binding(1) var scene_texture: texture_2d<f32>;
@group(0) @binding(2) var scene_sampler: sampler;

const GRAVITY_EPSILON: f32 = 1e-4;
const NUM_STEPS: u32 = 11u;

// Precomputed `0.8 / (i + 6 + sqrt(i+1))` for i in 0..11, matching v4's
// INTENSITY_SCALARS table (line/shaders/gravity/fragment.glsl).
const INTENSITY_SCALARS = array<f32, 11>(
    0.114285714, 0.095077216, 0.082202612, 0.072727273,
    0.065380480, 0.059481810, 0.054623350, 0.050541977,
    0.047058824, 0.044047339, 0.041415103,
);

fn gravity(p: vec2<f32>, attraction_center: vec2<f32>, g: f32) -> vec2<f32> {
    let delta = attraction_center - p;
    let dist_sq = max(dot(delta, delta), GRAVITY_EPSILON);
    return delta * (g / dist_sq);
}

fn smear(uv_pixels: vec2<f32>, attraction_center: vec2<f32>) -> vec4<f32> {
    var incoming_p = uv_pixels;
    var outgoing_p = uv_pixels;
    var color = vec4<f32>(0.0);

    // Chromatic shift factors. v4: outgoing = (0.96, 1.0, 1.0/0.96, 1.0);
    //                            incoming = (1.0/0.96, 1.0, 0.96, 1.0).
    let outgoing_factor = vec4<f32>(0.96, 1.0, 1.0 / 0.96, 1.0);
    let incoming_factor = vec4<f32>(1.0 / 0.96, 1.0, 0.96, 1.0);

    let v_mouse_pull = (params.iMouse - uv_pixels) * params.iMouseFactor;

    var v_incoming_accum = incoming_factor;
    var v_outgoing_accum = outgoing_factor;

    for (var i: u32 = 0u; i < NUM_STEPS; i = i + 1u) {
        incoming_p = incoming_p - gravity(incoming_p, attraction_center, params.g_constant);
        outgoing_p = outgoing_p + gravity(outgoing_p, attraction_center, params.g_constant);

        incoming_p = incoming_p - v_mouse_pull;
        outgoing_p = outgoing_p + v_mouse_pull;

        let intensity = INTENSITY_SCALARS[i];

        let in_uv  = incoming_p / params.iResolution;
        let out_uv = outgoing_p / params.iResolution;

        color = color + textureSample(scene_texture, scene_sampler, in_uv)
                     * intensity * v_incoming_accum;
        color = color + textureSample(scene_texture, scene_sampler, out_uv)
                     * intensity * v_outgoing_accum;

        v_incoming_accum = v_incoming_accum * incoming_factor;
        v_outgoing_accum = v_outgoing_accum * outgoing_factor;
    }
    return color;
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// Fullscreen triangle: three vertices that cover the screen with UV mapping.
@vertex
fn vertex(@builtin(vertex_index) idx: u32) -> VertexOutput {
    var pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    var uv = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );
    var out: VertexOutput;
    out.clip_position = vec4<f32>(pos[idx], 0.0, 1.0);
    out.uv = uv[idx];
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv_pixels = in.uv * params.iResolution;
    let base = textureSample(scene_texture, scene_sampler, in.uv);
    let smeared = smear(uv_pixels, params.iResolution / 2.0);
    let combined = base + smeared;
    // Per-channel gamma curve.
    return vec4<f32>(
        pow(combined.r, params.gamma),
        pow(combined.g, params.gamma),
        pow(combined.b, params.gamma),
        combined.a,
    );
}
