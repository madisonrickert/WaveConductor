//! Radiance materials: the additive aura billboards ([`RadianceMaterial`])
//! and (Task 8) the silhouette fill quad + the per-frame driver.
//!
//! ## Additive blend
//!
//! `AlphaMode2d::Blend` routes the draw into `Transparent2d`, and
//! [`RadianceMaterial::specialize`] overrides the color target to pure
//! additive `(One, One)` — flame's recipe, per-material-pipeline so it never
//! leaks into the other sketches' blends. Gradient stops are linear HDR (may
//! exceed 1.0) so cores clear the tonemapper's white knee and bloom.

use bevy::asset::Asset;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, RenderPipelineDescriptor,
    SpecializedMeshPipelineError,
};
use bevy::render::storage::ShaderBuffer;
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d, Material2dKey};

/// Billboard half-size in world px (Camera2d: 1 unit = 1 px), passed to the
/// shader via `params_a.y`.
pub const QUAD_HALF_PX: f32 = 6.0;

/// The additive soft-disc billboard material for the aura particles.
///
/// Shares the particle `ShaderBuffer` handle with
/// [`crate::radiance::compute::sim_params::RadianceSimParams`] (compute
/// writes read-write; this vertex shader reads read-only).
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct RadianceMaterial {
    /// Particle storage buffer, read-only from the vertex shader.
    #[storage(0, read_only)]
    pub particles: Handle<ShaderBuffer>,
    /// x = master intensity (HDR, audio-lifted), y = quad half px,
    /// z = palette shift `0..1`, w = sparkle `0..1`.
    #[uniform(1)]
    pub params_a: Vec4,
    /// Gradient stop A (linear HDR).
    #[uniform(2)]
    pub color_a: Vec4,
    /// Gradient stop B.
    #[uniform(3)]
    pub color_b: Vec4,
    /// Gradient stop C.
    #[uniform(4)]
    pub color_c: Vec4,
    /// x = elapsed seconds (sparkle phase), y/z/w reserved (zero).
    #[uniform(5)]
    pub params_b: Vec4,
}

impl Material2d for RadianceMaterial {
    fn vertex_shader() -> ShaderRef {
        "shaders/radiance/render.wgsl".into()
    }

    fn fragment_shader() -> ShaderRef {
        "shaders/radiance/render.wgsl".into()
    }

    /// `Blend` routes into `Transparent2d`; [`Self::specialize`] then makes
    /// it pure additive.
    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }

    /// Override the color-target blend to pure additive `(One, One)` —
    /// flame's mechanism for HDR accumulation inside the 2D pipeline.
    fn specialize(
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: Material2dKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if let Some(fragment) = descriptor.fragment.as_mut() {
            if let Some(Some(target)) = fragment.targets.get_mut(0) {
                target.blend = Some(BlendState {
                    color: BlendComponent {
                        src_factor: BlendFactor::One,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                    alpha: BlendComponent {
                        src_factor: BlendFactor::One,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                });
            }
        }
        Ok(())
    }
}
