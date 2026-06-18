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
//! - **`attract_particle_fraction`** — fraction of particles kept alive during
//!   attract mode; the rest fade out and stay dead until wake. Dev-only knob.
//! - **`attract_color_strength`** — peak strength of the attract-mode
//!   velocity tint (meteor wakes pull toward a cool colour). Dev-only knob.
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
//! - **`synth_full_volume_mm`** / **`synth_silence_mm`** — the physical
//!   distance band of that attenuation: a hand at or nearer than
//!   `synth_full_volume_mm` plays at full drive, fading to silence at
//!   `synth_silence_mm`. Kiosk-tuning knobs (a kiosk visitor stands ~0.5 m
//!   out and should fade over several feet, not by 1 m). Dev-only knobs.
//!
//! ## Synth timing chain (for tuners)
//!
//! Three smoothing stages sit between a gesture and the speaker; each timing
//! is owned by exactly one place:
//!
//! 1. **Hand drive** ([`crate::line::leap_attractors::HandAudioDrive`]) —
//!    instant on rise; on fall, exponential with
//!    `τ = max(synth_release_ms / 1000, 0.67 s)`
//!    ([`crate::line::leap_attractors::hand_drive_release_tau_s`]). Not
//!    separately tunable: it follows `synth_release_ms` so it can never clip
//!    stage 2's tail.
//! 2. **Upness envelope** (`ParticleStats::grouped_upness`) — the musical
//!    attack/release pair: `synth_attack_ms` owns how fast a press speaks,
//!    `synth_release_ms` owns the tail length.
//! 3. **Synth follow** — a fixed 16 ms `follow(0.016)` inside the
//!    `LineSynth` DSP graph; anti-zipper smoothing only, never tune timing
//!    there.
//!
//! To change press snappiness, adjust `synth_attack_ms`; to change tail
//! length, `synth_release_ms` (the drive's τ tracks it automatically). The
//! drive's gamma/falloff knobs shape *loudness*, not timing.

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
    /// `spawn_line` re-runs with the new sampler. Rendered as the image
    /// template library picker (a plain Browse… file picker when the
    /// `templates` feature is off).
    #[setting(
        default = String::new(),
        ty = TemplateLibrary,
        filter_label = "Image",
        extensions = ["png", "jpg", "jpeg", "webp"],
        section = "Spawn",
        category = User,
        requires_restart
    )]
    #[serde(default)]
    pub spawn_template: String,

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
        category = Dev
    )]
    #[serde(default = "default_attract_particle_fraction")]
    pub attract_particle_fraction: f32,

    /// Peak strength of the attract-mode velocity tint (fast particles —
    /// meteor wakes — pull toward a desaturated cool colour). Scaled by the
    /// screensaver fade envelope, so it ramps in/out with attract mode and is
    /// exactly 0 during live interaction. `0.0` disables the tint entirely;
    /// keep it subtle — the calm field's warm-white personality must hold.
    /// Dev-only knob.
    #[setting(
        default = 0.35_f32,
        min = 0.0_f32,
        max = 1.0_f32,
        step = 0.05_f32,
        label = "Attract color strength",
        category = Dev
    )]
    #[serde(default = "default_attract_color_strength")]
    pub attract_color_strength: f32,

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
    /// drive — applied to whichever proximity band is active: the kiosk
    /// distance band (`synth_full_volume_mm`..`synth_silence_mm`, when the
    /// provider estimates a physical distance) or the legacy Leap-z band
    /// (40 mm loudest .. 350 mm silent, otherwise). `1.0` = linear fade
    /// across the band; `> 1.0` makes loudness drop off faster as the hand
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

    /// Near rail of the hand→volume distance band, in **physical camera
    /// millimetres** ([`wc_core::input::hand::Hand::camera_distance_mm`]): a
    /// hand at or nearer than this plays at full drive. Default 500 mm — a
    /// kiosk visitor's natural standing distance is full volume, not a
    /// special "lean in" reward. Only applies when the provider estimates a
    /// physical distance (`MediaPipe` with the depth estimator on); otherwise
    /// the drive falls back to the legacy Leap-z band. Dev-only knob.
    #[setting(
        default = 500.0_f32,
        min = 100.0_f32,
        max = 1500.0_f32,
        step = 10.0_f32,
        label = "Audio full-volume distance",
        unit = "mm",
        category = Dev
    )]
    #[serde(default = "default_synth_full_volume_mm")]
    pub synth_full_volume_mm: f32,

    /// Far rail of the hand→volume distance band (physical camera mm): the
    /// drive reaches silence here. Default 2400 mm (~8 ft, the middle of the
    /// kiosk's 5–10 ft falloff target); values at or below the near rail are
    /// guarded against in the drive math. Note the *visual* attractor power
    /// still fades by ~1 m (the Leap-z far rail), so a far grab past that
    /// moves particles weakly while the sound carries further out. Dev-only
    /// knob.
    #[setting(
        default = 2400.0_f32,
        min = 600.0_f32,
        max = 4000.0_f32,
        step = 50.0_f32,
        label = "Audio silence distance",
        unit = "mm",
        category = Dev
    )]
    #[serde(default = "default_synth_silence_mm")]
    pub synth_silence_mm: f32,
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

fn default_attract_particle_fraction() -> f32 {
    0.6
}

fn default_attract_color_strength() -> f32 {
    0.35
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

fn default_synth_full_volume_mm() -> f32 {
    500.0
}

fn default_synth_silence_mm() -> f32 {
    2400.0
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
        assert!(
            (parsed.attract_particle_fraction - 0.6).abs() < 1e-6,
            "attract_particle_fraction not default"
        );
        assert!(
            (parsed.attract_color_strength - 0.35).abs() < 1e-6,
            "attract_color_strength not default"
        );
    }
}
