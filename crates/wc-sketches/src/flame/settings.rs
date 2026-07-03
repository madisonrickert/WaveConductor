//! Flame sketch settings.
//!
//! The `name` is the sketch's identity: it seeds branch count, transforms,
//! colors, and audio character (see `super::branches`). It is a LIVE setting:
//! the name-change watcher rebuilds the fractal in place (no restart fade).
//! `carousel_names` (a `TextList`) holds the names the screensaver cycles
//! through; `super::ui::debounce_name_admission` populates it once a typed
//! name settles.
//!
//! Per-field serde defaults follow the house pattern: every field carries
//! `#[serde(default = "default_<name>")]` so legacy TOML deserializes cleanly.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

/// User-tunable parameters for the Flame sketch.
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "flame")]
pub struct FlameSettings {
    /// The visitor's name. Empty means "use the default placeholder name".
    /// Live: the watcher rebuilds branches + reseeds the node buffer on change.
    #[setting(
        default = String::new(),
        ty = Text,
        label = "Name",
        section = "Identity",
        category = User
    )]
    #[serde(default = "default_name")]
    pub name: String,

    /// Approximate total point budget. The tree depth is
    /// floor(ln(budget)/ln(branches)), so actual totals land under this.
    /// Live: the watcher rebuilds layout + mesh when it changes.
    #[setting(
        default = 100_000.0_f32,
        min = 10_000.0_f32,
        max = 200_000.0_f32,
        step = 10_000.0_f32,
        label = "Point budget",
        section = "Fractal",
        category = Dev
    )]
    #[serde(default = "default_target_points")]
    pub target_points: f32,

    /// Camera auto-rotation speed. 1.0 = one orbit per minute (v4's
    /// `OrbitControls` autoRotateSpeed = 1). 0 disables.
    #[setting(
        default = 1.0_f32,
        min = 0.0_f32,
        max = 10.0_f32,
        step = 0.1_f32,
        label = "Autorotate speed",
        section = "Camera",
        category = Dev
    )]
    #[serde(default = "default_autorotate_speed")]
    pub autorotate_speed: f32,

    /// Fake depth-of-field strength: the `* 3.0` factor in v4's
    /// `outOfFocusAmount`. 0 disables the `DoF` entirely.
    #[setting(
        default = 3.0_f32,
        min = 0.0_f32,
        max = 10.0_f32,
        step = 0.1_f32,
        label = "DoF strength",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_dof_strength")]
    pub dof_strength: f32,

    /// In-focus point size in pixels (v4 `originalSize = 2.0`).
    #[setting(
        default = 2.0_f32,
        min = 0.5_f32,
        max = 8.0_f32,
        step = 0.1_f32,
        label = "Point size",
        unit = "px",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_base_point_size")]
    pub base_point_size: f32,

    /// Per-point base opacity (v4 material `opacity = 0.2`). The additive
    /// accumulation of ~100k points does the brightening.
    #[setting(
        default = 0.2_f32,
        min = 0.0_f32,
        max = 1.0_f32,
        step = 0.01_f32,
        label = "Point opacity",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_point_opacity")]
    pub point_opacity: f32,

    /// Point size clamp in pixels (v4 `min(50., gl_PointSize)`); bounds
    /// additive overdraw when zoomed close.
    #[setting(
        default = 50.0_f32,
        min = 4.0_f32,
        max = 128.0_f32,
        step = 1.0_f32,
        label = "Point size clamp",
        unit = "px",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_point_size_clamp")]
    pub point_size_clamp: f32,

    /// Fog start distance in view units (v4 `THREE.Fog(bg, 2, 60)`).
    #[setting(
        default = 2.0_f32,
        min = 0.0_f32,
        max = 20.0_f32,
        step = 0.5_f32,
        label = "Fog near",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_fog_near")]
    pub fog_near: f32,

    /// Fog full-fade distance in view units.
    #[setting(
        default = 60.0_f32,
        min = 5.0_f32,
        max = 200.0_f32,
        step = 5.0_f32,
        label = "Fog far",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_fog_far")]
    pub fog_far: f32,

    /// Output gamma applied in the render shader (v4 baked pow(0.545)).
    /// Starting point for the operator's AgX-era eye tune.
    #[setting(
        default = 0.545_f32,
        min = 0.1_f32,
        max = 4.0_f32,
        step = 0.005_f32,
        label = "Gamma",
        section = "Visual",
        category = User
    )]
    #[serde(default = "default_gamma")]
    pub gamma: f32,

    /// Pre-tonemap exposure multiplier on the point contribution. Mirrors
    /// the Dots/Line/Cymatics `master_brightness` knob.
    #[setting(
        default = 1.0_f32,
        min = 0.0_f32,
        max = 3.0_f32,
        step = 0.05_f32,
        label = "Master brightness",
        section = "Visual",
        category = User
    )]
    #[serde(default = "default_master_brightness")]
    pub master_brightness: f32,

    /// Camera tonemapping operator while Flame is active. House default.
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

    /// Seconds between screensaver carousel advances.
    #[setting(
        default = 120.0_f32,
        min = 15.0_f32,
        max = 600.0_f32,
        step = 15.0_f32,
        label = "Carousel period",
        unit = "s",
        section = "Screensaver",
        category = Dev
    )]
    #[serde(default = "default_carousel_period_secs")]
    pub carousel_period_secs: f32,

    /// Fraction of full complexity the ember decays to during the
    /// screensaver (Madison: "40-60%"). 1.0 disables the decay.
    #[setting(
        default = 0.5_f32,
        min = 0.2_f32,
        max = 1.0_f32,
        step = 0.05_f32,
        label = "Ember fraction",
        section = "Screensaver",
        category = Dev
    )]
    #[serde(default = "default_ember_fraction")]
    pub ember_fraction: f32,

    /// Brightness lift past the tonemapper's white knee during attract mode
    /// (the Dots-established pattern, default 2.2).
    #[setting(
        default = 2.2_f32,
        min = 1.0_f32,
        max = 4.0_f32,
        step = 0.1_f32,
        label = "Attract brightness",
        section = "Screensaver",
        category = Dev
    )]
    #[serde(default = "default_attract_brightness")]
    pub attract_brightness: f32,

    /// Names admitted by `super::ui::admit_name` once a typed name settles
    /// (`super::ui::NAME_SETTLE_SECS` of no further edits) — the screensaver
    /// carousel cycles this list. Front is most recent; editable/reorderable
    /// in the dock via the `TextList` widget.
    #[setting(
        default = Vec::new(),
        ty = TextList,
        label = "Carousel names",
        section = "Screensaver",
        category = User
    )]
    #[serde(default = "default_carousel_names")]
    pub carousel_names: Vec<String>,

    /// Scale on the CPU morph-energy proxy (analytic |dcX/dt| + warp speed)
    /// before it enters the synth's v4 velocity curves. The primary ear-tune
    /// knob standing in for v4's measured point velocity.
    #[setting(
        default = 1.0_f32,
        min = 0.0_f32,
        max = 10.0_f32,
        step = 0.1_f32,
        label = "Morph energy scale",
        section = "Audio",
        category = Dev
    )]
    #[serde(default = "default_morph_energy_scale")]
    pub morph_energy_scale: f32,

    /// Stand-in for v4's `count^2 / 8` chord-gain factor (box-count `count`
    /// has no v5 source). Ear-tune target.
    #[setting(
        default = 1.0_f32,
        min = 0.0_f32,
        max = 8.0_f32,
        step = 0.1_f32,
        label = "Chord energy scale",
        section = "Audio",
        category = Dev
    )]
    #[serde(default = "default_chord_energy_scale")]
    pub chord_energy_scale: f32,

    /// Master output trim for the Flame synth voice.
    #[setting(
        default = 1.0_f32,
        min = 0.0_f32,
        max = 2.0_f32,
        step = 0.05_f32,
        label = "Synth volume",
        section = "Audio",
        category = User
    )]
    #[serde(default = "default_synth_volume_scale")]
    pub synth_volume_scale: f32,

    /// Morph-energy envelope attack, ms.
    #[setting(
        default = 120.0_f32,
        min = 5.0_f32,
        max = 500.0_f32,
        step = 5.0_f32,
        label = "Synth attack",
        unit = "ms",
        section = "Audio",
        category = Dev
    )]
    #[serde(default = "default_synth_attack_ms")]
    pub synth_attack_ms: f32,

    /// Morph-energy envelope release, ms.
    #[setting(
        default = 600.0_f32,
        min = 100.0_f32,
        max = 5000.0_f32,
        step = 50.0_f32,
        label = "Synth release",
        unit = "ms",
        section = "Audio",
        category = Dev
    )]
    #[serde(default = "default_synth_release_ms")]
    pub synth_release_ms: f32,
}

