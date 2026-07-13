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
/// The `WAVECONDUCTOR_HAND_PROVIDER` env var, when set, selects the
/// provider installed at *launch* (launch scripts, capture harness); this
/// setting takes over on its first change — there is no session pin. The
/// env-only `mock` / `synthetic` test fixtures are developer scaffolding,
/// not user choices, so they do not appear here.
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

/// How the `MediaPipe` inference sessions should choose an execution provider —
/// the operator's override for the GPU-vs-CPU decision, so a box whose GPU EP
/// crashes at graph fusion can be pinned to CPU (or forced back onto the GPU for
/// diagnosis) without a rebuild.
///
/// Variant identifiers double as the persisted strings *and* the dropdown labels
/// (see `wc_core_macros`: serde serializes unit variants as their name, and the
/// panel has no per-variant label mapping), so they are chosen to read in a
/// dropdown.
///
/// Applied at provider (re)start: `MediaPipeConfig::backend` is seeded from this
/// on registry build, and each ONNX model resolves its provider from it in
/// `OrtInference::load`. Changing it takes effect when the provider is next
/// rebuilt (relaunch, or a toggle of the "Tracking provider" dropdown) — there is
/// no live per-frame re-tune, because the choice is only read while a session is
/// being constructed.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HandTrackingBackend {
    /// Attempt the platform GPU EP (`CoreML` / `DirectML`); if it fails at commit,
    /// warn and rebuild that model on the CPU EP. The safety net — the default.
    #[default]
    Auto,
    /// Attempt the platform GPU EP and do **not** fall back: a commit failure is
    /// surfaced as a load error. Disables the safety net deliberately, so a
    /// broken EP is loud rather than silently degraded — a diagnosis lever, not a
    /// deployment default.
    ForceGpu,
    /// Never register a GPU EP; build a CPU-only session from the start. The
    /// fastest way to confirm CPU tracking works, and the operator's lever when a
    /// GPU EP is flaky.
    ForceCpu,
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
    /// starts the new one, no restart — including over a provider launched
    /// via the `WAVECONDUCTOR_HAND_PROVIDER` env default.
    #[setting(
        default = HandProviderChoice::Auto,
        ty = Enum,
        category = User,
        section = "Hand Tracking",
        label = "Tracking provider"
    )]
    #[serde(default)]
    pub provider: HandProviderChoice,

    /// Which execution provider the `MediaPipe` ONNX sessions should use
    /// (`Auto` tries the platform GPU EP and falls back to CPU on a commit
    /// failure; `ForceGpu` disables that fallback; `ForceCpu` skips the GPU EP
    /// entirely). Applied when the provider is next (re)built — see
    /// [`HandTrackingBackend`]. Exposed as a `User` knob so a field tester can
    /// A/B GPU vs CPU inference without a new build.
    ///
    /// `requires_restart`: unlike `provider`, this is read only while a
    /// `MediaPipe` session is being constructed (`register_mediapipe` seeds
    /// `MediaPipeConfig::backend` from it), not on a live per-frame path. A raw
    /// edit takes effect on the next provider (re)build — a relaunch, or
    /// toggling the "Tracking provider" dropdown off and back — never on the
    /// value change alone. The flag draws the panel's amber restart badge so
    /// the operator isn't left wondering why flipping it did nothing yet.
    #[setting(
        default = HandTrackingBackend::Auto,
        ty = Enum,
        category = User,
        section = "Hand Tracking",
        label = "Inference backend",
        requires_restart
    )]
    #[serde(default)]
    pub backend: HandTrackingBackend,

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
        label = "Smoothing min cutoff",
        unit = "Hz"
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

    /// The inference backend preference defaults to `Auto` when a settings file
    /// saved before the field existed is loaded — never erroring, never landing
    /// on a forced mode the operator did not choose.
    #[test]
    fn backend_defaults_to_auto_when_absent_from_saved_settings() {
        let parsed: HandTrackingSettings =
            toml::from_str("leap_background = true").expect("pre-backend settings file loads");
        assert_eq!(parsed.backend, HandTrackingBackend::Auto);
    }

    /// The persisted representation is the bare variant name, matching the
    /// dropdown's reflection write-back, so persistence and the panel can never
    /// disagree about an identifier.
    #[test]
    fn backend_persists_as_the_variant_name() {
        let settings = HandTrackingSettings {
            backend: HandTrackingBackend::ForceCpu,
            ..HandTrackingSettings::default()
        };
        let text = toml::to_string(&settings).expect("settings serialize");
        assert!(text.contains("backend = \"ForceCpu\""), "got: {text}");
    }

    /// Round-trip every variant through the persisted form.
    #[test]
    fn backend_choice_round_trips_through_toml() {
        for choice in [
            HandTrackingBackend::Auto,
            HandTrackingBackend::ForceGpu,
            HandTrackingBackend::ForceCpu,
        ] {
            let settings = HandTrackingSettings {
                backend: choice,
                ..HandTrackingSettings::default()
            };
            let text = toml::to_string(&settings).expect("serialize");
            let back: HandTrackingSettings = toml::from_str(&text).expect("parse back");
            assert_eq!(back.backend, choice);
        }
    }

    /// `backend` is read only when a `MediaPipe` session is next constructed
    /// (see `register_mediapipe` in the binary crate), never on a live
    /// per-frame path — so it must carry `requires_restart`. Otherwise the
    /// panel shows no amber badge and an operator flipping the dropdown has no
    /// indication the change is not yet in effect.
    #[test]
    fn backend_is_marked_requires_restart() {
        use crate::settings::SketchSettings;

        let Some(def) = HandTrackingSettings::settings_def()
            .into_iter()
            .find(|d| d.field_name == "backend")
        else {
            unreachable!("the derive macro always emits a def for `backend`");
        };
        assert!(
            def.requires_restart,
            "backend only takes effect on the next provider (re)build; \
             the panel must show the restart badge"
        );
    }
}
