//! Radiance materials: the additive aura billboards ([`RadianceMaterial`])
//! and the silhouette fill quad (`RadianceSilhouetteMaterial`) + the
//! per-frame driver (`drive_radiance_materials`) that packs settings/state
//! into both every frame.
//!
//! ## Additive blend
//!
//! `AlphaMode2d::Blend` routes the draw into `Transparent2d`, and
//! [`RadianceMaterial::specialize`] overrides the color target to pure
//! additive `(One, One)` — flame's recipe, per-material-pipeline so it never
//! leaks into the other sketches' blends. Gradient stops are linear HDR (may
//! exceed 1.0) so cores clear the tonemapper's white knee and bloom.
//!
//! ## Silhouette fill
//!
//! `RadianceSilhouetteMaterial` stays ordinary `AlphaMode2d::Blend` (no
//! `specialize` override): the fill occludes via normal alpha, and only its
//! rim rides HDR magnitude into bloom. It is drawn under the particles
//! (spawned at z 0.0 vs the billboards' z 1.0 — Task 9).
//!
//! The material, `particle_material_params`, and `drive_radiance_materials`
//! are gated behind `body-tracking-mediapipe`: they consume
//! `radiance::systems::sim_params::RadianceState`, which lives in a module
//! wc-core gates behind the same feature (camera-independent, CI-testable
//! headless). The `cargo doc` gate builds default features only, so this
//! surface must be absent there (plain code spans here, not intra-doc
//! links, per the house rule) — see `Cargo.toml`'s `body-tracking-mediapipe`
//! forwarding feature, and `radiance::systems::mod`/`radiance::compute::mod`
//! for the identical precedent.

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

#[cfg(feature = "body-tracking-mediapipe")]
use wc_core::lifecycle::screensaver::fade::ScreensaverFade;

#[cfg(feature = "body-tracking-mediapipe")]
use crate::radiance::settings::{RadiancePalette, RadianceSettings};
#[cfg(feature = "body-tracking-mediapipe")]
use crate::radiance::systems::sim_params::RadianceState;
#[cfg(feature = "body-tracking-mediapipe")]
use crate::radiance::systems::spawn::RadianceRoot;

/// The window-filling silhouette material sampling the person mask.
///
/// Drawn under the particles (spawned at z 0.0 vs the billboards' z 1.0) via
/// ordinary alpha blending; only the rim is HDR-emissive.
#[cfg(feature = "body-tracking-mediapipe")]
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct RadianceSilhouetteMaterial {
    /// The shared 256² `R8Unorm` person mask (Plan B writes it in place;
    /// Bevy re-uploads on mutation).
    #[texture(0)]
    #[sampler(1)]
    pub mask: Handle<Image>,
    /// x = fill intensity, y = rim glow, z = mask threshold, w = mirror.
    #[uniform(2)]
    pub fill_params: Vec4,
    /// x = elapsed seconds, y = shimmer amount, z = raw-mask debug, w =
    /// fit-to-height aspect factor (`window_w`/`window_h`; 1 = full-window stretch).
    #[uniform(3)]
    pub effect_params: Vec4,
    /// Deep glassy base color (linear).
    #[uniform(4)]
    pub fill_color: Vec4,
    /// Emissive rim color (linear HDR).
    #[uniform(5)]
    pub rim_color: Vec4,
}

#[cfg(feature = "body-tracking-mediapipe")]
impl Material2d for RadianceSilhouetteMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/radiance/silhouette.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }
}

/// The deep indigo glass base fill (linear). A constant, not a setting: the
/// palette drives the rim; the body stays a dark glassy anchor.
#[cfg(feature = "body-tracking-mediapipe")]
#[must_use]
pub fn silhouette_fill_color() -> Vec4 {
    Vec4::new(0.05, 0.03, 0.10, 1.0)
}

