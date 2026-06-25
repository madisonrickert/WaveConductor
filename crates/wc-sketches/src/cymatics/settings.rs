//! Cymatics settings: full Dev surface for physics, visual, interaction,
//! audio, and attract knobs.
//!
//! ## Category split
//!
//! - **User** (visible without ADVANCED): `master_brightness` (visual
//!   brightness trim), `gamma` (visual contrast/display-gamma trim),
//!   `osc_level` + `blub_level` (audio volume trims).
//! - **Dev** (ADVANCED toggle required, resets each launch): all other
//!   knobs — `vertical_resolution` + `iterations` (`requires_restart`),
//!   the physics decay/force constants plus `source_amplitude`, `skew_curve`,
//!   six interaction tuning factors, the screensaver raindrop/colour knobs
//!   (`attract_radius` and the four `ping_*` raindrop knobs), and the four
//!   Lissajous speeds.
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
//!
//! ## Field origins
//!
//! - **`vertical_resolution`** / **`iterations`**: already in the minimal
//!   surface (C8); kept here unchanged. Both restart on change (they allocate
//!   GPU textures and fix the compute dispatch count at spawn time).
//! - **Physics fields** (`force_multiplier`, `velocity_decay`, `height_decay`,
//!   `accumulated_height_decay`, `source_amplitude`): v4 constants from
//!   `simulate.wgsl` (and the v4-hardcoded `2.0` source amplitude); now live
//!   knobs read each frame by `update_cymatics_sim_params`. `source_amplitude`
//!   is applied CPU-side when the per-sub-step `wave_signal` is precomputed.
//! - **`skew_curve`**: exponent applied to the raw `skewIntensity` before
//!   packing into the render material uniform. `1.0` = linear (v4 behaviour).
//! - **`master_brightness`**: post-render brightness multiplier. `1.0` = no-op.
//! - **`gamma`**: per-channel display gamma applied as the final visual
//!   correction (mirrors Line/Dots `gamma`). `1.0` = identity (v4 default).
//! - **Interaction fields** (`min_radius`, `interacting_radius`, `target_radius`,
//!   `grow_factor`, `decay_factor`, `lerp_factor`): v4 module constants from
//!   `index.ts`; now live knobs threaded into `step_centers` via `CenterTuning`.
//! - **`osc_level`** / **`blub_level`**: per-voice output-gain trims applied
//!   in `drive_cymatics_audio` on top of the v4 formulas.
//! - **Attract fields** (`attract_radius`, `c1_omega_x`, `c1_omega_y`,
//!   `c2_omega_x`, `c2_omega_y`): v4 `ATTRACT_ACTIVE_RADIUS` and the four
//!   Lissajous angular speeds in `screensaver.rs`; now live knobs for the
//!   attract wander.
//! - **Raindrop fields** (`ping_interval`, `ping_jitter`, `ping_strength`,
//!   `ping_duration`): the screensaver raindrop model — the four `ping_*` knobs
//!   tune the intermittent staggered drops driven by
//!   `screensaver::drive_cymatics_pings`.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

/// User-tunable and Dev-tunable parameters for the Cymatics sketch.
///
/// Settings are stored as `f32` to match the derive macro's `Number` setting
/// type (as Dots does); call sites convert to `u32` via a clamped
/// `u32::try_from` rather than a bare `as` cast.
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "cymatics")]
pub struct CymaticsSettings {
    // ── Simulation (requires restart) ────────────────────────────────────────
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

