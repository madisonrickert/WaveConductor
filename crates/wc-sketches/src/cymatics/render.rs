//! Fullscreen Cymatics render: a window-sized quad with [`CymaticsMaterial`]
//! sampling the compute ping-pong texture A directly (the odd-N continuity
//! refresh keeps A current at frame end, so there is no separate display
//! texture to blit into).
//!
//! Ports v4 `renderCymatics.frag` lighting via
//! `assets/shaders/cymatics/render.wgsl`: height-gradient surface normal,
//! two directional lights with power-8 specular, `BASE_COL` / `BASE_BODY_COL`
//! mix by absolute height, vignette, radial background, and `skewIntensity`
//! body push. The 2D pass draws this quad; the hand-mesh composite and
//! bloom/AgX layer go on top.
//!
//! ## Texture sampling
//!
//! Texture A is `rgba32float`. Linear sampling of 32-bit-float textures requires
//! the `float32-filterable` WebGPU feature, which this project does not depend
//! on. `render.wgsl` reads all texels via `textureLoad` (integer coordinates),
//! so only `TEXTURE_BINDING` usage is required on the sampled texture — which
//! [`create_cymatics_textures`] sets on A.
//!
//! ## Uniform layout
//!
//! Follows the flat-field idiom of [`crate::particles::material::ParticleMaterial`]:
//! individual [`Vec4`] uniforms rather than a nested struct, matching Bevy's
//! `AsBindGroup` / encase `ShaderType` requirements without a separate derive.
//! [`CymaticsRenderParams`] is the public API type for the spawn helper; it is
//! not used as a `#[uniform]` field directly.
//!
//! [`create_cymatics_textures`]: crate::cymatics::compute::create_cymatics_textures

use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d};
use bytemuck::{Pod, Zeroable};

use super::CymaticsRoot;

/// Public API type for configuring the render material parameters.
///
/// Matches the logical fields of the WGSL uniforms. Packed into two
/// [`Vec4`] bindings on [`CymaticsMaterial`] (matching the flat-field
/// idiom of [`crate::particles::material::ParticleMaterial`]).
///
/// `#[repr(C)]` + `bytemuck`: used for size tests and potential CPU-side
/// serialisation; not passed to the GPU as a raw buffer.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct CymaticsRenderParams {
    /// Window size in pixels — aspect correction and vignette scale.
    pub screen_resolution: Vec2,
    /// Sim grid size in texels — UV-to-texel conversion and gradient scale.
    pub sim_resolution: Vec2,
    /// v4 `skewIntensity` (after `skew_curve` exponent): pushes the body
    /// colour toward white. Set each frame by `update_cymatics_material`.
    pub skew_intensity: f32,
    /// Post-render brightness multiplier. Packed into `CymaticsMaterial::skew.y`
    /// and applied in `render.wgsl` as `col * master_brightness`. `1.0` = no-op.
    pub master_brightness: f32,
    /// Pad to 32 bytes (16-byte-aligned struct). Private: not externally meaningful.
    _pad: Vec2,
}

/// Fullscreen material that samples the compute ping-pong texture A.
///
/// Bind group layout (all at `@group(2)`):
/// - `@binding(0)` `resolution: vec4<f32>` — `.xy` = screen (px), `.zw` = sim (texels)
/// - `@binding(1)` `skew: vec4<f32>` — `.x` = `skewIntensity`, `.y` = `master_brightness`,
///   `.z` = user gamma, `.w` = screensaver saturation (1.0 = identity)
/// - `@binding(2)` `cell_tex: texture_2d<f32>` — texture A, `textureLoad` only
///
/// The shader (`assets/shaders/cymatics/render.wgsl`) uses `textureLoad` for
/// all texel accesses; no sampler binding is declared or required.
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct CymaticsMaterial {
    /// Packed resolution: `.xy` = screen (px), `.zw` = sim grid (texels).
    #[uniform(0)]
    pub resolution: Vec4,
    /// Packed visual uniform: `.x` = v4 `skewIntensity`, `.y` = `master_brightness`,
    /// `.z` = user gamma (`1.0` = identity), `.w` = screensaver saturation
    /// (`1.0` = identity / active path).
    #[uniform(1)]
    pub skew: Vec4,
    /// Ping-pong texture A (`rgba32float`); accessed via `textureLoad` only —
    /// no sampler binding generated. A carries `TEXTURE_BINDING` usage, which
    /// `textureLoad` requires, and holds the latest field at frame end.
    #[texture(2)]
    pub cell_texture: Handle<Image>,
}

impl Material2d for CymaticsMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/cymatics/render.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Opaque
    }
}

/// Spawn the window-sized fullscreen quad tagged [`CymaticsRoot`].
///
/// The mesh is a [`Rectangle`] sized to `window_size`; call
/// `resize_cymatics_quad` on [`bevy::window::WindowResized`] to keep it synchronised.
/// The material is initialised with `skew.x = 0` (resting, updated each
/// frame by `update_cymatics_material`), `skew.y = master_brightness`
/// (from settings; default 1.0 so the first frame is not black),
/// `skew.z = gamma` (from settings; default 1.0 = identity so the first
/// frame is not gamma-flashed before `update_cymatics_material` runs), and
/// `skew.w = 1.0` (saturation identity, so the first frame is not
/// desaturated before the first update; the screensaver ramps it in later).
pub fn spawn_cymatics_quad(
    commands: &mut Commands<'_, '_>,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<CymaticsMaterial>,
    cell_texture: Handle<Image>,
    window_size: Vec2,
    sim_resolution: Vec2,
    master_brightness: f32,
    gamma: f32,
) -> Entity {
    let w = window_size.x.max(1.0);
    let h = window_size.y.max(1.0);
    let mesh = meshes.add(Rectangle::new(w, h));
    let material = materials.add(CymaticsMaterial {
        resolution: Vec4::new(w, h, sim_resolution.x, sim_resolution.y),
        // skew.x = skewIntensity (updated each frame by update_cymatics_material)
        // skew.y = master_brightness (updated each frame; initialised here to
        //          avoid a black first frame)
        // skew.z = gamma (updated each frame; initialised to the settings value
        //          so a persisted non-identity gamma applies from frame 1 and
        //          the 1.0 default does not flash before the first update)
        // skew.w = screensaver saturation (1.0 = identity; the screensaver ramps
        //          it in later — initialised to 1.0 so frame 1 is not desaturated)
        skew: Vec4::new(0.0, master_brightness, gamma, 1.0),
        cell_texture,
    });
    commands
        .spawn((
            Mesh2d(mesh),
            MeshMaterial2d(material),
            Transform::default(),
            CymaticsRoot,
        ))
        .id()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_params_is_32_bytes() {
        // Vec2(8) + Vec2(8) + f32(4) + f32(4) + Vec2(8) = 32 bytes (#[repr(C)], no gap).
        // master_brightness replaced one pad f32 from the old Vec3 pad; total unchanged.
        assert_eq!(std::mem::size_of::<CymaticsRenderParams>(), 32);
    }
}
