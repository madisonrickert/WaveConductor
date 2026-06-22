// Explode post-process — WGSL port of v4 src/sketches/dots/shaders/explode/fragment.glsl.
//
// Produces the signature radial chromatic-aberration zoom: 5 iterations of
// per-channel radial samples compounding by `shrink_factor=0.98`, spiraling
// around the cursor with `m2 = mat2x2(1.6,-1.2,1.2,1.6)`. Result is
// `pow(col + original, vec4(gamma))`.
//
// Uniforms come from DotsPostParams; the input scene texture is bound at
// @group(0) @binding(1) (sampler at @binding(2)).

struct PostParams {
    i_resolution: vec2<f32>,
    i_mouse: vec2<f32>,
    shrink_factor: f32,
    gamma: f32,
};

@group(0) @binding(0) var<uniform> params: PostParams;
@group(0) @binding(1) var scene_tex: texture_2d<f32>;
@group(0) @binding(2) var scene_sampler: sampler;

// Radial sample: contract `uv` toward `center` by `shrink`.
// Returns the scene colour at the contracted position, or vec4(0) if the
// contracted UV falls outside [0,1]^2 (mirrors v4's bounds check).
// textureSampleLevel is used instead of textureSample because this is called
// inside a loop — non-uniform control flow requires an explicit LOD.
fn exploded_texture(uv: vec2<f32>, center: vec2<f32>, shrink: f32) -> vec4<f32> {
    let offset = uv - center;
    let sample_pos = center + normalize(offset) * length(offset) * shrink;
    if sample_pos.x < 0.0 || sample_pos.x >= 1.0 ||
       sample_pos.y < 0.0 || sample_pos.y >= 1.0 {
        return vec4<f32>(0.0);
    }
    return textureSampleLevel(scene_tex, scene_sampler, sample_pos, 0.0);
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// Fullscreen triangle: three vertices cover the screen with UV mapping.
// Copied from assets/shaders/line/gravity.wgsl.
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
    let uv = in.uv;

    // Spiral rotation matrix from v4: GLSL mat2(1.6,-1.2,1.2,1.6) is column-major,
    // so col0=(1.6,-1.2), col1=(1.2,1.6). WGSL mat2x2(col0, col1) is also
    // column-major, so the faithful encoding matches v4 column-for-column.
    let m2 = mat2x2<f32>(vec2<f32>(1.6, -1.2), vec2<f32>(1.2, 1.6));

    var center = params.i_mouse;
    let original = textureSampleLevel(scene_tex, scene_sampler, uv, 0.0);
    var col = vec4<f32>(0.0);
    var shrink = 1.0;

    for (var i: f32 = 0.0; i < 5.0; i = i + 1.0) {
        let weight = 1.0 / (i + 1.0);
        col.r = col.r + exploded_texture(uv, center, shrink).r * weight;
        shrink = shrink * params.shrink_factor;
        col.g = col.g + exploded_texture(uv, center, shrink).g * weight;
        shrink = shrink * params.shrink_factor;
        col.b = col.b + exploded_texture(uv, center, shrink).b * weight;
        shrink = shrink * params.shrink_factor;
        center = center - m2 * (center - vec2<f32>(0.5)) * 0.5928;
    }

    // Guard pow against undefined behaviour on negative bases. HDR scene values
    // are non-negative, but accumulated floating-point adds could underflow
    // slightly; clamp defensively before pow.
    let base = max(col + original, vec4<f32>(0.0));
    return pow(base, vec4<f32>(params.gamma));
}
