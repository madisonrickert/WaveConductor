//! Cymatics settings.
//!
//! This minimal surface (sim grid resolution + sub-step count, both
//! `requires_restart`) is expanded to the full Dev surface in a later stage.
//! Both knobs reallocate GPU resources (textures) or change the per-frame
//! dispatch shape at spawn time, so both restart the sketch on change.
//!
//! ## Serde forward-compatibility
//!
//! Each field carries `#[serde(default = "default_<name>")]` so a legacy
//! persisted TOML written before a new field was added still deserializes
//! cleanly: the missing field falls back to its default and the sibling fields
//! are preserved. (See [`crate::dots::settings`] for the full rationale.)
//!
//! The `SketchSettings` derive generates the [`Default`] impl from the
//! `#[setting(default = ...)]` attributes, so there is intentionally no manual
//! `impl Default` here — adding one would conflict with the derived impl.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

/// User-tunable parameters for the Cymatics sketch.
///
/// Settings are stored as `f32` to match the derive macro's `Number` setting
/// type (as Dots does); call sites convert to `u32` via a clamped
/// `u32::try_from` rather than a bare `as` cast.
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "cymatics")]
pub struct CymaticsSettings {
    /// Sim grid vertical resolution in texels. Restart on change (the ping-pong
    /// textures reallocate at spawn time). The horizontal resolution is derived
    /// from this and the window aspect.
    #[setting(
        default = 480.0_f32,
        min = 64.0_f32,
        max = 1080.0_f32,
        step = 1.0_f32,
        label = "Vertical resolution",
        section = "Simulation",
        category = Dev,
        requires_restart
    )]
    #[serde(default = "default_vertical_resolution")]
    pub vertical_resolution: f32,

    /// Sim sub-steps per frame (v4 `numIterations = 20`). Restart on change
    /// (the per-frame dispatch count is fixed at spawn time). Clamped to the
    /// compute pipeline's `MAX_ITERATIONS` slot count at use sites.
    #[setting(
        default = 20.0_f32,
        min = 1.0_f32,
        max = 120.0_f32,
        step = 1.0_f32,
        label = "Iterations per frame",
        section = "Simulation",
        category = Dev,
        requires_restart
    )]
    #[serde(default = "default_iterations")]
    pub iterations: f32,
}

// Per-field serde defaults. Values MUST match the `#[setting(default = ...)]`
// attributes above so a missing-field deserialize lands on the same value the
// derived `Default` impl produces. Update both sites together.
fn default_vertical_resolution() -> f32 {
    480.0
}

fn default_iterations() -> f32 {
    20.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The derived `Default` must match the per-field serde defaults so the
    /// in-memory default and a missing-field deserialize agree.
    #[test]
    fn default_values_match_serde_defaults() {
        let defaults = CymaticsSettings::default();
        assert!(
            (defaults.vertical_resolution - default_vertical_resolution()).abs() < f32::EPSILON,
            "vertical_resolution default mismatch"
        );
        assert!(
            (defaults.iterations - default_iterations()).abs() < f32::EPSILON,
            "iterations default mismatch"
        );
    }

    /// Legacy persisted TOML missing one field still deserializes the other
    /// field cleanly via the per-field `#[serde(default)]`.
    #[test]
    #[allow(
        clippy::expect_used,
        reason = "test-only: panic on bad TOML is the intended failure mode"
    )]
    fn missing_field_preserves_sibling_values() {
        let legacy = r"
            vertical_resolution = 240.0
        ";
        let parsed: CymaticsSettings = toml::from_str(legacy).expect("legacy TOML must parse");
        assert!(
            (parsed.vertical_resolution - 240.0).abs() < 1e-6,
            "vertical_resolution not preserved"
        );
        assert!(
            (parsed.iterations - 20.0).abs() < 1e-6,
            "iterations should fall back to default"
        );
    }
}
