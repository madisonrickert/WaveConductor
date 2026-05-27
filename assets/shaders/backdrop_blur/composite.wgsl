// Paint-callback composite pass.
//
// Samples the half-resolution blurred texture at the egui rect's UV
// coordinates and applies a corner-radius SDF mask so the painted quad
// matches the panel's rounded corners exactly. Output alpha is the SDF
// coverage; egui will composite this rect under the panel's translucent
// tint that's drawn immediately after this callback.

struct Uniforms {
    /// UV rect of this panel inside the source viewport (xy=min, zw=max).
    uv_rect: vec4<f32>,
    /// Half-extent of the panel rect in *clip-space units of this draw call*.
    half_extent: vec2<f32>,
    /// Corner radius in the same units as `half_extent`.
    corner_radius: f32,
    _pad: f32,
}

@group(0) @binding(0) var blur_texture: texture_2d<f32>;
@group(0) @binding(1) var blur_sampler: sampler;
@group(0) @binding(2) var<uniform> uniforms: Uniforms;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) local: vec2<f32>,  // Position inside the panel rect, centered at origin.
    @location(1) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VertexOutput {
    var out: VertexOutput;
    // Quad triangulated as two tris; vid maps to corners 0..6.
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>( 1.0,  1.0),
        vec2<f32>(-1.0,  1.0),
    );
    let c = corners[vid];
    out.local = c * uniforms.half_extent;
    // UV in source viewport: lerp uv_rect.xy → uv_rect.zw by (c * 0.5 + 0.5).
    // Clip-space Y increases upward but UV-Y (screen-space) increases downward,
    // so flip Y to avoid a vertical mirror of the sampled backdrop.
    let t = c * 0.5 + 0.5;
    let t_uv = vec2<f32>(t.x, 1.0 - t.y);
    out.uv = mix(uniforms.uv_rect.xy, uniforms.uv_rect.zw, t_uv);
    out.clip_position = vec4<f32>(c, 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Rounded-rect SDF.
    let r = uniforms.corner_radius;
    let q = abs(in.local) - uniforms.half_extent + vec2<f32>(r);
    let sdf = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
    let coverage = clamp(0.5 - sdf, 0.0, 1.0);

    let sample = textureSample(blur_texture, blur_sampler, in.uv);
    return vec4<f32>(sample.rgb, coverage);
}