/// Rotate a linear-HDR color's hue by `phase` full turns (`0..1` wraps; `0`
/// and `1` are exact identity). Exact HSV rotation: saturation and the HDR
/// value (max component) are preserved bit-for-bit, so a rotated palette
/// stays as vivid and as bloom-hot as the authored one — the psychedelic
/// hue-cycle must never wash colors out mid-rotation (a YIQ-matrix rotate
/// would).
#[must_use]
pub fn rotate_hue(color: Vec4, phase: f32) -> Vec4 {
    // `.abs()` folds rem_euclid's `-0.0` (exact negative whole turns) into
    // `+0.0` so the bit compare below catches it; the bit compare itself
    // sidesteps `clippy::float_cmp`.
    let phase = phase.rem_euclid(1.0).abs();
    // Exact identity at whole turns (no float round-trip drift).
    if phase.to_bits() == 0.0_f32.to_bits() {
        return color;
    }
    let value = color.x.max(color.y).max(color.z);
    let low = color.x.min(color.y).min(color.z);
    let chroma = value - low;
    if chroma <= 0.0 {
        return color; // achromatic: hue undefined, rotation is identity
    }
    // RGB -> hue in 0..6 sextant units.
    let hue = if (value - color.x).abs() < f32::EPSILON {
        ((color.y - color.z) / chroma).rem_euclid(6.0)
    } else if (value - color.y).abs() < f32::EPSILON {
        (color.z - color.x) / chroma + 2.0
    } else {
        (color.x - color.y) / chroma + 4.0
    };
    let hue = (hue + phase * 6.0).rem_euclid(6.0);
    // Hue -> RGB at the same chroma, then restore the original minimum.
    let mid = chroma * (1.0 - (hue.rem_euclid(2.0) - 1.0).abs());
    #[allow(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "hue is clamped to [0, 6) by rem_euclid; the sextant index is 0..=5"
    )]
    let (red, green, blue) = match hue as u32 {
        0 => (chroma, mid, 0.0),
        1 => (mid, chroma, 0.0),
        2 => (0.0, chroma, mid),
        3 => (0.0, mid, chroma),
        4 => (mid, 0.0, chroma),
        _ => (chroma, 0.0, mid),
    };
    Vec4::new(red + low, green + low, blue + low, color.w)
}

/// Pack the particle-material params + palette stops for one frame:
/// hue-rotated by the psychedelic cycle phase, then ember-blended by the
/// screensaver fade envelope (the attract ember keeps its authored warmth —
/// only the user palette rotates). Pure for testability.
///
/// Returns `(params_a, [color_a, color_b, color_c])`.
#[cfg(feature = "body-tracking-mediapipe")]
#[must_use]
pub fn particle_material_params(
    state: &RadianceState,
    palette: RadiancePalette,
    fade_alpha: f32,
) -> (Vec4, [Vec4; 3]) {
    let a = fade_alpha.clamp(0.0, 1.0);
    // Attract mode dims toward the ember: intensity eases to 70%.
    let intensity = state.intensity * (1.0 - a * 0.3);
    // Audio-swelled billboard size: the aura visibly breathes with the music
    // (intensity term) and jumps on onsets. Clamped at 1.5x — fill cost
    // scales with the square of this factor, so the ceiling bounds the
    // worst-case raster load at ~2.25x for onset instants only.
    let quad_half = QUAD_HALF_PX * (0.85 + 0.25 * state.intensity + 0.3 * state.onset_env).min(1.5);
    let params_a = Vec4::new(intensity, quad_half, state.palette_shift, state.sparkle);
    let user = palette.stops().map(|c| rotate_hue(c, state.hue_phase));
    let ember = RadiancePalette::Ember.stops();
    let colors = [
        user[0].lerp(ember[0], a),
        user[1].lerp(ember[1], a),
        user[2].lerp(ember[2], a),
    ];
    (params_a, colors)
}

