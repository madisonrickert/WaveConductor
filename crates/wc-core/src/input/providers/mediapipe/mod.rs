//! In-process `MediaPipe` webcam hand-tracking provider.
//!
//! Derives 21-landmark hands from a conventional webcam using `MediaPipe`'s
//! two-stage ONNX models (palm detection → ROI → landmark regression), run
//! in-process via the pure-Rust `tract` runtime. All pre/post-processing glue
//! (anchors, NMS, ROI affine, coordinate mapping, signals) lives in this module
//! directory; the provider emits into the same Leap-device-millimetre
//! convention the Leap provider uses, so every downstream consumer is unchanged.
//!
//! Data flow: [`HandTrackingProvider::start`] loads the two ONNX models, opens a
//! [`capture::FrameSource`] (a real webcam under the
//! `hand-tracking-mediapipe-camera` feature, or an injected mock in tests), and
//! spawns a [`worker`] thread that runs the [`pipeline::Pipeline`] at a capped
//! rate and pushes completed [`crate::input::hand::Hand`] frames onto a
//! lock-free `rtrb` ring. `poll` non-blockingly drains that ring on the Bevy
//! main thread.
//!
//! See the design spec
//! `docs/superpowers/specs/2026-06-04-mediapipe-webcam-hand-tracking-design.md`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::prelude::*;
use rtrb::Consumer;
use smallvec::SmallVec;

use self::capture::{CaptureError, FrameSource};
use self::inference::{HandInference, TractInference};
use self::pipeline::{Pipeline, PipelineConfig};
use self::smoothing::{HandSmoother, DEFAULT_BETA, DEFAULT_MIN_CUTOFF};
use self::worker::{spawn_worker, SourceFactory, WorkerHandle, WorkerMsg};
use crate::input::hand::Hand;
use crate::input::provider::{HandTrackingProvider, ProviderId};
use crate::input::state::{
    DevicePresence, HandTrackingError, HandTrackingFrame, ProviderDiagnostics, ProviderStatus,
    ServiceConnection, MAX_HANDS,
};

mod anchors;
mod capture;
mod coords;
mod inference;
mod landmark;
mod palm;
mod pipeline;
mod signals;
mod smoothing;
mod worker;

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
    /// Apply render-rate One-Euro smoothing (see [`smoothing`]). On by default;
    /// turn off to expose the raw ~15–20 fps inference poses (for A/B comparison
    /// during tuning). The app wires this to `WAVECONDUCTOR_HAND_SMOOTHING`.
    pub smoothing: bool,
    /// Rest deadzone for the grab signal so a relaxed-open hand reads exactly
    /// `0` (see [`pipeline::PipelineConfig::grab_rest_deadzone`]). Seeded from and
    /// kept in sync with [`crate::settings::HandTrackingSettings`] (dev panel).
    pub grab_rest_deadzone: f32,
    /// One-Euro minimum cutoff (Hz) for render-rate smoothing — the at-rest
    /// smoothing strength (see [`smoothing::DEFAULT_MIN_CUTOFF`]). Seeded from and
    /// kept in sync with [`crate::settings::HandTrackingSettings`] (dev panel).
    pub smoothing_min_cutoff: f32,
    /// One-Euro speed coefficient for render-rate smoothing (see
    /// [`smoothing::DEFAULT_BETA`]). Seeded from and kept in sync with
    /// [`crate::settings::HandTrackingSettings`] (dev panel).
    pub smoothing_beta: f32,
    /// Directory holding `palm_detection.onnx` and `hand_landmark.onnx`.
    /// Defaults to the workspace-relative `assets/models/hand` (resolved at
    /// runtime against the working directory, like Bevy's `assets/`).
    pub model_dir: PathBuf,
}

