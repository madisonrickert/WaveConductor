// Dual-Kawase upsample pass (Bjørge, ARM 2015).
//
// Samples the input at 8 surrounding points (4 cardinal + 4 diagonal),
// weighted so the cardinals contribute 2x and diagonals 1x, summed to 12.
// The destination texture is twice the size of the input.

struct Uniforms {
    texel_size: vec2<f32>,
    _pad: vec2<f32>,
}

@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var input_sampler: sampler;
@group(0) @binding(2) var<uniform> uniforms: Uniforms;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VertexOutput {
    var out: VertexOutput;
    let uv = vec2<f32>(f32((vid << 1u) & 2u), f32(vid & 2u));
    out.uv = uv;
    out.clip_position = vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let o = uniforms.texel_size;
    var sum = vec4<f32>(0.0);
    // Diagonals (weight 1).
    sum += textureSample(input_texture, input_sampler, in.uv + vec2<f32>(-o.x,  o.y));
    sum += textureSample(input_texture, input_sampler, in.uv + vec2<f32>( o.x,  o.y));
    sum += textureSample(input_texture, input_sampler, in.uv + vec2<f32>(-o.x, -o.y));
    sum += textureSample(input_texture, input_sampler, in.uv + vec2<f32>( o.x, -o.y));
    // Cardinals (weight 2).
    sum += textureSample(input_texture, input_sampler, in.uv + vec2<f32>(0.0,  o.y * 2.0)) * 2.0;
    sum += textureSample(input_texture, input_sampler, in.uv + vec2<f32>(0.0, -o.y * 2.0)) * 2.0;
    sum += textureSample(input_texture, input_sampler, in.uv + vec2<f32>( o.x * 2.0, 0.0)) * 2.0;
    sum += textureSample(input_texture, input_sampler, in.uv + vec2<f32>(-o.x * 2.0, 0.0)) * 2.0;
    return sum / 12.0;
}