/// `Update` (gated `in_state(AppState::Radiance)` — runs through Idle and
/// the screensaver like flame's material driver, so the ember blend and the
/// held last-frame envelopes keep rendering): pack settings + state into
/// both materials every frame. The per-frame cost is a small uniform
/// re-prepare, the same class as flame's eight-uniform driver.
#[cfg(feature = "body-tracking-mediapipe")]
#[allow(
    clippy::too_many_arguments,
    reason = "Bevy system — each param is a distinct ECS resource/query the driver packs into the two materials"
)]
pub fn drive_radiance_materials(
    time: Res<'_, Time>,
    window: Single<'_, '_, &Window>,
    settings: Res<'_, RadianceSettings>,
    state: Res<'_, RadianceState>,
    fade: Res<'_, ScreensaverFade>,
    particle_roots: Query<
        '_,
        '_,
        &bevy::sprite_render::MeshMaterial2d<RadianceMaterial>,
        With<RadianceRoot>,
    >,
    silhouette_roots: Query<
        '_,
        '_,
        &bevy::sprite_render::MeshMaterial2d<RadianceSilhouetteMaterial>,
        With<RadianceRoot>,
    >,
    mut particle_materials: ResMut<'_, Assets<RadianceMaterial>>,
    mut silhouette_materials: ResMut<'_, Assets<RadianceSilhouetteMaterial>>,
) {
    let (params_a, colors) = particle_material_params(&state, settings.palette, fade.alpha());
    let params_b = Vec4::new(time.elapsed_secs(), 0.0, 0.0, 0.0);
    for handle in &particle_roots {
        if let Some(mut material) = particle_materials.get_mut(&handle.0) {
            material.params_a = params_a;
            material.color_a = colors[0];
            material.color_b = colors[1];
            material.color_c = colors[2];
            material.params_b = params_b;
        }
    }
    // Rim takes the palette's hottest stop (ember-blended like the
    // particles); the debug lane routes the raw-mask overlay. The rim's
    // brightness rides the audio: the dancer's outline glows with the level
    // and flashes on every onset — the single most legible reactive lane on
    // a busy floor.
    let rim = colors[2];
    let rim_drive = (0.7 + 0.35 * state.intensity + 0.6 * state.onset_env).min(2.2);
    let fill_params = Vec4::new(
        settings.silhouette_fill,
        settings.rim_glow * rim_drive,
        settings.mask_threshold,
        f32::from(u8::from(settings.mirror)),
    );
    // `fit_to_height` maps the square mask to a centred, height-tall square so
    // the dancer keeps its proportions on non-square displays; the silhouette
    // shader remaps its mask sample by this aspect factor (`window_w/window_h`;
    // 1.0 = the full-window stretch). Matches `uv_to_world` in the sim baker so
    // fill, rim, and particle spawns agree.
    let fit_aspect = if settings.fit_to_height {
        window.width() / window.height().max(1.0)
    } else {
        1.0
    };
    let effect_params = Vec4::new(
        time.elapsed_secs(),
        state.sparkle,
        f32::from(u8::from(settings.mask_debug_overlay)),
        fit_aspect,
    );
    for handle in &silhouette_roots {
        if let Some(mut material) = silhouette_materials.get_mut(&handle.0) {
            material.fill_params = fill_params;
            material.effect_params = effect_params;
            material.fill_color = silhouette_fill_color();
            material.rim_color = rim;
        }
    }
}

#[cfg(all(test, feature = "body-tracking-mediapipe"))]
mod tests {
    use super::*;

    /// Fade 0 (Active) uses the user palette verbatim; fade 1 lands on the
    /// ember stops with intensity eased to 70%.
    #[test]
    fn particle_params_blend_to_ember_on_fade() {
        let state = RadianceState {
            onset_env: 0.0,
            intensity: 1.0,
            sparkle: 0.4,
            palette_shift: 0.25,
            ..RadianceState::default()
        };
        let (pa0, c0) = particle_material_params(&state, RadiancePalette::Prism, 0.0);
        assert!((pa0.x - 1.0).abs() < 1e-6);
        assert!((pa0.z - 0.25).abs() < 1e-6);
        assert_eq!(c0, RadiancePalette::Prism.stops());
        let (pa1, c1) = particle_material_params(&state, RadiancePalette::Prism, 1.0);
        assert!((pa1.x - 0.7).abs() < 1e-6, "ember intensity ease");
        assert_eq!(c1, RadiancePalette::Ember.stops());
    }

