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
//! leaks into the other sketches' blends. The per-slot identity colors are
//! linear HDR (may exceed 1.0) so flame cores clear the tonemapper's white
//! knee and bloom; the temperature-over-lifetime ramp (white-hot birth →
//! body hue → deep ember) lives in `render.wgsl`'s `flame_color`.
//!
//! ## Silhouette fill
//!
//! `RadianceSilhouetteMaterial` stays ordinary `AlphaMode2d::Blend` (no
//! `specialize` override): the fill occludes via normal alpha, and only its
//! per-slot rims ride HDR magnitude into bloom. It is drawn under the
//! particles (spawned at z 0.0 vs the billboards' z 1.0 — Task 9). All four
//! mask channels render simultaneously; fill alpha and rim brightness ride
//! each body's fade envelope so figures ease in and out.
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
    ShaderType, SpecializedMeshPipelineError,
};
use bevy::render::storage::ShaderBuffer;
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d, Material2dKey};

use crate::radiance::settings::RadiancePalette;

/// Billboard half-size in world px (Camera2d: 1 unit = 1 px), passed to the
/// shader via `params.y`.
pub const QUAD_HALF_PX: f32 = 4.0;

/// Overwrite the first color target's blend with pure additive `(One, One)`
/// so the draw accumulates HDR light into bloom instead of alpha-occluding —
/// the one shared recipe behind Radiance's three additive layers (the aura
/// billboards here, the beat-pulse quad, and the sparkle quad), each of whose
/// `Material2d::specialize` calls this. Per-material-pipeline, so it never
/// leaks into other sketches' blends (Flame keeps its own copy).
pub(crate) fn override_additive_blend(descriptor: &mut RenderPipelineDescriptor) {
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
}

/// Master brightness scale on the additive aura (folded into `params.x`).
/// The flame is tens of thousands of overlapping additive HDR quads — an
/// unscaled `state.intensity` saturates the whole frame to white, so this
/// brings one particle's contribution down to ember scale and lets the
/// *density* (emission, overlap) paint the heat gradient.
pub const AURA_BRIGHTNESS: f32 = 0.24;

/// Per-slot hue offsets are `slot × hue_spread` full turns; this array names
/// the slot multipliers so the derivation reads as intent (slot 0 keeps the
/// palette hue verbatim — the solo dancer always gets the authored palette).
pub const SLOT_HUE_STEPS: [f32; 4] = [0.0, 1.0, 2.0, 3.0];

/// The uniform block the aura billboard shader consumes.
///
/// Struct parity: mirrors `AuraUniform` in `shaders/radiance/render.wgsl`.
#[derive(ShaderType, Clone, Copy, Debug)]
pub struct RadianceAuraUniform {
    /// x = master intensity (HDR, audio-lifted), y = quad half px,
    /// z = highs sparkle `0..1`, w = elapsed seconds (flicker phase).
    pub params: Vec4,
    /// Per-body-slot linear-HDR color identity (see [`slot_identity_colors`]).
    pub slot_colors: [Vec4; 4],
}

impl Default for RadianceAuraUniform {
    /// Neutral: zero intensity, base quad size, palette-less white identity.
    fn default() -> Self {
        Self {
            params: Vec4::new(0.0, QUAD_HALF_PX, 0.0, 0.0),
            slot_colors: [Vec4::ONE; 4],
        }
    }
}

/// The additive velocity-stretched billboard material for the flame aura.
///
/// Shares the particle `ShaderBuffer` handle with
/// [`crate::radiance::compute::sim_params::RadianceSimParams`] (compute
/// writes read-write; this vertex shader reads read-only).
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct RadianceMaterial {
    /// Particle storage buffer, read-only from the vertex shader.
    #[storage(0, read_only)]
    pub particles: Handle<ShaderBuffer>,
    /// Packed per-frame params + per-slot flame colors.
    #[uniform(1)]
    pub aura: RadianceAuraUniform,
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
    /// flame's mechanism for HDR accumulation inside the 2D pipeline (the
    /// shared `override_additive_blend` recipe — a code span, not a link:
    /// the helper is `pub(crate)` and this doc is public).
    fn specialize(
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: Material2dKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        override_additive_blend(descriptor);
        Ok(())
    }
}

#[cfg(feature = "body-tracking-mediapipe")]
use wc_core::lifecycle::screensaver::fade::ScreensaverFade;