impl Default for MediaPipeConfig {
    fn default() -> Self {
        Self {
            camera_index: 0,
            mirror: true,
            max_inference_hz: 30,
            smoothing: true,
            grab_rest_deadzone: PipelineConfig::default().grab_rest_deadzone,
            smoothing_min_cutoff: DEFAULT_MIN_CUTOFF,
            smoothing_beta: DEFAULT_BETA,
            model_dir: PathBuf::from("assets/models/hand"),
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
    /// Shared status snapshot, written by `poll` from worker messages and read
    /// in [`Self::status`]. A `Mutex` read once per frame on the Bevy main
    /// thread is fine (not a real-time/audio thread, so the no-`Mutex` rule does
    /// not apply here).
    status: Arc<Mutex<ProviderStatus>>,
    /// Shared diagnostics snapshot, read in [`Self::diagnostics`].
    diagnostics: Arc<Mutex<ProviderDiagnostics>>,
    /// Worker handle, ring consumer, and any test-injected source. Wrapped in a
    /// `Mutex` so the provider is `Sync` (the trait requires it) without
    /// `unsafe`: `rtrb::Consumer` and `Box<dyn FrameSource>` are `Send` but not
    /// `Sync`, and `Mutex<T: Send>` is `Sync`. Only ever accessed via `&mut self`
    /// (`get_mut`), so there is no real contention.
    runtime: Mutex<Runtime>,
    /// Render-rate One-Euro smoothing. The worker produces poses at the
    /// inference rate (~15–20 fps); `poll` runs at render rate (~60 fps) and
    /// eases the exposed pose toward [`Self::target_hands`] each call so motion
    /// reads as fluid. `MediaPipe`-only — all of this lives in this provider.
    smoother: HandSmoother,
    /// Latest inference result from the worker, held between worker frames as
    /// the smoothing target.
    target_hands: SmallVec<[Hand; MAX_HANDS]>,
    /// Capture timestamp of [`Self::target_hands`].
    target_ts: Duration,
    /// Whether the previous `poll` emitted a hand — lets us emit a single
    /// clearing frame when the last hand leaves, then go quiet.
    had_hands: bool,
    /// Live grab rest-deadzone (`f32` bits), shared with the worker's
    /// [`Pipeline`] so the dev tuning panel can re-tune it without a restart.
    /// Written by [`Self::set_grab_deadzone`]; read by the pipeline each frame.
    live_grab_deadzone: Arc<AtomicU32>,
}

/// The provider's running state (everything that exists only between `start`
/// and `stop`).
#[derive(Default)]
struct Runtime {
    worker: Option<WorkerHandle>,
    consumer: Option<Consumer<WorkerMsg>>,
    /// Test-injected source. `+ Send` so it can move into the worker factory.
    injected_source: Option<Box<dyn FrameSource + Send>>,
}

impl MediaPipeProvider {
    /// Construct a provider. Does not open the camera or load models; that
    /// happens in [`HandTrackingProvider::start`].
    #[must_use]
    pub fn new(config: MediaPipeConfig) -> Self {
        let smoother = HandSmoother::new(config.smoothing_min_cutoff, config.smoothing_beta);
        let live_grab_deadzone = Arc::new(AtomicU32::new(config.grab_rest_deadzone.to_bits()));
        Self {
            config,
            status: Arc::new(Mutex::new(ProviderStatus::default())),
            diagnostics: Arc::new(Mutex::new(ProviderDiagnostics::default())),
            runtime: Mutex::new(Runtime::default()),
            smoother,
            target_hands: SmallVec::new(),
            target_ts: Duration::ZERO,
            had_hands: false,
            live_grab_deadzone,
        }
    }

    /// Live-set the grab rest-deadzone (shared with the running worker pipeline).
    /// Cheap and lock-free; safe to call every frame from a tuning system.
    pub fn set_grab_deadzone(&self, deadzone: f32) {
        self.live_grab_deadzone
            .store(deadzone.to_bits(), Ordering::Relaxed);
    }

    /// Live-retune the render-rate smoothing (applies to tracked hands and to
    /// banks created later, without resetting any hand's filter state).
    pub fn set_smoothing_params(&mut self, min_cutoff: f32, beta: f32) {
        self.config.smoothing_min_cutoff = min_cutoff;
        self.config.smoothing_beta = beta;
        self.smoother.set_params(min_cutoff, beta);
    }

    /// The configuration this provider was constructed with.
    #[must_use]
    pub fn config(&self) -> &MediaPipeConfig {
        &self.config
    }

    /// Set the horizontal mirror. Applies on the next [`HandTrackingProvider::start`].
    pub fn set_mirror(&mut self, mirror: bool) {
        self.config.mirror = mirror;
    }

    /// Set the camera index. Applies on the next [`HandTrackingProvider::start`].
    pub fn set_camera_index(&mut self, index: u32) {
        self.config.camera_index = index;
    }

    /// Inject a frame source for testing (used instead of opening a webcam).
    #[cfg(test)]
    pub fn set_test_source(&mut self, source: Box<dyn FrameSource + Send>) {
        if let Ok(rt) = self.runtime.get_mut() {
            rt.injected_source = Some(source);
        }
    }

    /// Build the pipeline from the vendored models.
    fn build_pipeline(&self) -> Result<Pipeline, HandTrackingError> {
        let dir = &self.config.model_dir;
        let palm = load_model(dir, "palm_detection.onnx", &[1, 192, 192, 3])?;
        let landmark = load_model(dir, "hand_landmark.onnx", &[1, 224, 224, 3])?;
        let cfg = PipelineConfig {
            mirror: self.config.mirror,
            grab_rest_deadzone: self.config.grab_rest_deadzone,
            ..PipelineConfig::default()
        };
        let mut pipeline = Pipeline::new(palm, landmark, cfg);
        // Share the live deadzone cell so the tuning UI reaches the worker.
        pipeline.set_live_deadzone_source(Arc::clone(&self.live_grab_deadzone));
        Ok(pipeline)
    }
}

/// Push live hand-tuning settings into the running `MediaPipe` provider.
///
/// Mirrors `apply_leap_background_setting`: a `PreUpdate` system (after polling)
/// that, when [`crate::settings::HandTrackingSettings`] changes, re-tunes the
/// `MediaPipe` provider in place — the grab rest-deadzone (forwarded lock-free to
/// the worker pipeline) and the One-Euro smoothing parameters. No restart, so the
/// dev tuning panel adjusts feel live.
pub fn apply_mediapipe_tuning_settings(
    settings: Res<'_, crate::settings::HandTrackingSettings>,
    mut registry: ResMut<'_, crate::input::provider::ProviderRegistry>,
) {
    if !settings.is_changed() {
        return;
    }
    for slot in registry.iter_mut() {
        if slot.id != crate::input::provider::ProviderId::MediaPipe {
            continue;
        }
        if let Some(mp) = slot
            .inner
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<MediaPipeProvider>())
        {
            mp.set_grab_deadzone(settings.grab_rest_deadzone);
            mp.set_smoothing_params(settings.smoothing_min_cutoff, settings.smoothing_beta);
        }
    }
}

/// Open a real webcam source on the calling (worker) thread, or error. Runs
/// inside the worker so `!Send` camera backends never cross threads.
fn open_camera_source(camera_index: u32) -> Result<Box<dyn FrameSource>, CaptureError> {
    #[cfg(feature = "hand-tracking-mediapipe-camera")]
    {
        let source = capture::NokhwaFrameSource::open(camera_index)?;
        let boxed: Box<dyn FrameSource> = Box::new(source);
        Ok(boxed)
    }
    #[cfg(not(feature = "hand-tracking-mediapipe-camera"))]
    {
        let _ = camera_index;
        Err(CaptureError::NoCamera(
            "build with the hand-tracking-mediapipe-camera feature".into(),
        ))
    }
}

/// Load one ONNX model and wrap it as a boxed [`HandInference`].
fn load_model(
    dir: &Path,
    name: &str,
    input_shape: &[usize],
) -> Result<Box<dyn HandInference>, HandTrackingError> {
    let path = dir.join(name);
    let bytes = std::fs::read(&path).map_err(|e| {
        HandTrackingError::Misconfigured(format!("read model {}: {e}", path.display()))
    })?;
    let model = TractInference::load(&bytes, input_shape)
        .map_err(|e| HandTrackingError::Misconfigured(e.to_string()))?;
    let boxed: Box<dyn HandInference> = Box::new(model);
    Ok(boxed)
}

impl HandTrackingProvider for MediaPipeProvider {
    fn start(&mut self) -> Result<(), HandTrackingError> {
        let pipeline = self.build_pipeline()?;
        // A test-injected source is used directly; otherwise the worker opens the
        // webcam on its own thread (camera backends can be !Send). Both arms
        // produce a `Send` factory.
        let injected = self
            .runtime
            .get_mut()
            .ok()
            .and_then(|rt| rt.injected_source.take());
        let camera_index = self.config.camera_index;
        let make_source: SourceFactory = match injected {
            Some(src) => Box::new(move || {
                let boxed: Box<dyn FrameSource> = src;
                Ok(boxed)
            }),
            None => Box::new(move || open_camera_source(camera_index)),
        };
        let (producer, consumer) = rtrb::RingBuffer::new(256);
        let handle = spawn_worker(
            make_source,
            pipeline,
            self.config.max_inference_hz,
            producer,
        );
        if let Ok(rt) = self.runtime.get_mut() {
            rt.worker = Some(handle);
            rt.consumer = Some(consumer);
        }
        if let Ok(mut s) = self.status.lock() {
            // The worker flips this to Streaming via its first status message;
            // Connecting here lets the registry's start-check see success.
            s.service = ServiceConnection::Connecting;
            s.device = DevicePresence::Attached;
        }
        if let Ok(mut d) = self.diagnostics.lock() {
            d.sdk_version = Some("MediaPipe (tract) palm+landmark".into());
            d.device_serial = Some(format!("camera{}", self.config.camera_index));
        }
        // Cold-start the smoothing so a restart carries no stale pose/momentum.
        self.smoother.clear();
        self.target_hands.clear();
        self.target_ts = Duration::ZERO;
        self.had_hands = false;
        Ok(())
    }

