// Dual-Kawase downsample pass (Bjørge, ARM 2015).
//
// Samples the input texture at center plus 4 corner offsets (each one
// texel away in the destination space), then averages with center weight 4
// and corner weight 1. The destination texture is half the size of the
// input — the fragment-shader invocation rate halves each axis.
//
// Bind layout (group 0):
//   binding 0: input_texture (texture_2d<f32>)
//   binding 1: input_sampler (sampler)
//   binding 2: uniforms (struct { texel_size: vec2<f32>, _pad: vec2<f32> })

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

// Fullscreen triangle.
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
    var sum = textureSample(input_texture, input_sampler, in.uv) * 4.0;
    sum += textureSample(input_texture, input_sampler, in.uv - vec2<f32>( o.x,  o.y));
    sum += textureSample(input_texture, input_sampler, in.uv + vec2<f32>( o.x, -o.y));
    sum += textureSample(input_texture, input_sampler, in.uv + vec2<f32>(-o.x,  o.y));
    sum += textureSample(input_texture, input_sampler, in.uv + vec2<f32>( o.x,  o.y));
    return sum / 8.0;
}
