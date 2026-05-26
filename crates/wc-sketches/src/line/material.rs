//! `Material2d` implementation that binds the particle storage buffer for the
//! render shader.
//!
//! The same `ShaderStorageBuffer` handle owned by the sketch root entity is
//! fed to both `LineMaterial` (for rendering, read-only) and the compute
//! pipeline node (for simulation, read-write). Bevy reference-counts the
//! buffer; the data lives in one place on the GPU.

use bevy::asset::Asset;
use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::render::storage::ShaderStorageBuffer;
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d};

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

    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }
}
