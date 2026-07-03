// Flame additive point cloud. Projection happens HERE, not in a Camera3d:
// view_from_model/clip_from_view come from the CPU orbit camera as uniforms,
// so the global 2D camera pipeline (HDR + bloom + tonemapping) is untouched.
// Ports v4 flamePoints.vert.frag / flamePoints.frag:
//   - fake DoF: size and opacity fall off with |dist - focal| / focal
//   - additive accumulation with per-point opacity and a 1/255 alpha floor
//   - fog toward the scene background, then pow(x, gamma) shaping
//
// The mesh is a flat TriangleList of `total * 6` origin vertices (data unused):
// each node draws one billboarded quad, corner picked from `vertex_index % 6u`,
// mirroring the house billboard idiom in particles/render.wgsl.

struct FlameNode {
    pos: vec3<f32>,
    _pad0: f32,
    color: vec3<f32>,
    _pad1: f32,
}

@group(2) @binding(0) var<storage, read> nodes: array<FlameNode>;
@group(2) @binding(1) var disc_texture: texture_2d<f32>;
@group(2) @binding(2) var disc_sampler: sampler;
@group(2) @binding(3) var<uniform> view_from_model: mat4x4<f32>;
@group(2) @binding(4) var<uniform> clip_from_view: mat4x4<f32>;
@group(2) @binding(5) var<uniform> render_a: vec4<f32>; // focal, size, dof, opacity
@group(2) @binding(6) var<uniform> render_b: vec4<f32>; // live, gamma, brightness, clamp
@group(2) @binding(7) var<uniform> fog_color: vec4<f32>;
@group(2) @binding(8) var<uniform> fog_range: vec4<f32>; // near, far, viewport wh

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec3<f32>,
    @location(2) opacity_scalar: f32,
    @location(3) fog_factor: f32,
}

// One corner of the triangle-list quad: pos in [-0.5, 0.5]^2 plus the UV that
// samples the disc sprite. Copied from the particles/render.wgsl billboard
// table; UVs put v=1 at the bottom, v=0 at the top (Bevy image convention).
struct Corner {
    pos: vec2<f32>,
    uv: vec2<f32>,
}

fn quad_corner(corner: u32) -> Corner {
    var c: Corner;
    switch corner {
        case 0u: { c.pos = vec2<f32>(-0.5, -0.5); c.uv = vec2<f32>(0.0, 1.0); }
        case 1u: { c.pos = vec2<f32>( 0.5, -0.5); c.uv = vec2<f32>(1.0, 1.0); }
        case 2u: { c.pos = vec2<f32>( 0.5,  0.5); c.uv = vec2<f32>(1.0, 0.0); }
        case 3u: { c.pos = vec2<f32>(-0.5, -0.5); c.uv = vec2<f32>(0.0, 1.0); }
        case 4u: { c.pos = vec2<f32>( 0.5,  0.5); c.uv = vec2<f32>(1.0, 0.0); }
        default: { c.pos = vec2<f32>(-0.5,  0.5); c.uv = vec2<f32>(0.0, 0.0); }
    }
    return c;
}

@vertex
fn vertex(
    @builtin(vertex_index) vertex_index: u32,
    @location(0) _local_pos: vec3<f32>,
) -> VertexOutput {
    let node_index = vertex_index / 6u;
    let corner = quad_corner(vertex_index % 6u);
    let node = nodes[node_index];

    // Ember/live prefix: nodes beyond the live count collapse to a point.
    let live = f32(node_index < u32(render_b.x));

    let view_pos = view_from_model * vec4<f32>(node.pos, 1.0);
    let dist = max(-view_pos.z, 1e-4);

    // v4 fake DoF: out-of-focus points grow and fade.
    let focal = max(render_a.x, 1e-4);
    let oof = pow(abs(dist - focal) / focal, 2.0) * render_a.z;
    // v4: gl_PointSize = size * (1 + oof) * ((viewport_h / 2) / dist), clamped.
    let size_px = min(render_b.w, render_a.y * (1.0 + oof) * (fog_range.w * 0.5) / dist);

    var clip = clip_from_view * view_pos;
    // Screen-space billboard: pixel offset scaled into NDC, pre-divide.
    let viewport = vec2<f32>(fog_range.z, fog_range.w);
    clip = vec4<f32>(
        clip.xy + corner.pos * live * size_px * 2.0 / viewport * clip.w,
        clip.zw,
    );

    var out: VertexOutput;
    out.clip_position = clip;
    out.uv = corner.uv;
    out.color = node.color;
    out.opacity_scalar = live / pow(1.0 + oof, 2.0);
    out.fog_factor = clamp((dist - fog_range.x) / (fog_range.y - fog_range.x), 0.0, 1.0);
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let sprite = textureSample(disc_texture, disc_sampler, in.uv);
    // v4 fragment order: sprite modulate -> fog mix -> alpha (opacity * DoF,
    // floored at 1/255) -> pow(rgba, gamma). Under (One, One) blending the
    // fragment multiplies its own shaped alpha in.
    var rgb = in.color * sprite.rgb;
    rgb = mix(rgb, fog_color.rgb, in.fog_factor);
    let alpha = max(render_a.w * sprite.a * in.opacity_scalar, 1.0 / 255.0);
    let gamma = render_b.y;
    let shaped_rgb = pow(max(rgb, vec3<f32>(0.0)), vec3<f32>(gamma));
    let shaped_a = pow(alpha, gamma);
    return vec4<f32>(shaped_rgb * shaped_a * render_b.z, 1.0);
}
