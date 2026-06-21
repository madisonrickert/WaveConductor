//! `Material2d` implementation that binds the particle storage buffer for the
//! render shader.
//!
//! The same `ShaderBuffer` handle owned by the sketch root entity is
//! fed to both `LineMaterial` (for rendering, read-only) and the compute
//! pipeline node (for simulation, read-write). Bevy reference-counts the
//! buffer; the data lives in one place on the GPU.
//!
//! ## Blending
//!
//! Particles use standard alpha blending (`src * src_alpha + dst * (1 - src_alpha)`),
//! which is Bevy's default for `AlphaMode2d::Blend`. The gravity-smear
//! post-process (`assets/shaders/line/gravity.wgsl`) is v4's actual glow
//! mechanism: it ray-marches 11 steps of gravity-distorted UV samples and
//! accumulates the result on top of the scene, producing the luminous
//! chromatic-smear look. Additive blending at the particle level double-stacks
//! brightness — the post-process samples an already-additive framebuffer 22
//! times and adds it back — making particles far too bright.

use bevy::asset::Asset;
use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::render::storage::ShaderBuffer;
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d};

/// Bind-group layout: `@group(2) @binding(0)` is the particle storage buffer
/// (read-only at the render stage; write happens in the compute stage);
/// `@binding(1)` is the star sprite texture and `@binding(2)` its sampler,
/// both sampled in the fragment shader; `@binding(3)` is the debug solid
/// override, `@binding(4)` the attract-mode velocity-color params,
/// `@binding(5)` the per-image colour-influence params, and `@binding(6)` is
/// the psychedelic palette params.
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct LineMaterial {
    /// Particle storage buffer, read-only from the vertex shader.
    #[storage(0, read_only)]
    pub particles: Handle<ShaderBuffer>,
    /// Star sprite texture sampled in the fragment shader. The texture's
    /// alpha modulates each particle's final alpha so quads render as soft
    /// star points instead of flat-color rectangles.
    #[texture(1)]
    #[sampler(2)]
    pub star_texture: Handle<Image>,
    /// Debug solid-particle override (linear RGBA). When `a > 0` the fragment
    /// shader returns this flat colour instead of the star texel — the
    /// "magenta isolation" trick (`WC_DEBUG_SOLID_PARTICLES`). [`Vec4::ZERO`]
    /// (the [`Self::solid_off`] sentinel) means "off"; normal runs and release
    /// builds always seed this with the off sentinel.
    #[uniform(3)]
    pub solid_color: Vec4,
    /// Attract-mode color params: `x` = velocity-tint strength `0..=1`
    /// (`ScreensaverFade × LineSettings::attract_color_strength`); `y` =
    /// brightness lift (`ScreensaverFade × (LineSettings::attract_brightness −
    /// 1)`), applied as `rgb *= 1 + y` so the calm field's whites clear the
    /// `AgX` tonemapper's white knee; `z`/`w` reserved (zero). Driven by
    /// [`crate::line::screensaver::drive_attract_color`]; spawned at (and driven
    /// back to) [`Self::attract_color_off`] outside attract, where `x = y = 0`
    /// makes both the tint (`mix(rgb, _, 0.0)`) and the lift (`rgb * 1.0`)
    /// provable no-ops — Active rendering is unchanged.
    #[uniform(4)]
    pub attract_color: Vec4,
    /// Per-image colour-influence params: `x` = blend strength `0..=1` (the
    /// active template's `color_influence`, driven by `drive_color_influence`
    /// in the templates-gated `systems::color_influence` module); `y`/`z`/`w`
    /// reserved (zero). `Vec4::ZERO` ([`Self::template_color_off`])
    /// makes the per-particle image tint a provable no-op
    /// (`mix(rgb, rgb*img, 0.0)` returns `rgb` bit-exactly).
    #[uniform(5)]
    pub template_color: Vec4,
    /// Psychedelic palette params: `x` = mode index (`PaletteMode::index()`:
    /// `0` Off / `1` Velocity / `2` Spectrum), `y` = crossfade strength `0..=1`,
    /// `z` = palette spread (per-mode tuning), `w` = reserved. Driven by
    /// [`crate::line::systems::palette::drive_palette`]. [`Vec4::ZERO`]
    /// ([`Self::palette_off`]) sets mode `0`, so the render shader's uniform-mode
    /// branch is skipped and color is the pre-palette path bit-exactly.
    #[uniform(6)]
    pub palette_params: Vec4,
}

impl LineMaterial {
    /// The `solid_color` sentinel meaning "off" (use the star texture). Shared
    /// by the spawn site and the tests so they agree on the off value.
    pub fn solid_off() -> Vec4 {
        Vec4::ZERO
    }

    /// The `attract_color` value meaning "no velocity tint" (live / Active
    /// rendering). Shared by the spawn site, the attract driver, and tests.
    pub fn attract_color_off() -> Vec4 {
        Vec4::ZERO
    }

    /// The `template_color` value meaning "no image-colour tint" (color
    /// influence 0% / no active template). Shared by the spawn site, the
    /// colour-influence driver, and tests.
    pub fn template_color_off() -> Vec4 {
        Vec4::ZERO
    }

    /// The `palette_params` value meaning "palette off" (mode index `0`). Shared
    /// by the spawn site, the palette driver, and tests.
    pub fn palette_off() -> Vec4 {
        Vec4::ZERO
    }
}

impl Material2d for LineMaterial {
    fn vertex_shader() -> ShaderRef {
        "shaders/line/render.wgsl".into()
    }

    fn fragment_shader() -> ShaderRef {
        "shaders/line/render.wgsl".into()
    }

    /// Standard alpha blending (`AlphaMode2d::Blend`) — Bevy's default for the
    /// `Transparent2d` pass. The gravity-smear post-process provides the glow;
    /// no specialization needed.
    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_solid_color_is_off() {
        // alpha == 0 means "off" (use the star texture). Constructed via the
        // helper so spawn.rs and tests agree on the off-sentinel.
        assert_eq!(LineMaterial::solid_off(), Vec4::ZERO);
    }

    #[test]
    fn default_attract_color_is_off() {
        // strength (x) == 0 means "no velocity tint" — the Active-mode value.
        assert_eq!(LineMaterial::attract_color_off(), Vec4::ZERO);
    }

    #[test]
    fn default_template_color_is_off() {
        // strength (x) == 0 means "no image-colour tint" — the no-template value.
        assert_eq!(LineMaterial::template_color_off(), Vec4::ZERO);
    }

    #[test]
    fn default_palette_params_is_off() {
        // mode channel (x) == 0 means "palette off" — the shader branch is skipped.
        assert_eq!(LineMaterial::palette_off(), Vec4::ZERO);
    }
}
