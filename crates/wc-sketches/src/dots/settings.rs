//! Dots sketch settings.
//!
//! Curated knobs for the Dots ("Fabric") sketch, mirroring v4
//! `dots/index.ts` `static settings` which exposed `dot_spacing` and a
//! visual `gamma` knob.
//!
//! ## Serde forward-compatibility
//!
//! Each field carries `#[serde(default = "default_<name>")]` so a legacy
//! persisted TOML written before a new field was added still deserializes
//! cleanly: the missing field falls back to its default, and the sibling
//! fields are preserved. Without per-field defaults, missing one key would
//! fail the whole-section deserialize and silently revert every sibling to
//! default.
//!
//! Apply the same pattern to every settings struct: when adding a field
//! mid-cycle, also add a `default_<name>()` free function and the
//! `#[serde(default = "...")]` attribute.
//!
//! - **`dot_spacing`** — grid spacing between dot centers in canvas pixels.
//!   A smaller value places more dots (higher density); below ~4 px a
//!   1920-wide canvas exceeds 230,000 dots, risking runaway storage-buffer
//!   allocation. Restart on change (the compute pipeline rebuilds its
//!   storage buffer at spawn time).
//! - **`gravity_constant`** — attractor-force scale baked into every
//!   attractor power host-side (matching v4's `gravity_constant = 100`).
//!   `User`-category knob in the Particles panel; live (no restart).
//! - **`hand_power_scale`** — multiplicative trim applied to hand-attractor
//!   power before the `gravity_constant` bake. Close full-grab hands produce
//!   raw powers ~500–2500 vs. the mouse's ~200; `0.3` brings them down
//!   toward the mouse feel. Dev-only knob; live.
//! - **`fabric_tension`** — linear (Hookean) restoring-spring coefficient:
//!   how strongly each particle is pulled toward its immutable
//!   `original_xy` home. `0.0` means no spring; higher values create a
//!   stiffer grid. During the screensaver this is always baked at `0.0` so
//!   the spring does not fight the turbulence morph. `User`-category knob;
//!   live.
//! - **`gamma`** — per-channel gamma curve applied as a final visual
//!   correction step. v4 default = 1.0 (identity). Read live every frame in
//!   `post_params.rs`; no restart required. `User`-category so it appears
//!   without ADVANCED.
//! - **`attract_particle_fraction`** — fraction of particles kept alive
//!   during attract mode (screensaver). The rest fade out and stay dead until
//!   wake. Survivors are chosen by a deterministic per-index hash so the
//!   thinning is spatially uniform. `1.0` = the full field (mechanism
//!   visually off). Dev-only knob.
//! - **`attract_turbulence`** — drift speed of the attract-mode
//!   divergence-free curl-noise flow (world px/s). The screensaver's
//!   primary motion. `0.0` freezes the field. Dev-only knob.
//! - **`synth_volume_scale`** — master output gain trim for the synth voice.
//!   1.0 = unchanged. Lower values reduce loudness without touching system
//!   volume.
//! - **`synth_attack_ms`** — activity envelope attack time in milliseconds.
//!   Smaller = snappier press onset; larger = slower swell-in.
//! - **`synth_release_ms`** — activity envelope release tail in milliseconds.
//!   Smaller = abrupt cutoff on release; larger = long tail.
//! - **`breath_depth`** — amplitude of the modeled in-out breath swell.
//!   0 = no breath modulation; 1 = full ±100% swell (scaled by envelope so
//!   rest is always silent).
//! - **`bandpass_base_hz`** — bandpass cutoff at rest (envelope = 0). The
//!   low-end anchor for the envelope-to-frequency sweep. Dev-only tuning knob.
//! - **`bandpass_range_hz`** — how far the cutoff sweeps above the base across
//!   the full activity envelope `[0, 1]`. Dev-only tuning knob.
//! - **`breath_rate_hz`** — frequency of the modeled breath sine LFO in Hz.
//!   Lower = slower in-out pulse; higher = faster flutter. Dev-only knob.
//! - **`shrink_factor`** — uniform shrink applied to each iteration of the
//!   explode (chromatic-aberration) post-process pass. v4 default = 0.98.
//!   Lower values produce a more compact spiral halo. Dev-only knob; live.
//! - **`explode_focal_smoothing`** — hand-to-focal smoothing time constant (τ,
//!   seconds). Controls how quickly the explode spiral center follows a
//!   grabbing hand. `0.0` = instant snap. Dev-only knob; live.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

