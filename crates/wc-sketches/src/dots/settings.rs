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
//!   correction step. v4 default = 1.0 (identity). Restart on change.

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
        category = Dev,
        requires_restart
    )]
    #[serde(default = "default_dot_spacing")]
    pub dot_spacing: f32,

    /// Per-channel gamma curve applied as a final visual correction.
    /// v4 default = 1.0 (identity). Restart on change.
    #[setting(
        default = 1.0_f32,
        min = 0.1_f32,
        max = 4.0_f32,
        step = 0.1_f32,
        label = "Gamma",
        category = Dev,
        requires_restart
    )]
    #[serde(default = "default_gamma")]
    pub gamma: f32,
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
    }
}