    // ── Physics (live, no restart) ────────────────────────────────────────────
    /// Neighbour-force scale baked into each sub-step of the wave integrator
    /// (v4 `FORCE_MULTIPLIER = 0.25`). Higher values strengthen the neighbour
    /// coupling; too high causes numeric explosion. Read live each frame by
    /// `update_cymatics_sim_params`.
    #[setting(
        default = 0.25_f32,
        min = 0.0_f32,
        max = 2.0_f32,
        step = 0.01_f32,
        label = "Force multiplier",
        section = "Physics",
        category = Dev
    )]
    #[serde(default = "default_force_multiplier")]
    pub force_multiplier: f32,

    /// Per-sub-step velocity damping factor (v4 `VELOCITY_DECAY_FACTOR = 0.99818`).
    /// Values closer to `1.0` preserve velocity longer; values closer to `0.9`
    /// damp it quickly. Read live each frame.
    #[setting(
        default = 0.99818_f32,
        min = 0.9_f32,
        max = 1.0_f32,
        step = 0.0001_f32,
        label = "Velocity decay",
        section = "Physics",
        category = Dev
    )]
    #[serde(default = "default_velocity_decay")]
    pub velocity_decay: f32,

    /// Per-sub-step height damping factor (v4 `HEIGHT_DECAY_FACTOR = 0.9999`).
    /// Controls how quickly the raw wave height attenuates between sub-steps.
    /// Read live each frame.
    #[setting(
        default = 0.9999_f32,
        min = 0.9_f32,
        max = 1.0_f32,
        step = 0.0001_f32,
        label = "Height decay",
        section = "Physics",
        category = Dev
    )]
    #[serde(default = "default_height_decay")]
    pub height_decay: f32,

    /// Per-sub-step accumulated-height decay factor (v4
    /// `ACCUMULATED_HEIGHT_DECAY_FACTOR = 0.999`). Controls how quickly the
    /// time-integrated height (channel z of the sim texture) attenuates.
    /// Read live each frame.
    #[setting(
        default = 0.999_f32,
        min = 0.9_f32,
        max = 1.0_f32,
        step = 0.0001_f32,
        label = "Accum. height decay",
        section = "Physics",
        category = Dev
    )]
    #[serde(default = "default_accumulated_height_decay")]
    pub accumulated_height_decay: f32,

    /// Wave-source injection amplitude — the peak of the source oscillator
    /// `amplitude·sin(phase)` blended into the field at each of the two centres
    /// (v4 hardcoded `2.0`). Higher values inject more energy, so the ripples
    /// grow larger and the screensaver's "raindrop" rings read more clearly.
    /// Default `3.0` (slightly above the old `2.0`) for a touch more presence
    /// everywhere, active play included. Read live each frame by
    /// `update_cymatics_sim_params` and applied CPU-side in the compute prepare
    /// step (where the per-sub-step `wave_signal` is precomputed).
    #[setting(
        default = 3.0_f32,
        min = 0.5_f32,
        max = 8.0_f32,
        step = 0.5_f32,
        label = "Wave source amplitude",
        section = "Physics",
        category = Dev
    )]
    #[serde(default = "default_source_amplitude")]
    pub source_amplitude: f32,

    // ── Visual (live, no restart) ─────────────────────────────────────────────
    /// Post-render brightness multiplier applied to the final output colour.
    /// `1.0` is a no-op (v4 default). Values above `1.0` brighten the output;
    /// `0.0` is black. User-visible knob so kiosk operators can trim brightness
    /// without touching system display settings.
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

    /// Per-channel display gamma applied as a final visual correction, mirroring
    /// the Line and Dots `gamma` knob. `1.0` is the identity (v4 default); the
    /// shader skips the `pow` entirely at `1.0`. Values above `1.0` deepen the
    /// mid-tones (more contrast), below `1.0` lift them. Read live each frame via
    /// the render material's `skew.z` lane; no restart required.
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

    /// Exponent applied to raw `skewIntensity` (derived from `num_cycles`)
    /// before packing into the render material's skew uniform. `1.0` = linear,
    /// the v4 default. Values above `1.0` make the body-colour push more
    /// pronounced near peak interaction; values below `1.0` widen the ramp.
    #[setting(
        default = 1.0_f32,
        min = 0.1_f32,
        max = 5.0_f32,
        step = 0.1_f32,
        label = "Skew curve",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_skew_curve")]
    pub skew_curve: f32,

    /// Camera tonemapping operator for this sketch. Default `ReinhardLuminance`
    /// (chroma-preserving "neon glow"). Applied to the main camera while Cymatics
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

    /// Bloom prefilter threshold for this sketch. Default `0.7` — only HDR cores
    /// bloom (crisp midtones + glowing highlights). `0.0` blooms everything.
    #[setting(
        default = 0.7_f32,
        min = 0.0_f32,
        max = 3.0_f32,
        step = 0.05_f32,
        label = "Bloom threshold",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_bloom_threshold")]
    pub bloom_threshold: f32,

    // ── Interaction (live, no restart) ────────────────────────────────────────
    /// Resting alive-mask radius floor (v4 `MINIMUM_ACTIVE_RADIUS = 0.1`). At
    /// rest the wave sources oscillate inside a small mask of this radius.
    #[setting(
        default = 0.1_f32,
        min = 0.0_f32,
        max = 1.0_f32,
        step = 0.01_f32,
        label = "Resting radius",
        section = "Interaction",
        category = Dev
    )]
    #[serde(default = "default_min_radius")]
    pub min_radius: f32,

    /// Alive-mask radius floor while interacting (v4
    /// `MINIMUM_ACTIVE_RADIUS_INTERACTING = 0.5`). The radius snaps to at
    /// least this value the moment a press or grab begins.
    #[setting(
        default = 0.5_f32,
        min = 0.1_f32,
        max = 5.0_f32,
        step = 0.05_f32,
        label = "Min interacting radius",
        section = "Interaction",
        category = Dev
    )]
    #[serde(default = "default_interacting_radius")]
    pub interacting_radius: f32,

    /// Alive-mask radius target while interacting (v4
    /// `TARGET_ACTIVE_RADIUS_INTERACTING = 7.5`). The radius lerps toward
    /// this value each frame that interaction is active.
    #[setting(
        default = 7.5_f32,
        min = 0.5_f32,
        max = 20.0_f32,
        step = 0.5_f32,
        label = "Target interacting radius",
        section = "Interaction",
        category = Dev
    )]
    #[serde(default = "default_target_radius")]
    pub target_radius: f32,

    /// Per-frame lerp factor for radius growth toward `target_radius` while
    /// interacting (v4 `ACTIVE_RADIUS_INTERACTING_GROW_FACTOR = 0.01`).
    /// Higher = faster ramp-up.
    #[setting(
        default = 0.01_f32,
        min = 0.001_f32,
        max = 0.5_f32,
        step = 0.001_f32,
        label = "Radius grow factor",
        section = "Interaction",
        category = Dev
    )]
    #[serde(default = "default_grow_factor")]
    pub grow_factor: f32,

    /// Per-frame lerp factor for radius decay toward `min_radius` when idle
    /// (v4 `ACTIVE_RADIUS_IDLE_DECAY_FACTOR = 0.005`). Higher = faster decay
    /// back to rest.
    #[setting(
        default = 0.005_f32,
        min = 0.001_f32,
        max = 0.5_f32,
        step = 0.001_f32,
        label = "Radius decay factor",
        section = "Interaction",
        category = Dev
    )]
    #[serde(default = "default_decay_factor")]
    pub decay_factor: f32,

    /// Per-frame lerp factor for centre-position tracking (v4
    /// `INTERACTION_CENTER_LERP_FACTOR = 0.01`). Higher = centre snaps faster
    /// to the cursor or hand position.
    #[setting(
        default = 0.01_f32,
        min = 0.001_f32,
        max = 0.5_f32,
        step = 0.001_f32,
        label = "Center lerp factor",
        section = "Interaction",
        category = Dev
    )]
    #[serde(default = "default_lerp_factor")]
    pub lerp_factor: f32,

    // ── Audio (live, no restart) ──────────────────────────────────────────────
    /// Output-gain trim for the oscillator voice (`osc_volume`). Applied as a
    /// multiplier on top of the v4 smoothstep swell formula in
    /// `drive_cymatics_audio`. `1.0` = unchanged. Adjust to balance the osc
    /// voice against the blub loop.
    #[setting(
        default = 1.0_f32,
        min = 0.0_f32,
        max = 2.0_f32,
        step = 0.05_f32,
        label = "Osc volume",
        section = "Audio",
        category = User
    )]
    #[serde(default = "default_osc_level")]
    pub osc_level: f32,

    /// Output-gain trim for the blub loop voice (`blub_volume`). Applied as a
    /// multiplier in `drive_cymatics_audio` after the v4 formula and the
    /// mandatory `×0.05` scale (Rule #3). `1.0` = unchanged.
    #[setting(
        default = 1.0_f32,
        min = 0.0_f32,
        max = 2.0_f32,
        step = 0.05_f32,
        label = "Blub volume",
        section = "Audio",
        category = User
    )]
    #[serde(default = "default_blub_level")]
    pub blub_level: f32,

    // ── Screensaver / attract (live, no restart) ──────────────────────────────
    /// Ambient alive-mask radius held during the raindrop screensaver. Default
    /// `1.0`: the core mask radius is `attract_radius - 0.2` (= `0.8`) and the
    /// outer fade reaches `attract_radius + 0.8`, keeping the pond calm and
    /// fairly dark so each raindrop's expanding ring reads as a concentrated
    /// crest rather than part of a full-screen wash. The `0.1` to `2.0` range
    /// lets the operator widen it live; `0.1` (the resting floor) would produce a
    /// nearly invisible mask.
    #[setting(
        default = 1.0_f32,
        min = 0.1_f32,
        max = 2.0_f32,
        step = 0.05_f32,
        label = "Attract radius",
        section = "Screensaver",
        category = Dev
    )]
    #[serde(default = "default_attract_radius")]
    pub attract_radius: f32,

    /// Seconds between raindrops per attractor (the floor of the jittered
    /// interval). The screensaver drops one Hann-enveloped source pulse per
    /// attractor every `ping_interval` to `ping_interval + ping_jitter` seconds,
    /// staggered so the two attractors never fire in lock-step.
    #[setting(
        default = 15.0_f32,
        min = 5.0_f32,
        max = 120.0_f32,
        step = 5.0_f32,
        label = "Ping interval",
        section = "Screensaver",
        category = Dev
    )]
    #[serde(default = "default_ping_interval")]
    pub ping_interval: f32,

    /// Extra seconds of golden-ratio jitter added on top of `ping_interval`, so
    /// successive drops are irregular and the two attractors desync. `0.0` makes
    /// the cadence perfectly regular.
    #[setting(
        default = 5.0_f32,
        min = 0.0_f32,
        max = 90.0_f32,
        step = 5.0_f32,
        label = "Ping jitter",
        section = "Screensaver",
        category = Dev
    )]
    #[serde(default = "default_ping_jitter")]
    pub ping_jitter: f32,

    /// Raindrop strength — the peak displacement of each drop's Hann pulse.
    /// Higher values push the ring crests further into HDR so they bloom more
    /// vividly through `AgX`. Drop strength / vividness.
    #[setting(
        default = 4.0_f32,
        min = 0.5_f32,
        max = 10.0_f32,
        step = 0.5_f32,
        label = "Ping strength",
        section = "Screensaver",
        category = Dev
    )]
    #[serde(default = "default_ping_strength")]
    pub ping_strength: f32,

    /// Raindrop splash length in sim sub-steps — the width of each drop's Hann
    /// window `D`. Locked to sub-steps (not seconds), so the ring expansion is
    /// fps-independent. Longer = a slower, broader splash.
    #[setting(
        default = 30.0_f32,
        min = 5.0_f32,
        max = 120.0_f32,
        step = 1.0_f32,
        label = "Ping duration",
        section = "Screensaver",
        category = Dev
    )]
    #[serde(default = "default_ping_duration")]
    pub ping_duration: f32,

    /// Lissajous angular speed for centre-1's X component (rad/s). Default
    /// `0.1505` = 3.5× v4's `0.043`: the centres wander (and their ripples
    /// interfere) noticeably within a short watch instead of over ~145 s.
    /// Together with `c1_omega_y` it traces a slow incommensurate path across
    /// the sim UV field; all four omegas are scaled by the same 3.5× factor, so
    /// the v4 incommensurate ratios are preserved.
    #[setting(
        default = 0.1505_f32,
        min = 0.001_f32,
        max = 0.5_f32,
        step = 0.001_f32,
        label = "C1 omega x",
        section = "Screensaver",
        category = Dev
    )]
    #[serde(default = "default_c1_omega_x")]
    pub c1_omega_x: f32,

    /// Lissajous angular speed for centre-1's Y component (rad/s). Default
    /// `0.1085` = 3.5× v4's `0.031`. The 43:31 ratio with `c1_omega_x` is
    /// preserved by the common 3.5× scale, keeping the centre-1 path
    /// incommensurate with centre-2.
    #[setting(
        default = 0.1085_f32,
        min = 0.001_f32,
        max = 0.5_f32,
        step = 0.001_f32,
        label = "C1 omega y",
        section = "Screensaver",
        category = Dev
    )]
    #[serde(default = "default_c1_omega_y")]
    pub c1_omega_y: f32,

    /// Lissajous angular speed for centre-2's X component (rad/s). Default
    /// `0.1295` = 3.5× v4's `0.037`. Phase-offset by `+1.7 rad` at t=0 so both
    /// centres are spatially separated when the screensaver starts.
    #[setting(
        default = 0.1295_f32,
        min = 0.001_f32,
        max = 0.5_f32,
        step = 0.001_f32,
        label = "C2 omega x",
        section = "Screensaver",
        category = Dev
    )]
    #[serde(default = "default_c2_omega_x")]
    pub c2_omega_x: f32,

    /// Lissajous angular speed for centre-2's Y component (rad/s). Default
    /// `0.1015` = 3.5× v4's `0.029`. Phase-offset by `+0.6 rad` at t=0.
    #[setting(
        default = 0.1015_f32,
        min = 0.001_f32,
        max = 0.5_f32,
        step = 0.001_f32,
        label = "C2 omega y",
        section = "Screensaver",
        category = Dev
    )]
    #[serde(default = "default_c2_omega_y")]
    pub c2_omega_y: f32,
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

