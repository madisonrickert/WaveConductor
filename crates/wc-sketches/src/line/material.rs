//! `Material2d` implementation that binds the particle storage buffer for the
//! render shader.
//!
//! The same `ShaderStorageBuffer` handle owned by the sketch root entity is
//! fed to both `LineMaterial` (for rendering, read-only) and the compute
//! pipeline node (for simulation, read-write). Bevy reference-counts the
//! buffer; the data lives in one place on the GPU.
//!
//! ## Blending
//!
//! Particles use additive blending (`src * src_alpha + dst`) rather than
//! standard alpha blending (`src * src_alpha + dst * (1 - src_alpha)`).
//! Additive blending accumulates brightness where particles overlap, which
//! produces the luminous "star cluster" look consistent with v4. With standard
//! alpha blending the particles look dim because each quad partially occludes
//! what is already in the framebuffer rather than adding to it.
//!
//! `AlphaMode2d` has no `Add` variant, so the blend state is injected via the
//! `specialize` hook on `Material2d`. The pipeline must still be submitted to
//! the `Transparent2d` pass, which `AlphaMode2d::Blend` ensures.

use bevy::asset::Asset;
use bevy::image::Image;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, RenderPipelineDescriptor,
    SpecializedMeshPipelineError,
};
use bevy::render::storage::ShaderStorageBuffer;
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d, Material2dKey};

/// Additive blend state: `output = src.rgb * src.a + dst.rgb`.
///
/// Each particle quad contributes its brightness weighted by its own alpha,
/// then is *added* to the framebuffer content rather than blending over it.
/// Overlapping quads brighten each other, giving star-cluster glow at high
/// particle densities. Alpha channel uses `One + One` so the compositor treats
/// the layer as fully accumulated rather than partially transparent.
const ADDITIVE_BLEND: BlendState = BlendState {
    color: BlendComponent {
        src_factor: BlendFactor::SrcAlpha,
        dst_factor: BlendFactor::One,
        operation: BlendOperation::Add,
    },
    alpha: BlendComponent {
        src_factor: BlendFactor::One,
        dst_factor: BlendFactor::One,
        operation: BlendOperation::Add,
    },
};

/// Bind-group layout: `@group(2) @binding(0)` is the particle storage buffer
/// (read-only at the render stage; write happens in the compute stage);
/// `@binding(1)` is the star sprite texture and `@binding(2)` its sampler,
/// both sampled in the fragment shader.
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct LineMaterial {
    /// Particle storage buffer, read-only from the vertex shader.
    #[storage(0, read_only)]
    pub particles: Handle<ShaderStorageBuffer>,
    /// Star sprite texture sampled in the fragment shader. The texture's
    /// alpha modulates each particle's final alpha so quads render as soft
    /// star points instead of flat-color rectangles.
    #[texture(1)]
    #[sampler(2)]
    pub star_texture: Handle<Image>,
}

impl Material2d for LineMaterial {
    fn vertex_shader() -> ShaderRef {
        "shaders/line/render.wgsl".into()
    }

    fn fragment_shader() -> ShaderRef {
        "shaders/line/render.wgsl".into()
    }

    /// Tells Bevy to submit this material to the `Transparent2d` render phase.
    /// The actual blend equation is overridden to additive in `specialize`.
    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }

    /// Override the blend state on the first (and only) color target to
    /// additive. `AlphaMode2d::Blend` above places the pipeline in the
    /// transparent pass; this hook swaps the blend equation from the default
    /// `SrcAlpha + (1 - SrcAlpha) * Dst` to `SrcAlpha + Dst` so overlapping
    /// particles accumulate brightness instead of occluding each other.
    fn specialize(
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: Material2dKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if let Some(fragment) = descriptor.fragment.as_mut() {
            if let Some(Some(target)) = fragment.targets.first_mut() {
                target.blend = Some(ADDITIVE_BLEND);
            }
        }
        Ok(())
    }
}
