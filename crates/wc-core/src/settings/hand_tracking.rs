//! Global hand-tracking settings, persisted across sessions.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

/// Which hand-tracking backend the app should run — the DAW-style "audio
/// driver" selector. Persisted; applied live by the binary's
/// `apply_provider_choice` system, which rebuilds the provider registry on
/// change (no restart).
///
/// Variant identifiers double as the persisted strings *and* the dropdown
/// labels (see `wc_core_macros`: serde serializes unit variants as their
/// name, and the panel has no per-variant label mapping), so they are
/// chosen to read well in a dropdown.
///
/// This setting is the *only* selector for the real backends. The
/// `WAVECONDUCTOR_HAND_PROVIDER` env var survives solely for the
/// `mock` / `synthetic` test fixtures (capture harness / headless runs) —
/// developer scaffolding, not user choices, so they do not appear here;
/// a fixture, when installed, is pinned for the session.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HandProviderChoice {
    /// Probe for the best available backend: Leap first, then the webcam
    /// `MediaPipe` provider (when compiled in and its camera opens), else a
    /// silent mock so the app runs cleanly with no hardware.
    #[default]
    Auto,
    /// Ultraleap controller only; no silent fallback when it fails.
    Leap,
    /// Webcam `MediaPipe` only; no silent fallback when it fails.
    MediaPipe,
    /// No hand tracking; mouse and touch input only.
    Off,
}

/// Hand-tracking-wide settings (not per-sketch).
///
/// `provider` selects the tracking backend (see [`HandProviderChoice`]).
///
/// `leap_background`: should the Leap provider request the
/// `BackgroundFrames` policy at start? When `true`, tracking frames keep
/// arriving even when the `WaveConductor` window is not focused. Default
/// `false` per v4.
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "hand_tracking")]
pub struct HandTrackingSettings {
    /// Which tracking backend to run (`Auto` probes Leap → `MediaPipe`
    /// webcam → silent mock). Applies live: switching tears down the old
    /// provider (joining its worker / releasing the camera or device) and
    /// starts the new one, no restart. Ignored only when a
    /// `WAVECONDUCTOR_HAND_PROVIDER` mock/synthetic test fixture is
    /// installed (capture harness).
    #[setting(
        default = HandProviderChoice::Auto,
        ty = Enum,
        category = User,
        section = "Hand Tracking",
        label = "Tracking provider"
    )]
    #[serde(default)]
    pub provider: HandProviderChoice,

    /// Whether the Leap provider should request the `BackgroundFrames` policy
    /// at start. When `true`, tracking frames keep arriving even when the
    /// `WaveConductor` window is not focused. Default `false` per v4.
    #[setting(
        default = false,
        ty = Boolean,
        category = User,
        section = "Hand Tracking",
        label = "Receive Leap frames when window is not focused"
    )]
    #[serde(default)]
    pub leap_background: bool,

    /// MediaPipe-only: grab rest-deadzone — a relaxed-open hand whose raw grab is
    /// at/under this reads exactly `0`, so the attractor releases. Raise if the
    /// attractor lingers when the hand is open; lower if grab feels weak/late.
    /// The dev panel's "Grab raw (‰)" metric shows the pre-deadzone value, so
    /// the true rest floor can be read directly. Default must match
    /// `mediapipe::pipeline::PipelineConfig` (`0.05`). The previous `0.2` was
    /// calibrated against the image-space grab (pre-world-landmarks); on world
    /// landmarks a relaxed hand's raw grab is already near `0`, so `0.2`
    /// mostly blunted mid-curl response instead of trimming a rest floor.
    #[setting(
        default = 0.05_f32,
        min = 0.0,
        max = 0.6,
        step = 0.01,
        category = Dev,
        section = "Hand Tracking",
        label = "Grab rest deadzone"
    )]
    #[serde(default = "default_grab_rest_deadzone")]
    pub grab_rest_deadzone: f32,

    /// MediaPipe-only: depth calibration gain `k` for the size-estimated hand
    /// depth — the camera focal length in square-side units (≈ `0.82` for a
    /// typical 63° HFOV webcam). `0` disables the estimator entirely (fixed
    /// 120 mm depth, grab-only attractor control — the instant rollback during
    /// a live set; the diagnostic reads `0`). Calibrate against a tape
    /// measure: stand at 0.5 m with an open hand and tune until the dev
    /// panel's "Est. distance (mm)" reads ≈ 500. That diagnostic is the
    /// **physical** distance estimate, not the Leap z the attractor sees
    /// (which is remapped and clamped to `[40, 350]` mm). Default must match
    /// `mediapipe::coords::DEFAULT_DEPTH_CALIBRATION_K` (`0.8`).
    #[setting(
        default = 0.8_f32,
        min = 0.0,
        max = 1.5,
        step = 0.01,
        category = Dev,
        section = "Hand Tracking",
        label = "Depth calibration k (0 = off)"
    )]
    #[serde(default = "default_depth_calibration_k")]
    pub depth_calibration_k: f32,

    /// MediaPipe-only: One-Euro min cutoff (Hz) — at-rest smoothing. Lower =
    /// steadier when the hand holds still (more lag on slow motion). Default must
    /// match `mediapipe::smoothing::DEFAULT_MIN_CUTOFF` (`10.0`).
    #[setting(
        default = 10.0_f32,
        min = 0.1,
        max = 20.0,
        step = 0.05,
        category = Dev,
        section = "Hand Tracking",
        label = "Smoothing min cutoff (Hz)"
    )]
    #[serde(default = "default_smoothing_min_cutoff")]
    pub smoothing_min_cutoff: f32,

    /// MediaPipe-only: One-Euro speed coefficient — higher opens the cutoff
    /// faster during motion (less lag). Scale-normalized hand-lengths/sec.
    /// Default must match `mediapipe::smoothing::DEFAULT_BETA` (`6.0`).
    #[setting(
        default = 6.0_f32,
        min = 0.0,
        max = 10.0,
        step = 0.1,
        category = Dev,
        section = "Hand Tracking",
        label = "Smoothing beta"
    )]
    #[serde(default = "default_smoothing_beta")]
    pub smoothing_beta: f32,
}