/// User-tunable parameters for the Dots (Fabric) sketch.
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "dots")]
pub struct DotsSettings {
    /// Grid spacing between dot centers in canvas pixels. Restart on change
    /// (the compute pipeline rebuilds its storage buffer at spawn time).
    /// A minimum of 4.0 px prevents runaway particle-count allocation on
    /// wide canvases.
    #[setting(
        default = 20.0_f32,
        min = 4.0_f32,
        max = 100.0_f32,
        step = 1.0_f32,
        label = "Dot spacing (px)",
        section = "Particles",
        category = Dev,
        requires_restart
    )]
    #[serde(default = "default_dot_spacing")]
    pub dot_spacing: f32,

    /// Attractor-force scale baked into every attractor power host-side,
    /// matching v4's `gravity_constant = 100`. Raising this makes all
    /// attractors stronger; lowering it weakens them uniformly. Read live
    /// every frame in `sim_params.rs`; no restart required.
    #[setting(
        default = 100.0_f32,
        min = 0.0_f32,
        max = 500.0_f32,
        step = 10.0_f32,
        label = "Gravity",
        section = "Particles",
        category = User
    )]
    #[serde(default = "default_gravity_constant")]
    pub gravity_constant: f32,

    /// Multiplicative trim applied to hand-attractor power before baking
    /// `gravity_constant` in. Close full-grab hands produce raw powers of
    /// ~500–2500 vs. the mouse's ~200; `0.3` brings them down toward the
    /// mouse feel. Dev-only knob; read live every frame.
    #[setting(
        default = 0.3_f32,
        min = 0.0_f32,
        max = 2.0_f32,
        step = 0.05_f32,
        label = "Hand power scale",
        section = "Particles",
        category = Dev
    )]
    #[serde(default = "default_hand_power_scale")]
    pub hand_power_scale: f32,

    /// Linear (Hookean) restoring-spring coefficient: how strongly each
    /// particle is pulled back toward its immutable `original_xy` home.
    /// `0.0` means no linear spring; higher values create a stiffer fabric
    /// that resists displacement and returns more crisply after interaction.
    /// During the screensaver this is always baked at `0.0` so the spring
    /// does not fight the turbulence morph. Read live every frame.
    #[setting(
        default = 1.0_f32,
        min = 0.0_f32,
        max = 5.0_f32,
        step = 0.1_f32,
        label = "Fabric tension",
        section = "Particles",
        category = User
    )]
    #[serde(default = "default_fabric_tension")]
    pub fabric_tension: f32,

    /// Per-channel gamma curve applied as a final visual correction.
    /// v4 default = 1.0 (identity). Read live every frame in `post_params.rs`,
    /// so no restart is required.
    #[setting(
        default = 1.0_f32,
        min = 0.1_f32,
        max = 4.0_f32,
        step = 0.1_f32,
        label = "Gamma",
        section = "Visual",
        category = User
    )]
    #[serde(default = "default_gamma")]
    pub gamma: f32,

    /// Fraction of particles that stay alive during attract mode (screensaver).
    /// The rest fade out over the fade duration and stay dead until wake, when
    /// the normal alpha ramp fades them back in. Survivors are chosen by a
    /// deterministic per-index hash so the thinning is spatially uniform.
    /// `1.0` = the full field (mechanism visually off). Dev-only knob.
    #[setting(
        default = 0.6_f32,
        min = 0.2_f32,
        max = 1.0_f32,
        step = 0.05_f32,
        label = "Attract particle fraction",
        section = "Screensaver",
        category = Dev
    )]
    #[serde(default = "default_attract_particle_fraction")]
    pub attract_particle_fraction: f32,

    /// Attract-mode noise-turbulence drift speed (world px/s): how fast the
    /// divergence-free curl-noise flow advects the screensaver field. The
    /// screensaver's primary slow-morph motion. `0.0` freezes the field.
    /// Only active during the screensaver. Dev-only knob.
    #[setting(
        default = 6.0_f32,
        min = 0.0_f32,
        max = 20.0_f32,
        step = 0.5_f32,
        label = "Attract turbulence",
        section = "Screensaver",
        category = Dev
    )]
    #[serde(default = "default_attract_turbulence")]
    pub attract_turbulence: f32,

    /// Brightness multiplier applied to attract-mode particles so the calm,
    /// fraction-killed field clears the `AgX` tonemapper's white knee. The shader
    /// applies this as `rgb *= 1 + lift` where `lift = fade × (brightness − 1)`,
    /// so `1.0` is a provable no-op (lift = 0) and values above `1.0` add a
    /// progressively stronger brightness push. Fades in with the screensaver
    /// envelope and back out after wake. Dev-only knob.
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

    /// Velocity-tint strength during attract mode: how strongly the particle's
    /// speed is mapped to a colour shift in the shader's `attract_color.x`
    /// channel. Dots' slow turbulence won't trigger the velocity-tint WAKE band,
    /// so this defaults to `0.0` (tint off — only the brightness `y` channel
    /// matters). Dev-only knob.
    #[setting(
        default = 0.0_f32,
        min = 0.0_f32,
        max = 1.0_f32,
        step = 0.05_f32,
        label = "Attract color strength",
        section = "Screensaver",
        category = Dev
    )]
    #[serde(default = "default_attract_color_strength")]
    pub attract_color_strength: f32,

    // ── Visual (explode pass) ─────────────────────────────────────────────────
    /// Uniform shrink factor applied per iteration of the explode
    /// (chromatic-aberration) post-process pass. Controls how fast the
    /// spiral halo "bleeds" outward; lower values produce a more compact
    /// halo. v4 default = 0.98. Dev-only knob; live (no restart).
    #[setting(
        default = 0.98_f32,
        min = 0.9_f32,
        max = 1.0_f32,
        step = 0.005_f32,
        label = "Explode shrink",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_shrink_factor")]
    pub shrink_factor: f32,

    /// Hand-to-focal smoothing time constant (τ, seconds). Controls how
    /// quickly the explode spiral center follows a grabbing hand's world
    /// position. `0.0` = instant snap; larger values = slower, smoother
    /// follow. Mirrors `LineSettings::smear_focal_smoothing`. Dev-only
    /// knob; live.
    #[setting(
        default = 0.25_f32,
        min = 0.0_f32,
        max = 1.0_f32,
        step = 0.05_f32,
        label = "Hand split smoothing",
        unit = "s",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_explode_focal_smoothing")]
    pub explode_focal_smoothing: f32,

    /// Camera tonemapping operator for this sketch. Default `ReinhardLuminance`
    /// (chroma-preserving "neon glow"). Applied to the main camera while Dots
    /// is active; Home resets to SDR. Live, no restart.
    #[setting(
        default = wc_core::render::TonemapChoice::ReinhardLuminance,
        ty = Enum,
        label = "Tonemapping",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_tonemapping")]
    pub tonemapping: wc_core::render::TonemapChoice,

    /// Bloom intensity for this sketch (main camera). Default `0.35` — stronger
    /// glow than the SDR base 0.15. Live, no restart.
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

    /// Bloom prefilter threshold for this sketch. Default `0.0` — blooms the
    /// whole frame, which is what the default `EnergyConserving` composite needs
    /// to conserve energy (a non-zero threshold there would dim the image).
    /// Raise it only alongside `Additive` composite, where it cleanly gates the
    /// glow to bright cores.
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

    /// Bloom composite mode for this sketch (main camera). Default
    /// `EnergyConserving`: the blur is crossfaded into the scene, conserving
    /// energy while the threshold stays `0.0` (no dimming). Switch to `Additive`
    /// for a punchy glow that adds on top — pair that with a non-zero threshold
    /// so only bright cores bloom. Live, no restart.
    #[setting(
        default = wc_core::render::BloomComposite::EnergyConserving,
        ty = Enum,
        label = "Bloom composite",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_bloom_composite")]
    pub bloom_composite: wc_core::render::BloomComposite,

    // ── Audio ────────────────────────────────────────────────────────────────
    /// Master output gain trim applied after the activity envelope.
    /// 1.0 = unchanged. Adjust to balance kiosk loudness without touching
    /// system volume. Applied as `env * breath * synth_volume_scale`.
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

    /// Activity envelope attack time in milliseconds. Smaller = snappier
    /// press onset; larger = slower swell-in. Internally converted to an
    /// envelope lerp rate of `1000 / attack_ms`.
    #[setting(
        default = 115.0_f32,
        min = 5.0_f32,
        max = 200.0_f32,
        step = 5.0_f32,
        label = "Synth attack",
        unit = "ms",
        section = "Audio",
        category = User
    )]
    #[serde(default = "default_synth_attack_ms")]
    pub synth_attack_ms: f32,

    /// Activity envelope release tail in milliseconds. Smaller = abrupt
    /// cutoff on release; larger = long pad tail. Internally converted to an
    /// envelope lerp rate of `1000 / release_ms`.
    #[setting(
        default = 350.0_f32,
        min = 100.0_f32,
        max = 3000.0_f32,
        step = 50.0_f32,
        label = "Synth release",
        unit = "ms",
        section = "Audio",
        category = User
    )]
    #[serde(default = "default_synth_release_ms")]
    pub synth_release_ms: f32,

    /// Amplitude of the modeled in-out breath swell. At 0 there is no breath
    /// modulation; at 1 the volume and cutoff swell ±100% around their
    /// envelope value. Scaled by `env` so the breath is silent at rest.
    #[setting(
        default = 0.3_f32,
        min = 0.0_f32,
        max = 1.0_f32,
        step = 0.05_f32,
        label = "Breath depth",
        section = "Audio",
        category = User
    )]
    #[serde(default = "default_breath_depth")]
    pub breath_depth: f32,

    /// Bandpass cutoff at rest (envelope = 0), in Hz. The low-end anchor
    /// for the envelope-to-frequency sweep. Tune by ear at hardware sign-off.
    /// Approximation of v4's idle end of `120 / normVarLen × avgVel / 100`.
    #[setting(
        default = 110.0_f32,
        min = 50.0_f32,
        max = 1000.0_f32,
        step = 10.0_f32,
        label = "Bandpass base",
        unit = "Hz",
        section = "Audio",
        category = Dev
    )]
    #[serde(default = "default_bandpass_base_hz")]
    pub bandpass_base_hz: f32,

    /// Bandpass cutoff sweep range, in Hz, across the full `[0, 1]` activity
    /// envelope. `cutoff = base + envelope × range`. Tune by ear.
    #[setting(
        default = 280.0_f32,
        min = 50.0_f32,
        max = 4000.0_f32,
        step = 10.0_f32,
        label = "Bandpass range",
        unit = "Hz",
        section = "Audio",
        category = Dev
    )]
    #[serde(default = "default_bandpass_range_hz")]
    pub bandpass_range_hz: f32,

    /// Frequency of the modeled breath sine LFO in Hz. Lower = slower in-out
    /// pulse; higher = faster flutter. Tune by ear to match the particle
    /// in-out motion feel. Dev-only knob.
    #[setting(
        default = 0.7_f32,
        min = 0.1_f32,
        max = 4.0_f32,
        step = 0.1_f32,
        label = "Breath rate",
        unit = "Hz",
        section = "Audio",
        category = Dev
    )]
    #[serde(default = "default_breath_rate_hz")]
    pub breath_rate_hz: f32,
}

