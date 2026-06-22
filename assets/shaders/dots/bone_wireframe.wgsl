// Unlit flat-color material for the Dots sketch's hand-mesh wireframe bones.
//
// This is the shader hook for the Dots bone overlay: the bones are real 3D
// meshes (LineList topology) drawn by the off-screen-compositing
// `DotsHandMeshCamera3d`, so arbitrary per-bone shading and downstream
// post-process passes are all possible from here — unlike Bevy's built-in
// gizmos, whose pipeline shader is fixed. Today it simply emits the uniform
// `color`; extend `fragment` (and add bindings to `DotsBoneWireframeMaterial`)
// for richer effects.
//
// A position-only vertex stage (no normals/UVs) keeps the pipeline compatible
// with the bare LineList bone mesh; the standard PBR vertex shader assumes
// attributes this mesh doesn't carry.
//
// Carry-forward: this shader is generic and duplicates
// `assets/shaders/line/bone_wireframe.wgsl`. A shared home (e.g.
// `shaders/particles/bone_wireframe.wgsl`) is the eventual move.

#import bevy_pbr::mesh_functions::{get_world_from_local, mesh_position_local_to_clip}

// `#{MATERIAL_BIND_GROUP}` is Bevy's shader-def placeholder for the material
// bind group index (resolved per-pipeline), so this stays correct across
// engine versions instead of hard-coding `@group(2)`.
@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> color: vec4<f32>;

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
};

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;
    // Model matrix for this instance, then local -> clip space.
    let world_from_local = get_world_from_local(vertex.instance_index);
    out.clip_position =
        mesh_position_local_to_clip(world_from_local, vec4<f32>(vertex.position, 1.0));
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    return color;
}
