//! In-process `MediaPipe` webcam hand-tracking provider.
//!
//! Derives 21-landmark hands from a conventional webcam using `MediaPipe`'s
//! two-stage ONNX models (palm detection → ROI → landmark regression), run
//! in-process via the pure-Rust `tract` runtime. All pre/post-processing glue
//! (anchors, NMS, ROI affine, coordinate mapping, signals) lives in this module
//! directory; the provider emits into the same Leap-device-millimetre
//! convention the Leap provider uses, so every downstream consumer is unchanged.
//!
//! Data flow: a dedicated worker thread (Phase 8) owns the camera and the two
//! inference sessions and runs the pipeline at a capped rate, pushing completed
//! [`crate::input::hand::Hand`] frames onto a lock-free `rtrb` ring; the
//! provider's `poll` non-blockingly drains that ring on the Bevy main thread.
//! This skeleton reports lifecycle through
//! [`crate::input::state::ProviderStatus`] until the worker lands.
//!
//! See the design spec
//! `docs/superpowers/specs/2026-06-04-mediapipe-webcam-hand-tracking-design.md`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::prelude::*;

use crate::input::provider::HandTrackingProvider;
use crate::input::state::{
    HandTrackingError, HandTrackingFrame, ProviderDiagnostics, ProviderStatus,
};

mod anchors;
mod capture;
mod coords;
mod inference;
mod palm;
mod signals;

/// Construction-time configuration for the webcam provider.
#[derive(Debug, Clone)]
pub struct MediaPipeConfig {
    /// Camera index to open (0 = default device).
    pub camera_index: u32,
    /// Mirror the image horizontally (webcam-as-mirror — the natural
    /// installation interaction).
    pub mirror: bool,
    /// Inference rate cap, in Hz. Hand tracking does not need full frame rate;
    /// capping leaves CPU headroom for the render thread and lowers heat.
    pub max_inference_hz: u32,
}

impl Default for MediaPipeConfig {
    fn default() -> Self {
        Self {
            camera_index: 0,
            mirror: true,
            max_inference_hz: 30,
        }
    }
}

/// In-process webcam hand-tracking provider.
///
/// Construct with [`Self::new`], register in the
/// [`crate::input::provider::ProviderRegistry`] as
/// [`crate::input::provider::ProviderRole::Primary`]. The registry calls
/// [`HandTrackingProvider::start`] eagerly.
pub struct MediaPipeProvider {
    config: MediaPipeConfig,
    /// Shared status snapshot, written by the worker (Phase 8) and read in
    /// [`Self::status`]. Held behind a `Mutex` read once per frame on the Bevy
    /// main thread (not a real-time/audio thread, so a short uncontended lock
    /// is acceptable; the audio-thread no-`Mutex` rule does not apply here).
    status: Arc<Mutex<ProviderStatus>>,
    /// Shared diagnostics snapshot, written by the worker, read in
    /// [`Self::diagnostics`].
    diagnostics: Arc<Mutex<ProviderDiagnostics>>,
}

impl MediaPipeProvider {
    /// Construct a provider. Does not open the camera or load models; that
    /// happens in [`HandTrackingProvider::start`].
    #[must_use]
    pub fn new(config: MediaPipeConfig) -> Self {
        Self {
            config,
            status: Arc::new(Mutex::new(ProviderStatus::default())),
            diagnostics: Arc::new(Mutex::new(ProviderDiagnostics::default())),
        }
    }

    /// The configuration this provider was constructed with.
    #[must_use]
    pub fn config(&self) -> &MediaPipeConfig {
        &self.config
    }
}

impl HandTrackingProvider for MediaPipeProvider {
    fn start(&mut self) -> Result<(), HandTrackingError> {
        // The worker (camera open + model load + pipeline thread) lands in a
        // later phase. Until then, report Unavailable so the registry's
        // status-check treats this like a missing device and (under `auto`)
        // falls back to the mock provider.
        Err(HandTrackingError::Unavailable(
            "MediaPipeProvider worker not yet implemented".into(),
        ))
    }

    fn stop(&mut self) {}

    fn poll(&mut self, _now: Duration, _out: &mut Messages<HandTrackingFrame>) {}

    fn status(&self) -> ProviderStatus {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn diagnostics(&self) -> ProviderDiagnostics {
        self.diagnostics
            .lock()
            .map(|d| d.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::state::{PrimaryState, ServiceConnection};

    #[test]
    fn provider_before_start_is_not_started() {
        let p = MediaPipeProvider::new(MediaPipeConfig::default());
        assert!(matches!(p.status().service, ServiceConnection::NotStarted));
        assert_eq!(p.status().primary(), PrimaryState::NotStarted);
    }

    #[test]
    fn default_config_mirrors_and_caps_rate() {
        let p = MediaPipeProvider::new(MediaPipeConfig::default());
        assert!(p.config().mirror);
        assert_eq!(p.config().max_inference_hz, 30);
        assert_eq!(p.config().camera_index, 0);
    }
}