// Per-field serde defaults. Values MUST match the `#[setting(default = ...)]`
// attributes above so a missing-field deserialize lands on the same value the
// derive-macro `Default` impl would produce. Update both sites together.
fn default_dot_spacing() -> f32 {
    20.0
}

/// Default value backing both `DotsSettings::gravity_constant` and the const
/// `DOTS_GRAVITY_CONSTANT` in `sim_params.rs` (= 100.0). Keep both in sync.
fn default_gravity_constant() -> f32 {
    100.0
}

fn default_hand_power_scale() -> f32 {
    0.3
}

fn default_fabric_tension() -> f32 {
    1.0
}

fn default_gamma() -> f32 {
    1.0
}

fn default_attract_particle_fraction() -> f32 {
    0.6
}

fn default_attract_turbulence() -> f32 {
    6.0
}

fn default_attract_brightness() -> f32 {
    2.2
}

fn default_attract_color_strength() -> f32 {
    0.0
}

fn default_shrink_factor() -> f32 {
    0.98
}

fn default_explode_focal_smoothing() -> f32 {
    0.25
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

fn default_synth_volume_scale() -> f32 {
    1.0
}

fn default_synth_attack_ms() -> f32 {
    115.0
}

fn default_synth_release_ms() -> f32 {
    350.0
}

fn default_breath_depth() -> f32 {
    0.3
}

fn default_bandpass_base_hz() -> f32 {
    110.0
}

fn default_bandpass_range_hz() -> f32 {
    280.0
}

fn default_breath_rate_hz() -> f32 {
    0.7
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Confirms that legacy persisted TOML missing one field still
    /// deserializes the other fields cleanly. Without per-field
    /// `#[serde(default)]`, a missing key would fail the whole section
    /// and revert every sibling to default.
    #[test]
    #[allow(
        clippy::expect_used,
        reason = "test-only: panic on bad TOML is the intended failure mode"
    )]
    fn missing_field_preserves_sibling_values() {
        let legacy = r"
            dot_spacing = 32.0
        ";
        let parsed: DotsSettings = toml::from_str(legacy).expect("legacy TOML must parse");
        assert!(
            (parsed.dot_spacing - 32.0).abs() < 1e-6,
            "dot_spacing not preserved"
        );
        assert!(
            (parsed.gamma - 1.0).abs() < 1e-6,
            "gamma should fall back to default"
        );
    }

    #[test]
    fn default_values_match_serde_defaults() {
        let defaults = DotsSettings::default();
        assert!((defaults.dot_spacing - default_dot_spacing()).abs() < f32::EPSILON);
        // Particle physics fields added in task 6.
        assert!(
            (defaults.gravity_constant - default_gravity_constant()).abs() < f32::EPSILON,
            "gravity_constant default mismatch"
        );
        assert!(
            (defaults.hand_power_scale - default_hand_power_scale()).abs() < f32::EPSILON,
            "hand_power_scale default mismatch"
        );
        assert!(
            (defaults.fabric_tension - default_fabric_tension()).abs() < f32::EPSILON,
            "fabric_tension default mismatch"
        );
        assert!((defaults.gamma - default_gamma()).abs() < f32::EPSILON);
        assert!(
            (defaults.attract_particle_fraction - default_attract_particle_fraction()).abs()
                < f32::EPSILON
        );
        assert!((defaults.attract_turbulence - default_attract_turbulence()).abs() < f32::EPSILON);
        // Screensaver brightness lift fields added in task 9.
        assert!(
            (defaults.attract_brightness - default_attract_brightness()).abs() < f32::EPSILON,
            "attract_brightness default mismatch"
        );
        assert!(
            (defaults.attract_color_strength - default_attract_color_strength()).abs()
                < f32::EPSILON,
            "attract_color_strength default mismatch"
        );
        // Visual (explode) fields added in task 7.
        assert!(
            (defaults.shrink_factor - default_shrink_factor()).abs() < f32::EPSILON,
            "shrink_factor default mismatch"
        );
        assert!(
            (defaults.explode_focal_smoothing - default_explode_focal_smoothing()).abs()
                < f32::EPSILON,
            "explode_focal_smoothing default mismatch"
        );
        assert_eq!(
            defaults.tonemapping,
            default_tonemapping(),
            "tonemapping default mismatch"
        );
        assert!(
            (defaults.bloom_intensity - default_bloom_intensity()).abs() < f32::EPSILON,
            "bloom_intensity"
        );
        assert!(
            (defaults.bloom_threshold - default_bloom_threshold()).abs() < f32::EPSILON,
            "bloom_threshold"
        );
        // Audio fields added in task 4.
        assert!((defaults.synth_volume_scale - default_synth_volume_scale()).abs() < f32::EPSILON);
        assert!((defaults.synth_attack_ms - default_synth_attack_ms()).abs() < f32::EPSILON);
        assert!((defaults.synth_release_ms - default_synth_release_ms()).abs() < f32::EPSILON);
        assert!((defaults.breath_depth - default_breath_depth()).abs() < f32::EPSILON);
        assert!((defaults.bandpass_base_hz - default_bandpass_base_hz()).abs() < f32::EPSILON);
        assert!((defaults.bandpass_range_hz - default_bandpass_range_hz()).abs() < f32::EPSILON);
        assert!((defaults.breath_rate_hz - default_breath_rate_hz()).abs() < f32::EPSILON);
    }

    /// Confirms that persisted TOML missing the new attract fields still
    /// deserializes cleanly with the correct defaults and preserves siblings.
    #[test]
    #[allow(
        clippy::expect_used,
        reason = "test-only: panic on bad TOML is the intended failure mode"
    )]
    fn missing_attract_fields_fall_back_to_defaults() {
        let legacy = r"
            dot_spacing = 32.0
            gamma = 1.5
        ";
        let parsed: DotsSettings = toml::from_str(legacy).expect("legacy TOML must parse");
        // Sibling fields are preserved.
        assert!(
            (parsed.dot_spacing - 32.0).abs() < 1e-6,
            "dot_spacing not preserved"
        );
        assert!((parsed.gamma - 1.5).abs() < 1e-6, "gamma not preserved");
        // New attract fields fall back to their defaults.
        assert!(
            (parsed.attract_particle_fraction - 0.6).abs() < 1e-6,
            "attract_particle_fraction should default to 0.6, got {}",
            parsed.attract_particle_fraction
        );
        assert!(
            (parsed.attract_turbulence - 6.0).abs() < 1e-6,
            "attract_turbulence should default to 6.0, got {}",
            parsed.attract_turbulence
        );
    }
}
