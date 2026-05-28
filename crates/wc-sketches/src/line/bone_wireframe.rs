//! Metal-safe wireframe-bone material + geometry for the Line hand-mesh overlay.
//!
//! Bevy's `bevy::pbr::wireframe::WireframePlugin` needs
//! `WgpuFeatures::POLYGON_MODE_LINE`, which Metal does not support, so it
//! no-ops on macOS and the bones render as solid spheres. This module replaces
//! it with a path that works everywhere: an icosphere rendered as a
//! `PrimitiveTopology::LineList` mesh ([`icosphere_line_mesh`]) shaded by a
//! custom [`BoneWireframeMaterial`]. Line primitives need no special wgpu
//! feature.
//!
//! Because the bones are real 3D meshes drawn by the off-screen-compositing
//! `HandMeshCamera3d`, this material — and any post-process pass added to that
//! camera's graph — is the project's hook for applying shaders/effects to the
//! bones. (Bevy's built-in gizmos can draw Metal-safe lines too, but their
//! pipeline shader is fixed and accepts no custom material, so they cannot meet
//! that requirement.) See `assets/shaders/line/bone_wireframe.wgsl`.

use std::collections::HashSet;

use bevy::asset::{Asset, RenderAssetUsages};
use bevy::mesh::{Indices, Mesh, PrimitiveTopology, VertexAttributeValues};
use bevy::pbr::Material;
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;

/// Path to the bone wireframe WGSL (vertex + fragment), relative to the asset
/// root.
const BONE_WIREFRAME_SHADER: &str = "shaders/line/bone_wireframe.wgsl";

/// Icosphere subdivision level for the bone wireframe. `1` matches the geometry
/// density v4's solid bone spheres used. `ico(n)` only fails for `n >= 80`, so
/// `1` is statically safe.
const BONE_ICO_SUBDIVISIONS: u32 = 1;

/// Unlit flat-color material for the wireframe bones.
///
/// `color` binds at `@group(#{MATERIAL_BIND_GROUP}) @binding(0)` as a
/// `vec4<f32>` (see the WGSL). It is the per-bone shader entry point: today the
/// fragment just returns `color`, but the material can grow bindings + shader
/// logic for richer effects without touching the overlay-compositing setup.
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct BoneWireframeMaterial {
    /// Flat **emissive** line color in linear space, written straight to the
    /// overlay camera's HDR target. Values `> 1.0` are intended: the caller
    /// scales the base hue by an intensity (see
    /// `crate::line::hand_mesh::BONE_GLOW_INTENSITY`) so the HDR overlay's bloom
    /// turns the bones into a neon glow. Keep `alpha = 1.0` so bone cores are
    /// opaque for the premultiplied-alpha composite.
    #[uniform(0)]
    pub color: LinearRgba,
}

impl Material for BoneWireframeMaterial {
    fn vertex_shader() -> ShaderRef {
        BONE_WIREFRAME_SHADER.into()
    }

    fn fragment_shader() -> ShaderRef {
        BONE_WIREFRAME_SHADER.into()
    }
}

/// Build a wireframe icosphere of the given radius as a `LineList` mesh.
///
/// Generates Bevy's triangle-list icosphere, then emits one line per unique
/// triangle edge (each interior edge is shared by two faces, so dedup keeps it
/// drawn once). Line primitives render on Metal without
/// `WgpuFeatures::POLYGON_MODE_LINE`, giving the v4 wireframe look where the
/// `Wireframe` component cannot.
#[must_use]
pub fn icosphere_line_mesh(radius: f32) -> Mesh {
    // `ico()` only errors for subdivisions >= 80; `BONE_ICO_SUBDIVISIONS` is 1,
    // so the `Err` arm is unreachable. Fall back to an empty LineList mesh
    // rather than panicking if that invariant is ever broken.
    let Ok(triangles) = Sphere::new(radius).mesh().ico(BONE_ICO_SUBDIVISIONS) else {
        return Mesh::new(PrimitiveTopology::LineList, RenderAssetUsages::RENDER_WORLD);
    };

    let positions = match triangles.attribute(Mesh::ATTRIBUTE_POSITION) {
        Some(VertexAttributeValues::Float32x3(p)) => p.clone(),
        _ => Vec::new(),
    };

    let mut edges: HashSet<(u32, u32)> = HashSet::new();
    if let Some(indices) = triangles.indices() {
        let tri_indices: Vec<u32> = indices
            .iter()
            .map(|i| u32::try_from(i).unwrap_or(0))
            .collect();
        for tri in tri_indices.chunks_exact(3) {
            for &(a, b) in &[(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
                edges.insert((a.min(b), a.max(b)));
            }
        }
    }

    let mut line_indices = Vec::with_capacity(edges.len() * 2);
    for (a, b) in edges {
        line_indices.push(a);
        line_indices.push(b);
    }

    let mut mesh = Mesh::new(PrimitiveTopology::LineList, RenderAssetUsages::RENDER_WORLD);
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_indices(Indices::U32(line_indices));
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icosphere_line_mesh_is_line_list_with_paired_indices() {
        let mesh = icosphere_line_mesh(10.0);

        assert_eq!(mesh.primitive_topology(), PrimitiveTopology::LineList);

        let index_count = match mesh.indices() {
            Some(Indices::U32(v)) => v.len(),
            _ => 0,
        };
        assert!(
            index_count > 0 && index_count % 2 == 0,
            "LineList indices come in pairs; got {index_count}"
        );

        let positions = mesh.attribute(Mesh::ATTRIBUTE_POSITION);
        assert!(
            matches!(
                positions,
                Some(VertexAttributeValues::Float32x3(p)) if p.len() >= 12
            ),
            "icosphere should have at least 12 Float32x3 vertex positions"
        );
    }
}