// Physics defaults (v4 constants).
fn default_force_multiplier() -> f32 {
    0.25
}

fn default_velocity_decay() -> f32 {
    0.99818
}

fn default_height_decay() -> f32 {
    0.9999
}

fn default_accumulated_height_decay() -> f32 {
    0.999
}

// Wave-source injection amplitude. v4 hardcoded 2.0; 3.0 gives a touch more
// presence everywhere (see the `source_amplitude` field doc).
fn default_source_amplitude() -> f32 {
    3.0
}

// Visual defaults.
fn default_master_brightness() -> f32 {
    1.0
}

fn default_gamma() -> f32 {
    1.0
}

fn default_skew_curve() -> f32 {
    1.0
}

fn default_tonemapping() -> wc_core::render::TonemapChoice {
    wc_core::render::TonemapChoice::ReinhardLuminance
}

fn default_bloom_intensity() -> f32 {
    0.35
}

fn default_bloom_threshold() -> f32 {
    0.7
}

// Interaction defaults (v4 constants).
fn default_min_radius() -> f32 {
    0.1
}

fn default_interacting_radius() -> f32 {
    0.5
}

fn default_target_radius() -> f32 {
    7.5
}

fn default_grow_factor() -> f32 {
    0.01
}

fn default_decay_factor() -> f32 {
    0.005
}