    /// The quad half-size lane swells with intensity + onset and clamps at
    /// 1.5x the base constant.
    #[test]
    fn particle_params_swell_quad_half_with_audio() {
        let quiet = RadianceState::default();
        let (pa, _) = particle_material_params(&quiet, RadiancePalette::Ocean, 0.0);
        assert!(
            (pa.y - QUAD_HALF_PX * 0.85).abs() < 1e-5,
            "quiet floor: {}",
            pa.y
        );
        let loud = RadianceState {
            intensity: 1.0,
            onset_env: 1.0,
            ..RadianceState::default()
        };
        let (pa_loud, _) = particle_material_params(&loud, RadiancePalette::Ocean, 0.0);
        assert!(pa_loud.y > pa.y, "audio must swell the billboards");
        let slammed = RadianceState {
            intensity: 2.0,
            onset_env: 2.0,
            ..RadianceState::default()
        };
        let (pa_max, _) = particle_material_params(&slammed, RadiancePalette::Ocean, 0.0);
        assert!(
            (pa_max.y - QUAD_HALF_PX * 1.5).abs() < 1e-5,
            "swell clamps at 1.5x: {}",
            pa_max.y
        );
    }

    /// Whole turns are exact identity; achromatic colors never change.
    #[test]
    fn rotate_hue_identity_cases() {
        let c = Vec4::new(0.35, 0.10, 1.00, 1.0);
        assert_eq!(rotate_hue(c, 0.0), c);
        assert_eq!(rotate_hue(c, 1.0), c);
        assert_eq!(rotate_hue(c, -2.0), c);
        let grey = Vec4::new(0.5, 0.5, 0.5, 1.0);
        assert_eq!(rotate_hue(grey, 0.37), grey);
    }

    /// A third-turn cycles primaries (red → green → blue → red) and
    /// preserves the HDR value + saturation structure.
    #[test]
    fn rotate_hue_cycles_primaries_and_preserves_value() {
        let red = Vec4::new(2.0, 0.0, 0.0, 1.0); // HDR red
        let green = rotate_hue(red, 1.0 / 3.0);
        assert!(
            (green.y - 2.0).abs() < 1e-5 && green.x.abs() < 1e-5 && green.z.abs() < 1e-5,
            "{green}"
        );
        let blue = rotate_hue(green, 1.0 / 3.0);
        assert!(
            (blue.z - 2.0).abs() < 1e-5 && blue.x.abs() < 1e-5 && blue.y.abs() < 1e-5,
            "{blue}"
        );
        // Value (max) and min are invariant under any rotation.
        let stop = Vec4::new(0.35, 0.10, 1.00, 1.0);
        let rotated = rotate_hue(stop, 0.618);
        let value = rotated.x.max(rotated.y).max(rotated.z);
        let low = rotated.x.min(rotated.y).min(rotated.z);
        assert!((value - 1.0).abs() < 1e-5, "value preserved: {rotated}");
        assert!((low - 0.10).abs() < 1e-5, "min preserved: {rotated}");
        assert!((rotated.w - 1.0).abs() < f32::EPSILON, "alpha untouched");
    }

    /// A rotated palette feeds through to the packed stops (fade 0).
    #[test]
    fn particle_params_apply_hue_phase() {
        let state = RadianceState {
            intensity: 1.0,
            hue_phase: 0.5,
            ..RadianceState::default()
        };
        let (_, colors) = particle_material_params(&state, RadiancePalette::Prism, 0.0);
        let expected = RadiancePalette::Prism.stops().map(|c| rotate_hue(c, 0.5));
        assert_eq!(colors, expected);
    }
}
