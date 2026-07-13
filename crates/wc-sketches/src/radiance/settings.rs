//! Radiance sketch settings.
//!
//! Storage key `"radiance"`. Radiance *listens* rather than plays: there is no
//! synth section. `audio_input_device` is the app's first `RuntimeEnum`
//! setting — its option list comes from Plan A's device enumeration registered
//! under the `"audio_input_devices"` options key — and is `requires_restart`
//! so a device change tears down and rebuilds the capture stream via the
//! standard reload path. `particle_count` is `requires_restart` because the
//! GPU particle buffer and billboard mesh are sized once at spawn.
//!
//! Per-field serde defaults follow the house pattern: every field carries
//! `#[serde(default = "default_<name>")]` so legacy TOML deserializes cleanly,
//! and the two defaults-match tests below keep both sites in sync.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

/// Curated psychedelic gradient palettes for the aura particles. Each palette
/// is three linear-HDR gradient stops (values may exceed 1.0 — the additive
/// pipeline + bloom read them as emissive headroom); the render shader
/// interpolates a→b→c over the per-particle gradient coordinate, and the audio
/// drive slowly shifts that coordinate along the gradient.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RadiancePalette {
    /// Violet → magenta → gold. The default "prismatic" look.
    #[default]
    Prism,
    /// Deep red → orange → warm white. Also the screensaver's ember override.
    Ember,
    /// Teal → green → violet.
    Aurora,
    /// Deep blue → cyan → pale ice.
    Ocean,
}

impl RadiancePalette {
    /// The three linear-HDR gradient stops `[a, b, c]` (w unused, kept 1.0).
    #[must_use]
    pub fn stops(self) -> [Vec4; 3] {
        match self {
            Self::Prism => [
                Vec4::new(0.35, 0.10, 1.00, 1.0),
                Vec4::new(1.00, 0.25, 0.85, 1.0),
                Vec4::new(1.00, 0.85, 0.30, 1.0),
            ],
            Self::Ember => [
                Vec4::new(0.50, 0.08, 0.02, 1.0),
                Vec4::new(1.00, 0.35, 0.05, 1.0),
                Vec4::new(1.00, 0.80, 0.35, 1.0),
            ],
            Self::Aurora => [
                Vec4::new(0.05, 0.60, 0.50, 1.0),
                Vec4::new(0.20, 0.90, 0.40, 1.0),
                Vec4::new(0.60, 0.40, 1.00, 1.0),
            ],
            Self::Ocean => [
                Vec4::new(0.05, 0.25, 0.90, 1.0),
                Vec4::new(0.10, 0.70, 1.00, 1.0),
                Vec4::new(0.70, 0.95, 1.00, 1.0),
            ],
        }
    }
}