#[cfg(feature = "body-tracking-mediapipe")]
use crate::radiance::settings::RadianceSettings;
#[cfg(feature = "body-tracking-mediapipe")]
use crate::radiance::systems::sim_params::RadianceState;
#[cfg(feature = "body-tracking-mediapipe")]
use crate::radiance::systems::spawn::RadianceRoot;

/// Per-slot silhouette identity: rim color + fade for each body slot.
///
/// Struct parity: mirrors `SilhouetteSlots` in
/// `shaders/radiance/silhouette.wgsl`.
#[derive(ShaderType, Clone, Copy, Debug)]
pub struct RadianceSilhouetteSlots {
    /// Per-slot emissive rim color (linear HDR, the body's identity color).
    pub rim_colors: [Vec4; 4],
    /// Per-slot fade envelope (`TrackedBody::fade`); fill alpha and rim
    /// brightness both ride it so figures ease in/out.
    pub fades: Vec4,
}

impl Default for RadianceSilhouetteSlots {
    /// Slot 0 fully faded in (the synthetic/phantom writers' slot — also the
    /// pre-first-frame state before the driver retargets it), others off.
    fn default() -> Self {
        Self {
            rim_colors: [Vec4::ONE; 4],
            fades: Vec4::new(1.0, 0.0, 0.0, 0.0),
        }
    }
}

