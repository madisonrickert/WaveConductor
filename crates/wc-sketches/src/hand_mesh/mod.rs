//! Shared wireframe-bone hand overlay for sketches.
//!
//! Extracts the off-screen HDR bone-camera + additive composite that Line and
//! Dots each forked. Each consumer registers [`HandMeshPlugin`] with a
//! [`HandMeshConfig`]; the global [`bone_composite::HandMeshCompositePlugin`]
//! (registered once by `SketchesPlugin`) owns the composite pipeline and node.
//!
//! See [`bone_wireframe`] for the Metal-safe LineList bone mesh + material.

pub mod bone_wireframe;

pub use bone_wireframe::{icosphere_line_mesh, BoneWireframeMaterial};