fn default_lerp_factor() -> f32 {
    0.01
}

// Audio defaults.
fn default_osc_level() -> f32 {
    1.0
}

fn default_blub_level() -> f32 {
    1.0
}

// Attract / screensaver defaults. `attract_radius` = 1.0 keeps the raindrop
// pond broad enough to avoid a gray background wash; the raindrop knobs
// (`ping_*`) drive the visible motion. The four Lissajous speeds are 3.5× the
// v4 values so the two centres visibly wander within a short watch; scaling all
// four by the same factor preserves the v4 incommensurate ratios (43:31, 37:29,
// and the cross ratios), only shortening the periods from ~145–217 s to ~42–62 s.
fn default_attract_radius() -> f32 {
    1.0
}

// Raindrop ping defaults: drops every 15–20 s per attractor (15 floor +
// 0.0–5 golden-ratio jitter), each a strength-4.0 Hann pulse 30 sub-steps wide.
fn default_ping_interval() -> f32 {
    15.0
}

fn default_ping_jitter() -> f32 {
    5.0
}

fn default_ping_strength() -> f32 {
    4.0
}

fn default_ping_duration() -> f32 {
    30.0
}

fn default_c1_omega_x() -> f32 {
    0.1505
}

fn default_c1_omega_y() -> f32 {
    0.1085
}

