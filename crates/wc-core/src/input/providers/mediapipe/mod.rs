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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::prelude::*;
use rtrb::Consumer;

use self::capture::{CaptureError, FrameSource};
use self::inference::{HandInference, TractInference};
use self::pipeline::{Pipeline, PipelineConfig};
use self::worker::{spawn_worker, SourceFactory, WorkerHandle, WorkerMsg};
use crate::input::provider::{HandTrackingProvider, ProviderId};
use crate::input::state::{
    DevicePresence, HandTrackingError, HandTrackingFrame, ProviderDiagnostics, ProviderStatus,
    ServiceConnection,
};

mod anchors;
mod capture;
mod coords;
mod inference;
mod landmark;
mod palm;
mod pipeline;
mod signals;
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
        Self {
            config,
            status: Arc::new(Mutex::new(ProviderStatus::default())),
            diagnostics: Arc::new(Mutex::new(ProviderDiagnostics::default())),
            runtime: Mutex::new(Runtime::default()),
        }
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
            ..PipelineConfig::default()
        };
        Ok(Pipeline::new(palm, landmark, cfg))
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
    }

    fn poll(&mut self, _now: Duration, out: &mut Messages<HandTrackingFrame>) {
        let mut latest = None;
        let mut new_status = None;
        if let Ok(rt) = self.runtime.get_mut() {
            if let Some(consumer) = rt.consumer.as_mut() {
                while let Ok(msg) = consumer.pop() {
                    match msg {
                        WorkerMsg::Hands { hands, timestamp } => latest = Some((hands, timestamp)),
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
        if let Some((hands, timestamp)) = latest {
            out.write(HandTrackingFrame {
                provider: ProviderId::MediaPipe,
                hands,
                timestamp,
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
