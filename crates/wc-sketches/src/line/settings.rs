//! Line sketch settings.
//!
//! Curated knobs that show up in the user panel. v4 exposes two: particle
//! density and the gravity constant. Plan 7 mirrors that. Drag and attractor
//! radius existed as v5-only knobs during Plan 6 (the inverse-linear gravity
//! era); Plan 7 baked drag into [`crate::line::particle::SimParams`] from
//! fixed v4 constants and made the force constant-magnitude (no radius
//! needed), so both fields are dropped.
//!
//! ## Serde forward-compatibility
//!
//! Existing user TOML written by earlier v5 builds with `drag` /
//! `attractor_radius` keys still deserializes cleanly: serde's default is to
//! ignore unknown fields, and we intentionally do **not** set
//! `#[serde(deny_unknown_fields)]`. A future maintainer adding that attribute
//! would break upgrades from v5-line; leave it off so dropped knobs don't
//! invalidate persisted user settings.
//!
//! ### Missing-field forward-compat
//!
//! Each field carries `#[serde(default = "default_<name>")]` so a legacy
//! persisted TOML written before a new field was added still deserializes
//! cleanly: the missing field falls back to its default, and the sibling
//! fields are preserved. Without per-field defaults, missing one key
//! would fail the whole-section deserialize and silently revert *every*
//! sibling to default (the bug surfaced when Plan 8 added `gamma`).
//!
//! Apply the same pattern to every settings struct: when adding a field
//! mid-cycle, also add a `default_<name>()` free function and the
//! `#[serde(default = "...")]` attribute.
//!
//! - **`particle_density`** — particles per canvas-pixel of width. v4 uses 10
//!   (so a 1280px window has ~12,800 particles). Restart on change (the
//!   compute pipeline rebuilds its storage buffer).
//! - **`gravity_constant`** — strength of the pull toward attractors (v4
//!   `GRAVITY_CONSTANT`, default 280).
//! - **`gamma`** — per-channel gamma curve on the post-process pass.
//! - **`spawn_template`** — optional PNG path whose luminance × alpha weights
//!   the particle spawn density (empty = horizontal-line layout). Shown as a
//!   Browse… file picker in the user panel (Plan 11 Phase C).
//! - **`synth_volume_scale`** — master output gain trim for the synth voice.
//!   1.0 = unchanged. Lower values reduce kiosk loudness without touching
//!   system volume.
//! - **`synth_attack_ms`** — voice envelope attack time. Smaller = snappier
//!   press response; larger = slower swell-in.
//! - **`synth_release_ms`** — voice envelope release tail length. Smaller =
//!   abrupt cutoff on release; larger = long pad tail.
//! - **`synth_evolution_attack_s`** — how slowly the pad texture blooms over
//!   a sustained press. Dev-only knob.
//! - **`synth_evolution_release_s`** — how slowly the pad texture fades after
//!   release. Dev-only knob.
//! - **`synth_grab_gamma`** — exponent on grab strength in the hand→volume
//!   drive ([`crate::line::leap_attractors::HandAudioDrive`]). Dev-only knob.
//! - **`synth_distance_falloff`** — exponent on the hand-depth attenuation in
//!   the hand→volume drive. Dev-only knob.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