/// User-tunable parameters for the Radiance sketch.
// `mirror`, `mask_debug_overlay`, `edge_debug`, and `inference_readouts` are
// four independent, documented toggles (not a state machine); a struct of
// named bools reads clearer here than an enum/bitflags encoding.
#[allow(clippy::struct_excessive_bools)]
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "radiance")]
pub struct RadianceSettings {
    /// GPU particle budget. The storage buffer and billboard mesh are sized
    /// once at spawn, so this requires a restart (reload fade) to apply.
    #[setting(
        default = 120_000.0_f32,
        min = 10_000.0_f32,
        max = 300_000.0_f32,
        step = 10_000.0_f32,
        label = "Particle count",
        section = "Simulation",
        category = User,
        requires_restart
    )]
    #[serde(default = "default_particle_count")]
    pub particle_count: f32,

    /// Baseline emission: the per-second respawn pressure on dead particles
    /// (scaled by the bass drive). 0 = no new particles.
    #[setting(
        default = 0.5_f32,
        min = 0.0_f32,
        max = 1.0_f32,
        step = 0.01_f32,
        label = "Emission",
        section = "Simulation",
        category = User
    )]
    #[serde(default = "default_emission_rate")]
    pub emission_rate: f32,

    /// Curl-noise flow advection speed in world px/s (scaled by the highs
    /// drive). The primary "how alive is the aura" knob.
    #[setting(
        default = 90.0_f32,
        min = 0.0_f32,
        max = 400.0_f32,
        step = 5.0_f32,
        label = "Flow strength",
        section = "Simulation",
        category = User
    )]
    #[serde(default = "default_flow_strength")]
    pub flow_strength: f32,

    /// Constant upward acceleration in world px/s² — the flame-like rise
    /// (pulsed by the bass drive).
    #[setting(
        default = 60.0_f32,
        min = 0.0_f32,
        max = 300.0_f32,
        step = 5.0_f32,
        label = "Buoyancy",
        section = "Simulation",
        category = User
    )]
    #[serde(default = "default_buoyancy")]
    pub buoyancy: f32,

    /// Curl-noise octave count (1–3). More octaves = finer swirl detail at a
    /// small per-particle ALU cost.
    #[setting(
        default = 3_u32,
        min = 1_u32,
        max = 3_u32,
        step = 1_u32,
        label = "Curl octaves",
        section = "Simulation",
        category = Dev
    )]
    #[serde(default = "default_curl_octaves")]
    pub curl_octaves: u32,

    /// Aura gradient palette.
    #[setting(
        default = RadiancePalette::Prism,
        ty = Enum,
        label = "Palette",
        section = "Look",
        category = User
    )]
    #[serde(default = "default_palette")]
    pub palette: RadiancePalette,

    /// Silhouette fill intensity: strength of the dark glassy body fill.
    #[setting(
        default = 0.8_f32,
        min = 0.0_f32,
        max = 2.0_f32,
        step = 0.05_f32,
        label = "Silhouette fill",
        section = "Look",
        category = User
    )]
    #[serde(default = "default_silhouette_fill")]
    pub silhouette_fill: f32,

    /// Emissive rim brightness in the mask's edge band (HDR — feeds bloom).
    #[setting(
        default = 1.2_f32,
        min = 0.0_f32,
        max = 4.0_f32,
        step = 0.05_f32,
        label = "Rim glow",
        section = "Look",
        category = User
    )]
    #[serde(default = "default_rim_glow")]
    pub rim_glow: f32,

    /// Mirror the image horizontally (it is a mirror for the dancer). On by
    /// default per the spec.
    #[setting(
        default = true,
        label = "Mirror",
        section = "Look",
        category = User
    )]
    #[serde(default = "default_mirror")]
    pub mirror: bool,

    /// Master scale on every audio→visual coupling (emission, buoyancy,
    /// turbulence, burst, intensity). 0 = motion-drive only.
    #[setting(
        default = 1.0_f32,
        min = 0.0_f32,
        max = 3.0_f32,
        step = 0.05_f32,
        label = "Audio sensitivity",
        section = "Audio",
        category = User
    )]
    #[serde(default = "default_audio_sensitivity")]
    pub audio_sensitivity: f32,

    /// Capture device name. Empty = system default input. Options come from
    /// the runtime-enum source registered under `"audio_input_devices"` (Plan
    /// A's cpal enumeration); restart rebuilds the stream on the new device.
    #[setting(
        default = String::new(),
        ty = RuntimeEnum,
        options_key = "audio_input_devices",
        label = "Audio input",
        section = "Audio",
        category = User,
        requires_restart
    )]
    #[serde(default = "default_audio_input_device")]
    pub audio_input_device: String,

    /// Mask threshold for the silhouette fill/rim edge (render-side; the edge
    /// *point* extraction threshold is fixed at 0.5 by the body-tracking
    /// contract).
    #[setting(
        default = 0.5_f32,
        min = 0.05_f32,
        max = 0.95_f32,
        step = 0.01_f32,
        label = "Mask threshold",
        section = "Tracking",
        category = Dev
    )]
    #[serde(default = "default_mask_threshold")]
    pub mask_threshold: f32,

    /// Worker-side temporal EMA factor on the segmentation mask (higher =
    /// steadier, laggier). Routed through the body-tracking request on
    /// restart (Task 14). Default must match
    /// `wc_core::input::body::mask::DEFAULT_MASK_EMA_ALPHA` (`0.35`).
    #[setting(
        default = 0.35_f32,
        min = 0.0_f32,
        max = 0.98_f32,
        step = 0.02_f32,
        label = "Mask smoothing",
        section = "Tracking",
        category = Dev,
        requires_restart
    )]
    #[serde(default = "default_mask_ema")]
    pub mask_ema: f32,

    /// One-Euro landmark filter min-cutoff (Hz). Routed like mask smoothing.
    /// Default must match
    /// `wc_core::input::body::smoothing::DEFAULT_MIN_CUTOFF` (`0.05`).
    #[setting(
        default = 0.05_f32,
        min = 0.01_f32,
        max = 10.0_f32,
        step = 0.01_f32,
        label = "One-Euro min cutoff",
        section = "Tracking",
        category = Dev,
        requires_restart
    )]
    #[serde(default = "default_one_euro_min_cutoff")]
    pub one_euro_min_cutoff: f32,

    /// One-Euro landmark filter beta (speed coefficient). Routed like mask
    /// smoothing. Default must match
    /// `wc_core::input::body::smoothing::DEFAULT_BETA` (`80.0`) — note the
    /// much larger scale than the hand provider's beta (`6.0`): `MediaPipe`'s
    /// pose landmark filter uses body-scale-normalized speed (see
    /// `smoothing::body_scale`), which is a smaller unit than the hand
    /// provider's, so the compensating coefficient is proportionally larger.
    #[setting(
        default = 80.0_f32,
        min = 0.0_f32,
        max = 200.0_f32,
        step = 1.0_f32,
        label = "One-Euro beta",
        section = "Tracking",
        category = Dev,
        requires_restart
    )]
    #[serde(default = "default_one_euro_beta")]
    pub one_euro_beta: f32,

    /// Draw the raw segmentation mask grayscale instead of the styled fill.
    #[setting(
        default = false,
        label = "Mask debug overlay",
        section = "Debug",
        category = Dev
    )]
    #[serde(default = "default_mask_debug_overlay")]
    pub mask_debug_overlay: bool,

    /// Draw a gizmo tick + outward normal at every silhouette edge point.
    #[setting(
        default = false,
        label = "Edge-point debug",
        section = "Debug",
        category = Dev
    )]
    #[serde(default = "default_edge_debug")]
    pub edge_debug: bool,

    /// Show the tracking/audio readout overlay (presence, confidence, body
    /// frame rate, edge count, RMS/onset).
    #[setting(
        default = false,
        label = "Inference readouts",
        section = "Debug",
        category = Dev
    )]
    #[serde(default = "default_inference_readouts")]
    pub inference_readouts: bool,

    /// Camera tonemapping operator while Radiance is active. House default.
    #[setting(
        default = wc_core::render::TonemapChoice::ReinhardLuminance,
        ty = Enum,
        label = "Tonemapping",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_tonemapping")]
    pub tonemapping: wc_core::render::TonemapChoice,

    /// Bloom intensity for this sketch (main camera).
    #[setting(
        default = 0.35_f32,
        min = 0.0_f32,
        max = 1.0_f32,
        step = 0.05_f32,
        label = "Bloom intensity",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_bloom_intensity")]
    pub bloom_intensity: f32,

    /// Bloom prefilter threshold (0.0 pairs with `EnergyConserving`).
    #[setting(
        default = 0.0_f32,
        min = 0.0_f32,
        max = 3.0_f32,
        step = 0.05_f32,
        label = "Bloom threshold",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_bloom_threshold")]
    pub bloom_threshold: f32,

    /// Bloom composite mode.
    #[setting(
        default = wc_core::render::BloomComposite::EnergyConserving,
        ty = Enum,
        label = "Bloom composite",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_bloom_composite")]
    pub bloom_composite: wc_core::render::BloomComposite,
}