    fn stop(&mut self) {
        if let Ok(rt) = self.runtime.get_mut() {
            if let Some(mut worker) = rt.worker.take() {
                worker.stop();
            }
            rt.consumer = None;
        }
        if let Ok(mut s) = self.status.lock() {
            *s = ProviderStatus::default();
        }
        self.smoother.clear();
        self.target_hands.clear();
        self.had_hands = false;
    }

    fn poll(&mut self, now: Duration, out: &mut Messages<HandTrackingFrame>) {
        // Drain the worker ring: keep the most recent hands as the smoothing
        // target and apply the latest status.
        let mut new_target: Option<(SmallVec<[Hand; MAX_HANDS]>, Duration)> = None;
        let mut new_status = None;
        if let Ok(rt) = self.runtime.get_mut() {
            if let Some(consumer) = rt.consumer.as_mut() {
                while let Ok(msg) = consumer.pop() {
                    match msg {
                        WorkerMsg::Hands { hands, timestamp } => {
                            new_target = Some((hands, timestamp));
                        }
                        WorkerMsg::Status(s) => new_status = Some(s),
                    }
                }
            }
        }
        if let Some(status) = new_status {
            if let Ok(mut s) = self.status.lock() {
                *s = status;
            }
        }
        if let Some((hands, timestamp)) = new_target {
            self.target_hands = hands;
            self.target_ts = timestamp;
        }

        // Ease the exposed pose toward the held target every poll, so a
        // ~15–20 fps inference source renders as fluid ~60 fps motion. `now` is
        // `Time::elapsed` (monotonic), giving the One-Euro filter its dt. When
        // smoothing is disabled, emit the raw held pose for A/B comparison.
        let hands = if self.config.smoothing {
            self.smoother.smooth(&self.target_hands, now)
        } else {
            self.target_hands.clone()
        };
        // Emit while a hand is present, plus one clearing frame when the last
        // hand leaves — then stay quiet rather than spamming empty frames.
        if !hands.is_empty() || self.had_hands {
            self.had_hands = !hands.is_empty();
            out.write(HandTrackingFrame {
                provider: ProviderId::MediaPipe,
                hands,
                timestamp: self.target_ts,
            });
        }
    }