/// Ties `FlameSettings` to the shared sketch lifecycle glue.
impl wc_core::sketch::SketchLifecycle for FlameSettings {
    const STATE: wc_core::lifecycle::state::AppState = wc_core::lifecycle::state::AppState::Flame;

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
fn default_name() -> String {
    String::new()
}
fn default_target_points() -> f32 {
    100_000.0
}
fn default_autorotate_speed() -> f32 {
    1.0
}
fn default_dof_strength() -> f32 {
    3.0
}
fn default_base_point_size() -> f32 {
    2.0
}
fn default_point_opacity() -> f32 {
    0.2
}
fn default_point_size_clamp() -> f32 {
    50.0
}
fn default_fog_near() -> f32 {
    2.0
}
fn default_fog_far() -> f32 {
    60.0
}
fn default_gamma() -> f32 {
    0.545
}
fn default_master_brightness() -> f32 {
    1.0
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
fn default_carousel_period_secs() -> f32 {
    120.0
}
fn default_ember_fraction() -> f32 {
    0.5
}
fn default_attract_brightness() -> f32 {
    2.2
}
fn default_carousel_names() -> Vec<String> {
    Vec::new()
}
fn default_morph_energy_scale() -> f32 {
    1.0
}
fn default_chord_energy_scale() -> f32 {
    1.0
}
fn default_synth_volume_scale() -> f32 {
    1.0
}
fn default_synth_attack_ms() -> f32 {
    120.0
}
fn default_synth_release_ms() -> f32 {
    600.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Legacy persisted TOML missing fields still deserializes cleanly;
    /// siblings preserved (per-field serde defaults, the house pattern).
    #[test]
    #[allow(clippy::expect_used, reason = "test-only")]
    fn missing_field_preserves_sibling_values() {
        let legacy = r#"
            name = "madison"
            gamma = 0.6
        "#;
        let parsed: FlameSettings = toml::from_str(legacy).expect("legacy TOML must parse");
        assert_eq!(parsed.name, "madison");
        assert!((parsed.gamma - 0.6).abs() < 1e-6);
        assert!(
            (parsed.master_brightness - 1.0).abs() < 1e-6,
            "sibling default"
        );
        assert!(
            (parsed.carousel_period_secs - 120.0).abs() < 1e-6,
            "sibling default"
        );
        assert!(
            parsed.carousel_names.is_empty(),
            "missing carousel_names falls back to empty list"
        );
    }

    /// Every `#[setting(default = ...)]` matches its `default_*` serde fn.
    #[test]
    fn default_values_match_serde_defaults() {
        let d = FlameSettings::default();
        assert_eq!(d.name, default_name());
        assert!((d.target_points - default_target_points()).abs() < f32::EPSILON);
        assert!((d.autorotate_speed - default_autorotate_speed()).abs() < f32::EPSILON);
        assert!((d.dof_strength - default_dof_strength()).abs() < f32::EPSILON);
        assert!((d.base_point_size - default_base_point_size()).abs() < f32::EPSILON);
        assert!((d.point_opacity - default_point_opacity()).abs() < f32::EPSILON);
        assert!((d.point_size_clamp - default_point_size_clamp()).abs() < f32::EPSILON);
        assert!((d.fog_near - default_fog_near()).abs() < f32::EPSILON);
        assert!((d.fog_far - default_fog_far()).abs() < f32::EPSILON);
        assert!((d.gamma - default_gamma()).abs() < f32::EPSILON);
        assert!((d.master_brightness - default_master_brightness()).abs() < f32::EPSILON);
        assert_eq!(d.tonemapping, default_tonemapping());
        assert!((d.bloom_intensity - default_bloom_intensity()).abs() < f32::EPSILON);
        assert!((d.bloom_threshold - default_bloom_threshold()).abs() < f32::EPSILON);
        assert_eq!(d.bloom_composite, default_bloom_composite());
        assert!((d.carousel_period_secs - default_carousel_period_secs()).abs() < f32::EPSILON);
        assert!((d.ember_fraction - default_ember_fraction()).abs() < f32::EPSILON);
        assert!((d.attract_brightness - default_attract_brightness()).abs() < f32::EPSILON);
        assert_eq!(d.carousel_names, default_carousel_names());
        assert!((d.morph_energy_scale - default_morph_energy_scale()).abs() < f32::EPSILON);
        assert!((d.chord_energy_scale - default_chord_energy_scale()).abs() < f32::EPSILON);
        assert!((d.synth_volume_scale - default_synth_volume_scale()).abs() < f32::EPSILON);
        assert!((d.synth_attack_ms - default_synth_attack_ms()).abs() < f32::EPSILON);
        assert!((d.synth_release_ms - default_synth_release_ms()).abs() < f32::EPSILON);
    }
}