/// Ties `RadianceSettings` to the shared sketch lifecycle glue.
impl wc_core::sketch::SketchLifecycle for RadianceSettings {
    const STATE: wc_core::lifecycle::state::AppState =
        wc_core::lifecycle::state::AppState::Radiance;

    fn render_profile(&self) -> wc_core::sketch::RenderProfile {
        wc_core::sketch::RenderProfile {
            tonemapping: self.tonemapping,
            bloom_intensity: self.bloom_intensity,
            bloom_threshold: self.bloom_threshold,
            bloom_composite: self.bloom_composite,
        }
    }
}

// Per-field serde defaults. Values MUST match the `#[setting(default = ...)]`
// attributes above; update both sites together.
fn default_particle_count() -> f32 {
    120_000.0
}
fn default_emission_rate() -> f32 {
    0.5
}
fn default_flow_strength() -> f32 {
    90.0
}
fn default_buoyancy() -> f32 {
    60.0
}
fn default_curl_octaves() -> u32 {
    3
}
fn default_palette() -> RadiancePalette {
    RadiancePalette::Prism
}
fn default_silhouette_fill() -> f32 {
    0.8
}
fn default_rim_glow() -> f32 {
    1.2
}
fn default_mirror() -> bool {
    true
}
fn default_audio_sensitivity() -> f32 {
    1.0
}
fn default_audio_input_device() -> String {
    String::new()
}
fn default_mask_threshold() -> f32 {
    0.5
}
fn default_mask_ema() -> f32 {
    0.35
}
fn default_one_euro_min_cutoff() -> f32 {
    0.05
}
fn default_one_euro_beta() -> f32 {
    80.0
}
fn default_mask_debug_overlay() -> bool {
    false
}
fn default_edge_debug() -> bool {
    false
}
fn default_inference_readouts() -> bool {
    false
}
fn default_tonemapping() -> wc_core::render::TonemapChoice {
    wc_core::render::TonemapChoice::ReinhardLuminance
}
fn default_bloom_intensity() -> f32 {
    0.35
}
fn default_bloom_threshold() -> f32 {
    0.0
}
fn default_bloom_composite() -> wc_core::render::BloomComposite {
    wc_core::render::BloomComposite::EnergyConserving
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Legacy persisted TOML missing fields still deserializes cleanly;
    /// siblings preserved (per-field serde defaults, the house pattern).
    #[test]
    #[allow(clippy::expect_used, reason = "test-only")]
    fn missing_field_preserves_sibling_values() {
        let legacy = r"
            emission_rate = 0.7
            mirror = false
        ";
        let parsed: RadianceSettings = toml::from_str(legacy).expect("legacy TOML must parse");
        assert!((parsed.emission_rate - 0.7).abs() < 1e-6);
        assert!(!parsed.mirror);
        assert!(
            (parsed.particle_count - 120_000.0).abs() < 1e-6,
            "sibling default"
        );
        assert!(
            (parsed.flow_strength - 90.0).abs() < 1e-6,
            "sibling default"
        );
        assert_eq!(parsed.palette, RadiancePalette::Prism, "sibling default");
        assert!(
            parsed.audio_input_device.is_empty(),
            "missing device falls back to system default"
        );
    }

    /// Every `#[setting(default = ...)]` matches its `default_*` serde fn.
    #[test]
    fn default_values_match_serde_defaults() {
        let d = RadianceSettings::default();
        assert!((d.particle_count - default_particle_count()).abs() < f32::EPSILON);
        assert!((d.emission_rate - default_emission_rate()).abs() < f32::EPSILON);
        assert!((d.flow_strength - default_flow_strength()).abs() < f32::EPSILON);
        assert!((d.buoyancy - default_buoyancy()).abs() < f32::EPSILON);
        assert_eq!(d.curl_octaves, default_curl_octaves());
        assert_eq!(d.palette, default_palette());
        assert!((d.silhouette_fill - default_silhouette_fill()).abs() < f32::EPSILON);
        assert!((d.rim_glow - default_rim_glow()).abs() < f32::EPSILON);
        assert_eq!(d.mirror, default_mirror());
        assert!((d.audio_sensitivity - default_audio_sensitivity()).abs() < f32::EPSILON);
        assert_eq!(d.audio_input_device, default_audio_input_device());
        assert!((d.mask_threshold - default_mask_threshold()).abs() < f32::EPSILON);
        assert!((d.mask_ema - default_mask_ema()).abs() < f32::EPSILON);
        assert!((d.one_euro_min_cutoff - default_one_euro_min_cutoff()).abs() < f32::EPSILON);
        assert!((d.one_euro_beta - default_one_euro_beta()).abs() < f32::EPSILON);
        assert_eq!(d.mask_debug_overlay, default_mask_debug_overlay());
        assert_eq!(d.edge_debug, default_edge_debug());
        assert_eq!(d.inference_readouts, default_inference_readouts());
        assert_eq!(d.tonemapping, default_tonemapping());
        assert!((d.bloom_intensity - default_bloom_intensity()).abs() < f32::EPSILON);
        assert!((d.bloom_threshold - default_bloom_threshold()).abs() < f32::EPSILON);
        assert_eq!(d.bloom_composite, default_bloom_composite());
    }

    /// Every palette returns three finite stops (HDR values allowed above 1).
    #[test]
    fn palette_stops_are_finite() {
        for p in [
            RadiancePalette::Prism,
            RadiancePalette::Ember,
            RadiancePalette::Aurora,
            RadiancePalette::Ocean,
        ] {
            for stop in p.stops() {
                assert!(stop.is_finite(), "{p:?} stop {stop:?}");
                assert!(stop.min_element() >= 0.0, "{p:?} stop {stop:?}");
            }
        }
    }
}