/// User-tunable parameters for the Line sketch.
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "line")]
pub struct LineSettings {
    /// Particles per canvas-pixel of width. Restart on change.
    #[setting(
        default = 10.0_f32,
        min = 0.1_f32,
        max = 30.0_f32,
        step = 0.5_f32,
        section = "Particles",
        category = User,
        requires_restart
    )]
    #[serde(default = "default_particle_density")]
    pub particle_density: f32,

    /// Strength of the pull toward the pointer attractor. v4 default = 280.
    #[setting(
        default = 280.0_f32,
        min = 0.0_f32,
        max = 1000.0_f32,
        step = 10.0_f32,
        section = "Particles",
        category = User
    )]
    #[serde(default = "default_gravity_constant")]
    pub gravity_constant: f32,

    /// Per-channel gamma curve applied as the final step of the gravity-smear
    /// post-process. v4 default = 1.0.
    #[setting(
        default = 1.0_f32,
        min = 0.1_f32,
        max = 4.0_f32,
        step = 0.1_f32,
        section = "Visual",
        category = User
    )]
    #[serde(default = "default_gamma")]
    pub gamma: f32,

    /// Path to a PNG file whose luminance × alpha drives particle spawn density.
    /// Empty string = use the default horizontal-line layout. Relative paths
    /// resolve against the process current directory; absolute paths are
    /// honored as-is. v4 default = "" (no template). Restart on change so
    /// `spawn_line` re-runs with the new sampler. Rendered as a Browse… file
    /// picker (Plan 11 Phase C).
    #[setting(
        default = String::new(),
        ty = FilePath,
        filter_label = "Image",
        extensions = ["png", "jpg", "jpeg", "webp"],
        section = "Spawn",
        category = User,
        requires_restart
    )]
    #[serde(default)]
    pub spawn_template: String,

    /// Master output gain trim for the synth voice. `1.0` = unchanged.
    /// Applied as a final multiplier on the `volume` audio param so kiosk
    /// loudness can be balanced without touching system volume.
    #[setting(
        default = 1.0_f32,
        min = 0.0_f32,
        max = 2.0_f32,
        step = 0.05_f32,
        section = "Audio",
        category = User
    )]
    #[serde(default = "default_synth_volume_scale")]
    pub synth_volume_scale: f32,

    /// Voice envelope attack time in milliseconds. Smaller = snappier press
    /// onset; larger = slower swell-in. Internally converts to an envelope
    /// lerp rate of `1000 / attack_ms`.
    #[setting(
        default = 115.0_f32,
        min = 5.0_f32,
        max = 200.0_f32,
        step = 5.0_f32,
        section = "Audio",
        category = User
    )]
    #[serde(default = "default_synth_attack_ms")]
    pub synth_attack_ms: f32,

    /// Voice envelope release tail length in milliseconds. Smaller = abrupt
    /// cutoff; larger = long pad tail. Internally converts to an envelope
    /// lerp rate of `1000 / release_ms`.
    #[setting(
        default = 350.0_f32,
        min = 100.0_f32,
        max = 3000.0_f32,
        step = 50.0_f32,
        section = "Audio",
        category = User
    )]
    #[serde(default = "default_synth_release_ms")]
    pub synth_release_ms: f32,

    /// Pad evolution attack time (seconds). The texture / filter envelope
    /// blooms over this period during a sustained press. Larger values =
    /// more dramatic "patch develops" character. Dev-only knob.
    #[setting(default = 4.0_f32, min = 0.5_f32, max = 10.0_f32, step = 0.5_f32, category = Dev)]
    #[serde(default = "default_synth_evolution_attack_s")]
    pub synth_evolution_attack_s: f32,

    /// Pad evolution release time (seconds). The texture / filter envelope
    /// fades over this period after release. Should generally be longer than
    /// [`Self::synth_release_ms`] so the voice goes silent while the
    /// modulators are still alive. Dev-only knob.
    #[setting(default = 6.0_f32, min = 1.0_f32, max = 15.0_f32, step = 0.5_f32, category = Dev)]
    #[serde(default = "default_synth_evolution_release_s")]
    pub synth_evolution_release_s: f32,

    /// Exponent on grab strength in the hand→volume drive
    /// ([`crate::line::leap_attractors::HandAudioDrive`]). `1.0` = linear
    /// (half-closed fist ≈ half drive); `> 1.0` demands a more deliberate
    /// fist before the synth opens up; `< 1.0` makes light grabs louder.
    /// Dev-only knob.
    #[setting(
        default = 1.0_f32,
        min = 0.2_f32,
        max = 4.0_f32,
        step = 0.1_f32,
        label = "Grab→volume curve",
        category = Dev
    )]
    #[serde(default = "default_synth_grab_gamma")]
    pub synth_grab_gamma: f32,

    /// Exponent on the normalised hand-depth attenuation in the hand→volume
    /// drive. `1.0` = linear fade from nearest (40 mm, loudest) to farthest
    /// (350 mm, silent); `> 1.0` makes loudness drop off faster as the hand
    /// retreats from the sensor. Dev-only knob.
    #[setting(
        default = 1.0_f32,
        min = 0.2_f32,
        max = 4.0_f32,
        step = 0.1_f32,
        label = "Distance falloff",
        category = Dev
    )]
    #[serde(default = "default_synth_distance_falloff")]
    pub synth_distance_falloff: f32,
}

// Per-field serde defaults. Values MUST match the `#[setting(default = ...)]`
// attributes above so a missing-field deserialize lands on the same value the
// derive-macro `Default` impl would produce. Update both sites together.
fn default_particle_density() -> f32 {
    10.0
}

fn default_gravity_constant() -> f32 {
    280.0
}

fn default_gamma() -> f32 {
    1.0
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

fn default_synth_evolution_attack_s() -> f32 {
    4.0
}

fn default_synth_evolution_release_s() -> f32 {
    6.0
}

fn default_synth_grab_gamma() -> f32 {
    1.0
}

fn default_synth_distance_falloff() -> f32 {
    1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Confirms that legacy persisted TOML missing one field still
    /// deserializes the other fields cleanly. Without per-field
    /// `#[serde(default)]`, missing-field would fail the whole section
    /// and revert every sibling to default — Plan 8's `gamma` addition
    /// would have done exactly that to existing user files.
    #[test]
    #[allow(
        clippy::expect_used,
        reason = "test-only: panic on bad TOML is the intended failure mode"
    )]
    fn missing_field_preserves_sibling_values() {
        let legacy = r#"
            particle_density = 7.5
            gravity_constant = 320.0
            spawn_template = ""
        "#;
        let parsed: LineSettings = toml::from_str(legacy).expect("legacy TOML must parse");
        assert!(
            (parsed.particle_density - 7.5).abs() < 1e-6,
            "particle_density not preserved"
        );
        assert!(
            (parsed.gravity_constant - 320.0).abs() < 1e-6,
            "gravity_constant not preserved"
        );
        assert!((parsed.gamma - 1.0).abs() < 1e-6, "gamma not default");
    }
}