/// The window-filling silhouette material sampling the person mask.
///
/// Drawn under the particles (spawned at z 0.0 vs the billboards' z 1.0) via
/// ordinary alpha blending; only the rims are HDR-emissive. All four mask
/// channels render simultaneously with per-slot color + fade.
#[cfg(feature = "body-tracking-mediapipe")]
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct RadianceSilhouetteMaterial {
    /// The shared 256² `Rgba8Unorm` multi-body mask (channel `i` = body slot
    /// `i`, the pinned convention; Plan B writes it in place and Bevy
    /// re-uploads on mutation).
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
    /// Per-slot rim colors + fades.
    #[uniform(5)]
    pub slots: RadianceSilhouetteSlots,
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

/// Derive the four body-slot color identities for one frame: the palette's
/// saturated mid stop, hue-rotated by the psychedelic cycle phase plus
/// `slot × hue_spread` (see [`SLOT_HUE_STEPS`]), then ember-blended by the
/// screensaver fade envelope (the attract ember keeps its authored warmth —
/// only the user identity rotates). Pure for testability.
///
/// The mid stop is the identity anchor because the flame's temperature ramp
/// (render.wgsl `flame_color`) supplies the white-hot and ember extremes
/// itself — one saturated hue per dancer keeps the multi-body look coherent.
#[must_use]
pub fn slot_identity_colors(
    palette: RadiancePalette,
    hue_phase: f32,
    hue_spread: f32,
    ember_alpha: f32,
) -> [Vec4; 4] {
    let a = ember_alpha.clamp(0.0, 1.0);
    let base = palette.stops()[1];
    let ember = RadiancePalette::Ember.stops()[1];
    SLOT_HUE_STEPS.map(|step| rotate_hue(base, hue_phase + step * hue_spread).lerp(ember, a))
}

/// Pack the aura uniform for one frame from the smoothed audio state and the
/// per-slot identity colors. Pure for testability.
#[cfg(feature = "body-tracking-mediapipe")]
#[must_use]
pub fn particle_material_params(
    state: &RadianceState,
    slot_colors: [Vec4; 4],
    fade_alpha: f32,
    elapsed: f32,
) -> RadianceAuraUniform {
    let a = fade_alpha.clamp(0.0, 1.0);
    // Attract mode dims toward the ember: intensity eases to 70%.
    let intensity = state.intensity * AURA_BRIGHTNESS * (1.0 - a * 0.3);
    // Audio-swelled billboard size: the flame visibly breathes with the music
    // (intensity term) and swells on the beat-weighted bass drive. Clamped at
    // 1.5x — fill cost scales with the square of this factor, so the ceiling
    // bounds the worst-case raster load at ~2.25x for beat instants only.
    let quad_half =
        QUAD_HALF_PX * (0.85 + 0.2 * state.intensity + 0.35 * state.bass_drive).min(1.5);
    RadianceAuraUniform {
        params: Vec4::new(intensity, quad_half, state.sparkle, elapsed),
        slot_colors,
    }
}

/// Per-slot fade vector from the tracking state, with the phantom fallback:
/// when no slot is occupied at all (the attract phantom and the
/// pre-tracking spawn frame write mask/edges without a `TrackedBody`),
/// slot 0 gets full fade so those single-body writers keep rendering.
/// Occupied slots ride their real envelope — including fading-out bodies,
/// which is exactly what keeps a leaving dancer's figure easing away.
#[cfg(feature = "body-tracking-mediapipe")]
#[must_use]
pub fn slot_fades(body: Option<&wc_core::input::body::BodyTrackingState>) -> Vec4 {
    let Some(state) = body else {
        return Vec4::new(1.0, 0.0, 0.0, 0.0);
    };
    let mut fades = Vec4::ZERO;
    let mut any = false;
    for b in state.iter_bodies() {
        if b.slot < 4 {
            fades[b.slot] = b.fade.clamp(0.0, 1.0);
            any = true;
        }
    }
    if any {
        fades
    } else {
        Vec4::new(1.0, 0.0, 0.0, 0.0)
    }
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
    body: Option<Res<'_, wc_core::input::body::BodyTrackingState>>,
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
    // One derivation feeds every layer (particles, rims, and — via the same
    // helper — pulses and sparkles), so the per-body identity can never
    // drift between draws.
    let slot_colors = slot_identity_colors(
        settings.palette,
        state.hue_phase,
        settings.hue_spread,
        fade.alpha(),
    );
    let aura = particle_material_params(&state, slot_colors, fade.alpha(), time.elapsed_secs());
    for handle in &particle_roots {
        if let Some(mut material) = particle_materials.get_mut(&handle.0) {
            material.aura = aura;
        }
    }
    // Rim brightness rides the audio: every dancer's outline glows with the
    // level and flashes on every onset — the single most legible reactive
    // lane on a busy floor. Per-slot rim colors are the identity colors with
    // HDR headroom so the rims bloom.
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
    let slots = RadianceSilhouetteSlots {
        rim_colors: slot_colors.map(|c| (c * 1.3).with_w(1.0)),
        fades: slot_fades(body.as_deref()),
    };
    for handle in &silhouette_roots {
        if let Some(mut material) = silhouette_materials.get_mut(&handle.0) {
            material.fill_params = fill_params;
            material.effect_params = effect_params;
            material.fill_color = silhouette_fill_color();
            material.slots = slots;
        }
    }
}

#[cfg(all(test, feature = "body-tracking-mediapipe"))]
mod tests {
    use super::*;

    /// The uniforms' WGSL layout sizes: `AuraUniform` in `render.wgsl` is
    /// `params: vec4` (16 B) + `slot_colors: array<vec4, 4>` (64 B) = 80 B;
    /// `SilhouetteSlots` in `silhouette.wgsl` is
    /// `rim_colors: array<vec4, 4>` (64 B) + `fades: vec4` (16 B) = 80 B.
    /// Struct parity with the hand-written WGSL is by convention, so this
    /// locks the Rust side's sizes against silent field drift.
    #[test]
    fn material_uniform_sizes_match_wgsl() {
        use bevy::render::render_resource::ShaderType as _;
        assert_eq!(
            RadianceAuraUniform::min_size().get(),
            80,
            "RadianceAuraUniform must stay (1 + 4) vec4s"
        );
        assert_eq!(
            RadianceSilhouetteSlots::min_size().get(),
            80,
            "RadianceSilhouetteSlots must stay (4 + 1) vec4s"
        );
    }

    /// Fade 0 (Active) uses the palette identity verbatim; fade 1 lands on
    /// the ember identity with intensity eased to 70%.
    #[test]
    fn particle_params_blend_to_ember_on_fade() {
        let state = RadianceState {
            onset_env: 0.0,
            intensity: 1.0,
            sparkle: 0.4,
            ..RadianceState::default()
        };
        let c0 = slot_identity_colors(RadiancePalette::Prism, 0.0, 0.0, 0.0);
        let a0 = particle_material_params(&state, c0, 0.0, 3.0);
        assert!((a0.params.x - AURA_BRIGHTNESS).abs() < 1e-6);
        assert!((a0.params.z - 0.4).abs() < 1e-6, "sparkle lane");
        assert!((a0.params.w - 3.0).abs() < 1e-6, "elapsed lane");
        assert_eq!(c0[0], RadiancePalette::Prism.stops()[1]);
        let c1 = slot_identity_colors(RadiancePalette::Prism, 0.0, 0.0, 1.0);
        let a1 = particle_material_params(&state, c1, 1.0, 3.0);
        assert!(
            (a1.params.x - 0.7 * AURA_BRIGHTNESS).abs() < 1e-6,
            "ember intensity ease"
        );
        assert_eq!(c1[0], RadiancePalette::Ember.stops()[1]);
    }

    /// The quad half-size lane swells with intensity + the bass drive and
    /// clamps at 1.5x the base constant.
    #[test]
    fn particle_params_swell_quad_half_with_audio() {
        let colors = [Vec4::ONE; 4];
        let quiet = RadianceState::default();
        let a = particle_material_params(&quiet, colors, 0.0, 0.0);
        assert!(
            (a.params.y - QUAD_HALF_PX * 0.85).abs() < 1e-5,
            "quiet floor: {}",
            a.params.y
        );
        let loud = RadianceState {
            intensity: 1.0,
            bass_drive: 1.0,
            ..RadianceState::default()
        };
        let a_loud = particle_material_params(&loud, colors, 0.0, 0.0);
        assert!(a_loud.params.y > a.params.y, "audio must swell the flame");
        let slammed = RadianceState {
            intensity: 2.5,
            bass_drive: 1.0,
            ..RadianceState::default()
        };
        let a_max = particle_material_params(&slammed, colors, 0.0, 0.0);
        assert!(
            (a_max.params.y - QUAD_HALF_PX * 1.5).abs() < 1e-5,
            "swell clamps at 1.5x: {}",
            a_max.params.y
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

    /// Slot identities: slot 0 is the palette hue verbatim; the spread
    /// rotates each further slot by `slot × spread` turns; every slot keeps
    /// the same HDR value (distinct but equally vivid dancers); spread 0
    /// collapses to one shared identity.
    #[test]
    fn slot_identities_spread_harmoniously() {
        let spread = 0.13;
        let colors = slot_identity_colors(RadiancePalette::Prism, 0.2, spread, 0.0);
        let base = RadiancePalette::Prism.stops()[1];
        for (slot, c) in colors.iter().enumerate() {
            #[allow(
                clippy::as_conversions,
                clippy::cast_precision_loss,
                reason = "slot index 0..4, exact in f32"
            )]
            let expect = rotate_hue(base, 0.2 + slot as f32 * spread);
            assert_eq!(*c, expect, "slot {slot}");
            let value = c.x.max(c.y).max(c.z);
            let expect_value = base.x.max(base.y).max(base.z);
            assert!(
                (value - expect_value).abs() < 1e-5,
                "equal vividness: slot {slot} {c}"
            );
        }
        assert_ne!(colors[0], colors[1], "spread separates identities");
        let flat = slot_identity_colors(RadiancePalette::Prism, 0.2, 0.0, 0.0);
        assert_eq!(flat[0], flat[3], "zero spread collapses to one identity");
    }

    /// The fade vector mirrors occupied slots' envelopes (including
    /// fading-out bodies) and falls back to slot 0 when nothing is tracked
    /// (the phantom/synthetic single-body writers).
    #[test]
    fn slot_fades_ride_envelopes_with_phantom_fallback() {
        use wc_core::input::body::{BodyTrackingState, TrackedBody};
        assert_eq!(slot_fades(None), Vec4::new(1.0, 0.0, 0.0, 0.0));
        let empty = BodyTrackingState::default();
        assert_eq!(
            slot_fades(Some(&empty)),
            Vec4::new(1.0, 0.0, 0.0, 0.0),
            "no occupied slot -> phantom fallback"
        );
        let mut state = BodyTrackingState::default();
        state.bodies[1] = Some(TrackedBody {
            slot: 1,
            present: true,
            fade: 0.6,
            ..TrackedBody::default()
        });
        state.bodies[2] = Some(TrackedBody {
            slot: 2,
            present: false, // fading out
            fade: 0.25,
            ..TrackedBody::default()
        });
        let fades = slot_fades(Some(&state));
        assert_eq!(fades, Vec4::new(0.0, 0.6, 0.25, 0.0));
    }
}
