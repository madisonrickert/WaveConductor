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
}

// Per-field serde defaults. Values MUST match the `#[setting(default = ...)]`
// attributes above so a missing-field deserialize lands on the same value the
// derive-macro `Default` impl would produce. Update both sites together.
fn default_dot_spacing() -> f32 {
    20.0
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
        assert!((defaults.gamma - default_gamma()).abs() < f32::EPSILON);
        assert!(
            (defaults.attract_particle_fraction - default_attract_particle_fraction()).abs()
                < f32::EPSILON
        );
        assert!((defaults.attract_turbulence - default_attract_turbulence()).abs() < f32::EPSILON);
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