    fn status(&self) -> ProviderStatus {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn diagnostics(&self) -> ProviderDiagnostics {
        self.diagnostics
            .lock()
            .map(|d| d.clone())
            .unwrap_or_default()
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::capture::{Frame, MockFrameSource};
    use super::*;
    use crate::input::state::PrimaryState;

    fn vendored_models() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/models/hand")
    }

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
        assert!(p.config().smoothing, "smoothing on by default");
    }

    #[test]
    fn lifecycle_with_mock_source_reaches_streaming() {
        let config = MediaPipeConfig {
            model_dir: vendored_models(),
            ..MediaPipeConfig::default()
        };
        let mut provider = MediaPipeProvider::new(config);
        provider.set_test_source(Box::new(MockFrameSource::looping(vec![{
            let mut f = Frame::default();
            f.fit_to(64, 48);
            f
        }])));

        provider
            .start()
            .expect("provider should start with a mock source");

        // poll drains worker status messages into the shared snapshot.
        let mut messages = Messages::<HandTrackingFrame>::default();
        let mut streaming = false;
        for _ in 0..200 {
            provider.poll(Duration::ZERO, &mut messages);
            if provider.status().primary() == PrimaryState::Streaming {
                streaming = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        provider.stop();
        assert!(streaming, "provider never reached Streaming");
        assert_eq!(provider.status().primary(), PrimaryState::NotStarted); // after stop
    }

    #[test]
    fn missing_models_fail_to_start_cleanly() {
        let config = MediaPipeConfig {
            model_dir: PathBuf::from("definitely_missing_models_dir"),
            ..MediaPipeConfig::default()
        };
        let mut provider = MediaPipeProvider::new(config);
        provider.set_test_source(Box::new(MockFrameSource::solid(8, 8, [0, 0, 0])));
        assert!(provider.start().is_err());
    }
}