/// Serde fallbacks so a config saved before these fields existed still loads
/// (the values stay in sync with the provider's compile-time defaults).
fn default_grab_rest_deadzone() -> f32 {
    0.05
}
fn default_depth_calibration_k() -> f32 {
    0.8
}
fn default_smoothing_min_cutoff() -> f32 {
    10.0
}
fn default_smoothing_beta() -> f32 {
    6.0
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;

    /// A settings file saved before the `provider` field existed must load
    /// with `Auto` — not error out, and never silently land on `Off`.
    #[test]
    fn provider_defaults_to_auto_when_absent_from_saved_settings() {
        let parsed: HandTrackingSettings =
            toml::from_str("leap_background = true").expect("pre-provider settings file loads");
        assert_eq!(parsed.provider, HandProviderChoice::Auto);
        assert!(parsed.leap_background, "sibling fields still load");
    }

    /// The persisted representation is the bare variant name — the same
    /// string the panel's reflection write-back uses, so persistence and the
    /// dropdown can never disagree about an identifier.
    #[test]
    fn provider_persists_as_the_variant_name() {
        let settings = HandTrackingSettings {
            provider: HandProviderChoice::MediaPipe,
            ..HandTrackingSettings::default()
        };
        let text = toml::to_string(&settings).expect("settings serialize");
        assert!(text.contains("provider = \"MediaPipe\""), "got: {text}");
    }

    /// Round-trip every variant through the persisted form.
    #[test]
    fn provider_choice_round_trips_through_toml() {
        for choice in [
            HandProviderChoice::Auto,
            HandProviderChoice::Leap,
            HandProviderChoice::MediaPipe,
            HandProviderChoice::Off,
        ] {
            let settings = HandTrackingSettings {
                provider: choice,
                ..HandTrackingSettings::default()
            };
            let text = toml::to_string(&settings).expect("serialize");
            let back: HandTrackingSettings = toml::from_str(&text).expect("parse back");
            assert_eq!(back.provider, choice);
        }
    }
}