fn default_c2_omega_x() -> f32 {
    0.1295
}

fn default_c2_omega_y() -> f32 {
    0.1015
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The derived `Default` must match every per-field serde default so an
    /// in-memory default and a missing-field deserialize agree.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one logical assertion (every setting's default matches its serde \
                  default), spanning the full field surface; splitting it would only \
                  fragment the exhaustive check"
    )]
    fn default_values_match_serde_defaults() {
        let d = CymaticsSettings::default();

        // Simulation (restart)
        assert!((d.vertical_resolution - default_vertical_resolution()).abs() < f32::EPSILON);
        assert!((d.iterations - default_iterations()).abs() < f32::EPSILON);

        // Physics
        assert!(
            (d.force_multiplier - default_force_multiplier()).abs() < f32::EPSILON,
            "force_multiplier default mismatch"
        );
        assert!(
            (d.velocity_decay - default_velocity_decay()).abs() < f32::EPSILON,
            "velocity_decay default mismatch"
        );
        assert!(
            (d.height_decay - default_height_decay()).abs() < f32::EPSILON,
            "height_decay default mismatch"
        );
        assert!(
            (d.accumulated_height_decay - default_accumulated_height_decay()).abs() < f32::EPSILON,
            "accumulated_height_decay default mismatch"
        );
        assert!(
            (d.source_amplitude - default_source_amplitude()).abs() < f32::EPSILON,
            "source_amplitude default mismatch"
        );

        // Visual
        assert!(
            (d.master_brightness - default_master_brightness()).abs() < f32::EPSILON,
            "master_brightness default mismatch"
        );
        assert!(
            (d.gamma - default_gamma()).abs() < f32::EPSILON,
            "gamma default mismatch"
        );
        assert!(
            (d.skew_curve - default_skew_curve()).abs() < f32::EPSILON,
            "skew_curve default mismatch"
        );
        assert_eq!(
            d.tonemapping,
            default_tonemapping(),
            "tonemapping default mismatch"
        );
        assert!(
            (d.bloom_intensity - default_bloom_intensity()).abs() < f32::EPSILON,
            "bloom_intensity"
        );
        assert!(
            (d.bloom_threshold - default_bloom_threshold()).abs() < f32::EPSILON,
            "bloom_threshold"
        );

        // Interaction
        assert!(
            (d.min_radius - default_min_radius()).abs() < f32::EPSILON,
            "min_radius"
        );
        assert!(
            (d.interacting_radius - default_interacting_radius()).abs() < f32::EPSILON,
            "interacting_radius"
        );
        assert!(
            (d.target_radius - default_target_radius()).abs() < f32::EPSILON,
            "target_radius"
        );
        assert!(
            (d.grow_factor - default_grow_factor()).abs() < f32::EPSILON,
            "grow_factor"
        );
        assert!(
            (d.decay_factor - default_decay_factor()).abs() < f32::EPSILON,
            "decay_factor"
        );
        assert!(
            (d.lerp_factor - default_lerp_factor()).abs() < f32::EPSILON,
            "lerp_factor"
        );

        // Audio
        assert!(
            (d.osc_level - default_osc_level()).abs() < f32::EPSILON,
            "osc_level"
        );
        assert!(
            (d.blub_level - default_blub_level()).abs() < f32::EPSILON,
            "blub_level"
        );

        // Screensaver
        assert!(
            (d.attract_radius - default_attract_radius()).abs() < f32::EPSILON,
            "attract_radius"
        );
        assert!(
            (d.ping_interval - default_ping_interval()).abs() < f32::EPSILON,
            "ping_interval"
        );
        assert!(
            (d.ping_jitter - default_ping_jitter()).abs() < f32::EPSILON,
            "ping_jitter"
        );
        assert!(
            (d.ping_strength - default_ping_strength()).abs() < f32::EPSILON,
            "ping_strength"
        );
        assert!(
            (d.ping_duration - default_ping_duration()).abs() < f32::EPSILON,
            "ping_duration"
        );
        assert!(
            (d.c1_omega_x - default_c1_omega_x()).abs() < f32::EPSILON,
            "c1_omega_x"
        );
        assert!(
            (d.c1_omega_y - default_c1_omega_y()).abs() < f32::EPSILON,
            "c1_omega_y"
        );
        assert!(
            (d.c2_omega_x - default_c2_omega_x()).abs() < f32::EPSILON,
            "c2_omega_x"
        );
        assert!(
            (d.c2_omega_y - default_c2_omega_y()).abs() < f32::EPSILON,
            "c2_omega_y"
        );
    }

    /// Legacy persisted TOML missing one field still deserializes the other
    /// fields cleanly via the per-field `#[serde(default)]`.
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
        assert!(
            (parsed.force_multiplier - 0.25).abs() < 1e-6,
            "force_multiplier should fall back to default"
        );
        assert!(
            (parsed.master_brightness - 1.0).abs() < 1e-6,
            "master_brightness should fall back to default"
        );
        assert!(
            (parsed.gamma - 1.0).abs() < 1e-6,
            "gamma should fall back to default"
        );
        assert!(
            (parsed.attract_radius - 1.0).abs() < 1e-6,
            "attract_radius should fall back to default"
        );
        assert!(
            (parsed.ping_interval - 15.0).abs() < 1e-6,
            "ping_interval should fall back to default"
        );
        assert!(
            (parsed.ping_jitter - 5.0).abs() < 1e-6,
            "ping_jitter should fall back to default"
        );
    }
}
