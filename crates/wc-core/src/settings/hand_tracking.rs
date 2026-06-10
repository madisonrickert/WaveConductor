//! Global hand-tracking settings, persisted across sessions.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

/// Hand-tracking-wide settings (not per-sketch).
///
/// `leap_background`: should the Leap provider request the
/// `BackgroundFrames` policy at start? When `true`, tracking frames keep
/// arriving even when the `WaveConductor` window is not focused. Default
/// `false` per v4.
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "hand_tracking")]
pub struct HandTrackingSettings {
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
