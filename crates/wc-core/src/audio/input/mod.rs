//! Audio *input* capture and analysis (Radiance Unit A).
//!
//! The output engine's architecture (`super::engine`) run in reverse:
//!
//! ```text
//!   ┌─────────────────────────────────┐      ┌───────────────────────────┐
//!   │ Bevy main thread (per frame)    │      │ cpal input thread (kHz)   │
//!   │                                 │      │                           │
//!   │  Plan C inserts/removes         │      │  input callback           │
//!   │   Res<AudioCaptureRequest> ─────┼──┐   │   downmix to mono         │
//!   │                                 │  │   │   push f32 ──▶ rtrb ring  │
//!   │  PreUpdate: drive_capture ◀─────┼──┘   │   errors ──▶ AtomicBool   │
//!   │   (build/pause/teardown stream) │      └───────────────────────────┘
//!   │  PreUpdate: drain_and_analyze   │                  │
//!   │   ring ─▶ AnalysisEngine ─▶ Res<AudioAnalysis> ◀───┘
//!   └─────────────────────────────────┘
//! ```
//!
//! ## Activation contract (pinned across the Radiance plans)
//!
//! Inserting [`AudioCaptureRequest`] starts capture; removing it stops
//! capture. `paused: true` pauses the cpal stream and holds
//! [`AudioAnalysis`] at [`AudioAnalysis::neutral`]. The request is
//! sketch-agnostic: Plan C inserts it on entering Radiance and removes it on
//! exit; nothing in this module names a sketch.
//!
//! ## Failure posture
//!
//! Missing/failed/vanished device: [`AudioAnalysis`] holds neutral values,
//! the failure surfaces in `capture::AudioInputStatus` (diagnostics), and the
//! capture driver retries on a cooldown. Never panics, never blocks, never
//! silently falls back to a different device than the one requested.
//!
//! ## Always-on cost
//!
//! These systems are registered unconditionally (core plumbing, like the
//! settings-reload listeners): with no request present they no-op after a
//! couple of resource-existence checks per frame.

pub mod analysis;
pub mod capture;
pub mod devices;

use bevy::prelude::*;

/// Number of log-spaced spectral bands published in [`AudioAnalysis::bands`].
pub const AUDIO_BAND_COUNT: usize = 8;

/// Main-thread snapshot of the live audio-input analysis.
///
/// Always present once [`AudioInputPlugin`] is added; holds
/// [`AudioAnalysis::neutral`] whenever capture is inactive, paused, or
/// failed. Updated each `PreUpdate` by `analysis::drain_and_analyze`.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct AudioAnalysis {
    /// Post-AGC smoothed level, approximately `0..1`.
    pub rms: f32,
    /// Current AGC gain multiplier (`1.0` when neutral).
    pub gain: f32,
    /// Log-spaced band energies, post-AGC, approximately `0..1`.
    /// Band edges are `analysis::BAND_EDGES_HZ` (50 Hz – 12.8 kHz, octave
    /// spaced).
    pub bands: [f32; AUDIO_BAND_COUNT],
    /// Spectral-flux onset strength this frame, `>= 0`. Normalized against a
    /// slow running mean of flux, so ~1 is "typical activity" and spikes of
    /// 2–3+ indicate an onset.
    pub onset: f32,
    /// Debounced beat estimate, `0..1`: snaps to 1 on a detected beat and
    /// decays exponentially between beats.
    pub beat_confidence: f32,
    /// Post-AGC decaying peak-hold level, approximately `0..1`. Additive
    /// field beyond the pinned contract (spec computes RMS *and* peak).
    pub peak: f32,
    /// Capture stream is healthy and producing samples.
    pub active: bool,
}

impl AudioAnalysis {
    /// The inactive/failed/paused value: zeros, unity gain, not active.
    pub const fn neutral() -> Self {
        Self {
            rms: 0.0,
            gain: 1.0,
            bands: [0.0; AUDIO_BAND_COUNT],
            onset: 0.0,
            beat_confidence: 0.0,
            peak: 0.0,
            active: false,
        }
    }
}

impl Default for AudioAnalysis {
    fn default() -> Self {
        Self::neutral()
    }
}

/// Activation contract: INSERT this resource to start capture; REMOVE it to
/// stop. Sketch-agnostic — Plan C inserts it `OnEnter(AppState::Radiance)`
/// and removes it `OnExit`.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct AudioCaptureRequest {
    /// Which input device to capture from. `None` = system default input
    /// device. Names come from `devices::AvailableAudioInputDevices`.
    pub device_name: Option<String>,
    /// `true` during Idle/Screensaver: the cpal stream is paused and
    /// [`AudioAnalysis`] holds neutral values (attract mode is not
    /// audio-reactive).
    pub paused: bool,
}

/// Wires audio-input capture + analysis into the app.
///
/// Registered by [`super::AudioPlugin`] (core audio plumbing — never by a
/// sketch). Publishes [`AudioAnalysis`] and the
/// `devices::AvailableAudioInputDevices` runtime-enum source; reacts to
/// [`AudioCaptureRequest`] insert/remove/change every frame.
pub struct AudioInputPlugin;

impl Plugin for AudioInputPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AudioAnalysis>()
            .init_resource::<capture::AudioInputStatus>()
            .init_resource::<capture::CaptureRuntime>()
            .add_systems(PreUpdate, analysis::drain_and_analyze);
        // Task 8 chains capture::drive_capture ahead of the drain.
        // Later tasks extend this further: devices registry (Task 7).
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions; expect_used is denied workspace-wide for non-test code"
)]
mod tests {
    use super::*;

    #[test]
    fn neutral_analysis_is_zeroed_with_unity_gain_and_inactive() {
        let neutral = AudioAnalysis::neutral();
        assert!((neutral.rms - 0.0).abs() < f32::EPSILON);
        assert!((neutral.gain - 1.0).abs() < f32::EPSILON);
        assert!(neutral.bands.iter().all(|b| b.abs() < f32::EPSILON));
        assert!((neutral.onset - 0.0).abs() < f32::EPSILON);
        assert!((neutral.beat_confidence - 0.0).abs() < f32::EPSILON);
        assert!((neutral.peak - 0.0).abs() < f32::EPSILON);
        assert!(!neutral.active);
        assert_eq!(AudioAnalysis::default(), neutral);
    }

    #[test]
    fn plugin_installs_neutral_analysis_resource() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(AudioInputPlugin);
        app.update();
        let analysis = app.world().resource::<AudioAnalysis>();
        assert_eq!(*analysis, AudioAnalysis::neutral());
    }
}
