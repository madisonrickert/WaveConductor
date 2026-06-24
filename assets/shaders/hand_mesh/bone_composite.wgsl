// Additive bone-glow composite for the shared hand-mesh overlay.
//
// Samples the off-screen bone image (emissive wireframe bones on a black
// background, rendered by `HandMeshCamera3d` with no bloom and no tonemapping)
// and the current scene (the gravity-smeared particles), and outputs their
// linear-HDR sum.
//
// This node is inserted into the Core2d graph AFTER the sketch's post-process
// and BEFORE `Node2d::Bloom`, so the main camera's `Bloom` + `AgX` tonemap then
// glow the emissive bones and roll the combined frame to display range in one
// pass — exactly as if the bones were emissive geometry in the scene.
//
// Additive in linear HDR is physically "adding light": the bone image's black
// background contributes nothing (true passthrough of the scene), while the
// emissive bone texels add their value over the scene. No alpha channel is
// consulted, so this is immune to the bloom/tonemap transparent-alpha bug
// (bevyengine/bevy#8286) that broke the previous same-window overlay path.

@group(0) @binding(0) var scene_texture: texture_2d<f32>;
@group(0) @binding(1) var tex_sampler: sampler;
@group(0) @binding(2) var bone_texture: texture_2d<f32>;

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
    let scene = textureSample(scene_texture, tex_sampler, in.uv);
    let bones = textureSample(bone_texture, tex_sampler, in.uv);
    // Linear-HDR additive: emissive bones add as light over the scene; the bone
    // image's black background adds nothing. Alpha is carried from the scene.
    return vec4<f32>(scene.rgb + bones.rgb, scene.a);
}
