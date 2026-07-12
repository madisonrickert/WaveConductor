//! In-process `MediaPipe` webcam hand-tracking provider.
//!
//! Derives 21-landmark hands from a conventional webcam using `MediaPipe`'s
//! two-stage ONNX models (palm detection → ROI → landmark regression), run
//! in-process via ONNX Runtime (`ort`) with CoreML acceleration on macOS. All
//! pre/post-processing glue (anchors, NMS, ROI affine, coordinate mapping,
//! signals) lives in this module directory; the provider emits into the same
//! Leap-device-millimetre convention the Leap provider uses, so every downstream
//! consumer is unchanged.
//!
//! Data flow: `HandTrackingProvider::start` loads the two ONNX models, opens a
//! `capture::FrameSource` (a real webcam under the
//! `hand-tracking-mediapipe-camera` feature, or an injected mock in tests), and
//! spawns a `worker` thread that runs the `pipeline::Pipeline` at a capped
//! rate and pushes completed [`crate::input::hand::Hand`] frames onto a
//! lock-free `rtrb` ring. `poll` non-blockingly drains that ring on the Bevy
//! main thread. While the sketch sits in `Idle`/`Screensaver`,
//! `apply_mediapipe_idle_throttle` lowers the cap to
//! `worker::IDLE_INFERENCE_HZ` (4 Hz) to shed sustained thermal load;
//! a hand on a throttled frame still emits and wakes the app (~300 ms worst
//! case — see the constant's docs).
//!
//! See the design spec
//! `docs/superpowers/specs/2026-06-04-mediapipe-webcam-hand-tracking-design.md`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::prelude::*;
use rtrb::Consumer;
use smallvec::SmallVec;

use self::capture::{CaptureError, FrameSource};
use self::inference::HandInference;
use self::pipeline::{MediaPipeLiveTuning, Pipeline, PipelineConfig};
use self::smoothing::{HandSmoother, DEFAULT_BETA, DEFAULT_MIN_CUTOFF};
use self::worker::{
    spawn_worker, MediaPipeWorkerDiagnostics, SourceFactory, WorkerHandle, WorkerMsg,
};
use crate::input::hand::Hand;
use crate::input::provider::{HandTrackingProvider, ProviderId};
use crate::input::state::{
    DevicePresence, HandTrackingError, HandTrackingFrame, ProviderDiagnostics, ProviderMetric,
    ProviderStatus, ServiceConnection, MAX_HANDS,
};

mod anchors;
// The capture module was promoted to `crate::input::capture` (shared with the
// body-tracking worker). This alias keeps every `self::capture::…` /
// `super::capture::…` path in this provider compiling unchanged.
use crate::input::capture;
mod coords;
mod inference;
/// ONNX Runtime (`ort`) inference backend; the sole concrete [`inference::HandInference`]
/// implementation used by this pipeline.
mod inference_ort;
mod landmark;
mod palm;
mod pipeline;
mod signals;
mod smoothing;
mod worker;

/// Backend label before the provider has loaded its ONNX Runtime sessions.
const BACKEND_NOT_STARTED: &str = "not started";
/// Backend label when one model stage registers `CoreML` and another falls back
/// to CPU.
const BACKEND_COREML_CPU: &str = "ort/CoreML+CPU";
/// Backend label when one model stage registers `DirectML` and another falls
/// back to CPU.
const BACKEND_DIRECTML_CPU: &str = "ort/DirectML+CPU";
/// Backend label for any mixed accelerator state outside the named platform
/// pairs above.
const BACKEND_MIXED: &str = "ort/mixed";

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
    /// Apply render-rate One-Euro smoothing (see `smoothing`). On by default;
    /// turn off to expose the raw inference poses at the backend's cadence (for A/B comparison
    /// during tuning). This on/off escape hatch is the only smoothing knob still
    /// read from an env var at startup (`WAVECONDUCTOR_HAND_SMOOTHING=off`); the
    /// One-Euro *parameters* below are live-tunable through
    /// [`crate::settings::HandTrackingSettings`] (dev panel, no restart).
    pub smoothing: bool,
    /// Rest deadzone for the grab signal so a relaxed-open hand reads exactly
    /// `0` (see `pipeline::PipelineConfig::grab_rest_deadzone`). Seeded from and
    /// kept in sync with [`crate::settings::HandTrackingSettings`] (dev panel).
    pub grab_rest_deadzone: f32,
    /// Calibration gain `k` for the size-estimated hand depth; `<= 0` disables
    /// the estimator and pins depth to the fixed 120 mm proxy (see
    /// `pipeline::PipelineConfig::depth_calibration_k`). Seeded from and kept
    /// in sync with [`crate::settings::HandTrackingSettings`] (dev panel).
    pub depth_calibration_k: f32,
    /// One-Euro minimum cutoff (Hz) for render-rate smoothing — the at-rest
    /// smoothing strength (see `smoothing::DEFAULT_MIN_CUTOFF`). Seeded from and
    /// kept in sync with [`crate::settings::HandTrackingSettings`] (dev panel).
    pub smoothing_min_cutoff: f32,
    /// One-Euro speed coefficient for render-rate smoothing (see
    /// `smoothing::DEFAULT_BETA`). Seeded from and kept in sync with
    /// [`crate::settings::HandTrackingSettings`] (dev panel).
    pub smoothing_beta: f32,
    /// Directory holding `palm_detection.onnx` and `hand_landmark.onnx`.
    /// Resolved at runtime via [`crate::platform::assets::asset_root`] so the
    /// path is correct in dev, release, and macOS `.app` bundle deployments.
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
            depth_calibration_k: PipelineConfig::default().depth_calibration_k,
            smoothing_min_cutoff: DEFAULT_MIN_CUTOFF,
            smoothing_beta: DEFAULT_BETA,
            model_dir: crate::platform::assets::asset_root().join("models/hand"),
        }
    }
}

/// In-process webcam hand-tracking provider.
///
/// Construct with [`Self::new`], register in the
/// [`crate::input::provider::ProviderRegistry`] as
/// [`crate::input::provider::ProviderRole::Primary`]. The registry calls
/// `HandTrackingProvider::start` eagerly.
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
    /// backend's inference cadence (hardware-dependent); `poll` runs at render
    /// rate (~60 fps) and eases the exposed pose toward [`Self::target_hands`]
    /// each call so motion reads as fluid. `MediaPipe`-only — all of this lives
    /// in this provider.
    smoother: HandSmoother,
    /// Latest inference result from the worker, held between worker frames as
    /// the smoothing target.
    target_hands: SmallVec<[Hand; MAX_HANDS]>,
    /// Capture timestamp of [`Self::target_hands`].
    target_ts: Duration,
    /// Whether the previous `poll` emitted a hand — lets us emit a single
    /// clearing frame when the last hand leaves, then go quiet.
    had_hands: bool,
    /// Live pipeline tunables (grab rest-deadzone, depth calibration `k`;
    /// lock-free `f32` bits), shared with the worker's [`Pipeline`] so the dev
    /// tuning panel can re-tune them without a restart. Written by
    /// [`Self::set_grab_deadzone`] / [`Self::set_depth_calibration_k`]; read by
    /// the pipeline each frame.
    live_tuning: Arc<MediaPipeLiveTuning>,
    /// Backend label selected when the provider last started. Reused by the
    /// dev-panel metrics refill so the visible "Backend" row matches the
    /// sessions actually registered by [`Self::build_pipeline`].
    backend_label: &'static str,
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
    /// happens in `HandTrackingProvider::start`.
    #[must_use]
    pub fn new(config: MediaPipeConfig) -> Self {
        let smoother = HandSmoother::new(config.smoothing_min_cutoff, config.smoothing_beta);
        let live_tuning = Arc::new(MediaPipeLiveTuning::new(
            config.grab_rest_deadzone,
            config.depth_calibration_k,
        ));
        Self {
            config,
            status: Arc::new(Mutex::new(ProviderStatus::default())),
            diagnostics: Arc::new(Mutex::new(ProviderDiagnostics::default())),
            runtime: Mutex::new(Runtime::default()),
            smoother,
            target_hands: SmallVec::new(),
            target_ts: Duration::ZERO,
            had_hands: false,
            live_tuning,
            backend_label: BACKEND_NOT_STARTED,
        }
    }

    /// Live-set the grab rest-deadzone (shared with the running worker pipeline).
    /// Cheap and lock-free; safe to call every frame from a tuning system.
    pub fn set_grab_deadzone(&self, deadzone: f32) {
        self.live_tuning.set_grab_deadzone(deadzone);
    }

    /// Live-set the depth calibration gain `k` (shared with the running worker
    /// pipeline). `<= 0` disables the size estimator (fixed 120 mm depth pin).
    /// Cheap and lock-free; safe to call every frame from a tuning system.
    pub fn set_depth_calibration_k(&self, k: f32) {
        self.live_tuning.set_depth_k(k);
    }

    /// Live-set the idle inference throttle (shared with the running worker:
    /// `true` caps inference at `worker::IDLE_INFERENCE_HZ`). Mirrors the
    /// `SketchActivity` state — see `apply_mediapipe_idle_throttle`. Cheap
    /// and lock-free (one Relaxed atomic store); safe to call every frame.
    pub fn set_idle_throttle(&self, idle: bool) {
        self.live_tuning.set_idle_throttle(idle);
    }

    /// Whether the idle inference throttle is currently requested.
    #[must_use]
    pub fn idle_throttle(&self) -> bool {
        self.live_tuning.idle_throttle()
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

    /// Set the horizontal mirror. Applies on the next `HandTrackingProvider::start`.
    pub fn set_mirror(&mut self, mirror: bool) {
        self.config.mirror = mirror;
    }

    /// Set the camera index. Applies on the next `HandTrackingProvider::start`.
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

    /// Build the pipeline from the vendored models, returning the combined
    /// inference backend label (`"ort/CoreML"`, `"ort/CPU"`, or the mixed state)
    /// for diagnostics.
    fn build_pipeline(&self) -> Result<(Pipeline, &'static str), HandTrackingError> {
        let dir = &self.config.model_dir;
        let (palm, palm_backend) = load_model(dir, "palm_detection.onnx")?;
        let (landmark, landmark_backend) = load_model(dir, "hand_landmark.onnx")?;
        let backend = combined_backend(palm_backend, landmark_backend);
        let cfg = PipelineConfig {
            mirror: self.config.mirror,
            grab_rest_deadzone: self.config.grab_rest_deadzone,
            depth_calibration_k: self.config.depth_calibration_k,
            ..PipelineConfig::default()
        };
        let mut pipeline = Pipeline::new(palm, landmark, cfg);
        // Share the live tuning cell so the dev panel reaches the worker.
        pipeline.set_live_tuning_source(Arc::clone(&self.live_tuning));
        Ok((pipeline, backend))
    }
}

/// Push live hand-tuning settings into the running `MediaPipe` provider.
///
/// Mirrors `apply_leap_background_setting`: a `PreUpdate` system (after polling)
/// that, when [`crate::settings::HandTrackingSettings`] changes, re-tunes the
/// `MediaPipe` provider in place — the grab rest-deadzone and depth calibration
/// `k` (forwarded lock-free to the worker pipeline) and the One-Euro smoothing
/// parameters. No restart, so the dev tuning panel adjusts feel live.
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
            mp.set_depth_calibration_k(settings.depth_calibration_k);
            mp.set_smoothing_params(settings.smoothing_min_cutoff, settings.smoothing_beta);
        }
    }
}

/// Whether the given sketch activity should throttle `MediaPipe` inference.
///
/// `Idle` and `Screensaver` (no audience interacting) → throttled; `Active` →
/// full rate. `None` is the `SketchActivity` sub-state being absent (the app
/// is on the Home screen, not in a sketch): full rate, because the idle
/// machinery — and therefore the hand-bearing-frame wake path — only runs
/// inside sketch states, so throttling there could never be undone by a hand.
#[must_use]
fn idle_throttle_for_activity(activity: Option<&crate::lifecycle::state::SketchActivity>) -> bool {
    use crate::lifecycle::state::SketchActivity;
    matches!(
        activity,
        Some(SketchActivity::Idle | SketchActivity::Screensaver)
    )
}

/// Mirror [`SketchActivity`](crate::lifecycle::state::SketchActivity) into the
/// running `MediaPipe` provider's idle-throttle flag.
///
/// `Idle`/`Screensaver` → the worker caps inference at
/// `worker::IDLE_INFERENCE_HZ`; `Active` (or Home, where the sub-state is
/// absent) → full rate. The store is **unconditional every frame** rather than
/// change-gated: it is one Relaxed atomic store behind a registry downcast (no
/// allocation, no lock), and the unconditional write makes provider rebuilds
/// correct by construction — a registry rebuilt mid-Idle (provider dropdown
/// switch) starts un-throttled and picks up the true activity state on the
/// very next frame, with no rebuild-detection plumbing.
///
/// Wake sequencing: a hand seen on a throttled frame still emits a
/// hand-bearing frame (the throttle lowers rate, not behavior), which resets
/// the idle timer (`lifecycle::idle::reset_on_interaction`), flips the state
/// to `Active`, and this mirror un-throttles. There is an inherent one-frame
/// race — the worker may process one more frame at the idle interval before
/// the cleared flag lands — which is harmless: it only delays the *second*
/// post-wake inference by at most one idle period.
pub fn apply_mediapipe_idle_throttle(
    activity: Option<Res<'_, State<crate::lifecycle::state::SketchActivity>>>,
    mut registry: ResMut<'_, crate::input::provider::ProviderRegistry>,
) {
    let throttled = idle_throttle_for_activity(activity.as_ref().map(|state| state.get()));
    for slot in registry.iter_mut() {
        if slot.id != crate::input::provider::ProviderId::MediaPipe {
            continue;
        }
        if let Some(mp) = slot
            .inner
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<MediaPipeProvider>())
        {
            mp.set_idle_throttle(throttled);
        }
    }
}

/// Refill the dev-panel metrics list from one worker diagnostics snapshot.
/// Extracted from `poll` so the per-frame drain stays one screen long; called
/// at most once per poll, under the (main-thread-only) diagnostics lock.
fn refill_metrics(
    d: &mut ProviderDiagnostics,
    worker_diag: &MediaPipeWorkerDiagnostics,
    backend: &'static str,
) {
    d.metrics.clear();
    let p = worker_diag.pipeline;
    d.metrics.push(ProviderMetric::text("Backend", backend));
    d.metrics
        .push(ProviderMetric::duration("Pipeline total", p.total));
    d.metrics.push(ProviderMetric::duration(
        "Capture+decode",
        worker_diag.capture_decode,
    ));
    d.metrics.push(ProviderMetric::duration(
        "Inference interval",
        worker_diag.inference_interval,
    ));
    // Whether the idle throttle was requested (app is Idle/Screensaver) or
    // off (Active or Home). Static strings — no per-poll allocation.
    // The effective inference period is always visible via "Inference interval"
    // above, which stays honest when max_hz is already slower than
    // IDLE_INFERENCE_HZ (the configured rate is authoritative in that case).
    d.metrics.push(ProviderMetric::text(
        "Idle throttle",
        if worker_diag.idle_throttled {
            "requested"
        } else {
            "off"
        },
    ));
    d.metrics
        .push(ProviderMetric::duration("Preprocess", p.preprocess));
    d.metrics.push(ProviderMetric::duration("Palm", p.palm));
    d.metrics
        .push(ProviderMetric::duration("Landmark", p.landmark));
    d.metrics
        .push(ProviderMetric::text("Palm reason", p.palm_reason.label()));
    d.metrics
        .push(ProviderMetric::count("Tracks before", p.tracks_before));
    d.metrics
        .push(ProviderMetric::count("Tracks after", p.tracks_after));
    d.metrics.push(ProviderMetric::count("Hands", p.hands));
    // Physical size-estimated distance of the focal hand — the tape-measure
    // calibration readout for depth_calibration_k (0 when no hand or when the
    // estimator is off, k <= 0).
    d.metrics.push(ProviderMetric::count(
        "Est. distance (mm)",
        p.est_distance_mm,
    ));
    // Raw (pre-deadzone) vs deadzoned grab of the focal hand, permille. Shows
    // the rest deadzone subtracting and lets the operator read the true
    // relaxed-hand rest floor.
    d.metrics
        .push(ProviderMetric::count("Grab raw (‰)", p.grab_raw_permille));
    d.metrics
        .push(ProviderMetric::count("Grab (‰)", p.grab_permille));
    d.metrics
        .push(ProviderMetric::count("Track churn", p.track_churn));
    d.metrics.push(ProviderMetric::count(
        "Pipeline errors",
        worker_diag.pipeline_errors,
    ));
    // Ring-buffer backpressure drops, reported distinctly from camera-frame
    // drops (`dropped_frames`) so the dev panel never misattributes a slow
    // consumer as a camera problem.
    d.metrics.push(ProviderMetric::count(
        "Ring-full drops",
        worker_diag.ring_full_drops,
    ));
    // Invariant: the per-poll metrics refill must stay within the SmallVec's
    // inline capacity (20) — a spill here would heap-allocate on every
    // diagnostics frame. Adding a metric that trips this assert means raising
    // the capacity in `ProviderDiagnostics::metrics`, not accepting the spill.
    debug_assert!(
        !d.metrics.spilled(),
        "ProviderDiagnostics::metrics spilled inline capacity ({} metrics)",
        d.metrics.len()
    );
}

/// Open a real webcam source on the calling (worker) thread, or error. Runs
/// inside the worker so `!Send` camera backends never cross threads.
fn open_camera_source(camera_index: u32) -> Result<Box<dyn FrameSource>, CaptureError> {
    #[cfg(all(feature = "hand-tracking-mediapipe-camera", target_os = "macos"))]
    {
        let source = capture::AvfFrameSource::open(camera_index)?;
        let boxed: Box<dyn FrameSource> = Box::new(source);
        Ok(boxed)
    }
    #[cfg(all(feature = "hand-tracking-mediapipe-camera", not(target_os = "macos")))]
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

/// Load one ONNX model and wrap it as a boxed [`HandInference`], returning the
/// inference backend label the session registered (`"ort/CoreML"`,
/// `"ort/DirectML"`, or `"ort/CPU"`) alongside it for diagnostics.
///
/// Reads the model file from `dir/name` and builds an [`inference_ort::OrtInference`]
/// session. ONNX Runtime reads input/output shapes directly from the graph, so no
/// shape hint is needed here.
fn load_model(
    dir: &Path,
    name: &str,
) -> Result<(Box<dyn HandInference>, &'static str), HandTrackingError> {
    let path = dir.join(name);
    let bytes = std::fs::read(&path).map_err(|e| {
        HandTrackingError::Misconfigured(format!("read model {}: {e}", path.display()))
    })?;
    let model = inference_ort::OrtInference::load(&bytes)
        .map_err(|e| HandTrackingError::Misconfigured(e.to_string()))?;
    // Read the backend before boxing — it lives on the concrete type, not the
    // `HandInference` trait object.
    let backend = model.backend();
    let boxed: Box<dyn HandInference> = Box::new(model);
    Ok((boxed, backend))
}

/// Combine the palm and landmark backend labels into one diagnostics string.
///
/// They normally agree; if one stage falls back to CPU while the other reaches a
/// platform accelerator, report the mixed state rather than hiding the slow path.
fn combined_backend(palm: &'static str, landmark: &'static str) -> &'static str {
    if palm == landmark {
        palm
    } else if palm == inference_ort::BACKEND_COREML || landmark == inference_ort::BACKEND_COREML {
        BACKEND_COREML_CPU
    } else if palm == inference_ort::BACKEND_DIRECTML || landmark == inference_ort::BACKEND_DIRECTML
    {
        BACKEND_DIRECTML_CPU
    } else {
        BACKEND_MIXED
    }
}

impl HandTrackingProvider for MediaPipeProvider {
    fn start(&mut self) -> Result<(), HandTrackingError> {
        let (pipeline, backend) = self.build_pipeline()?;
        self.backend_label = backend;
        // Surface where inference actually registered so a silent CPU fallback
        // (the 240% CPU symptom) is visible in the dev panel, not assumed away.
        tracing::info!("MediaPipe hand inference backend: {backend} (palm+landmark)");
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
        // The worker reads the shared tuning cell's idle-throttle flag each
        // loop iteration (Idle/Screensaver → IDLE_INFERENCE_HZ cap).
        let handle = spawn_worker(
            make_source,
            pipeline,
            self.config.max_inference_hz,
            Arc::clone(&self.live_tuning),
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
            d.sdk_version = Some(format!("MediaPipe ({backend}) palm+landmark"));
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
        self.backend_label = BACKEND_NOT_STARTED;
    }

    fn poll(&mut self, now: Duration, out: &mut Messages<HandTrackingFrame>) {
        // Drain the worker ring: keep the most recent hands as the smoothing
        // target and apply the latest status.
        let mut new_target: Option<(SmallVec<[Hand; MAX_HANDS]>, Duration)> = None;
        let mut new_status = None;
        let mut new_diagnostics = None;
        let mut new_error = None;
        let mut new_camera_format = None;
        if let Ok(rt) = self.runtime.get_mut() {
            if let Some(consumer) = rt.consumer.as_mut() {
                while let Ok(msg) = consumer.pop() {
                    match msg {
                        WorkerMsg::Hands { hands, timestamp } => {
                            new_target = Some((hands, timestamp));
                        }
                        WorkerMsg::Status(s) => new_status = Some(s),
                        WorkerMsg::Diagnostics(d) => new_diagnostics = Some(d),
                        WorkerMsg::Error(e) => new_error = Some(e),
                        WorkerMsg::CameraFormat(f) => new_camera_format = Some(f),
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
        if new_diagnostics.is_some() || new_error.is_some() || new_camera_format.is_some() {
            if let Ok(mut d) = self.diagnostics.lock() {
                if let Some(err) = new_error {
                    d.last_error = Some(err);
                }
                if let Some(fmt) = new_camera_format {
                    // Fold the negotiated format into the device label shown next
                    // to "Attached" in the dev panel.
                    d.device_serial = Some(format!("camera{} · {}", self.config.camera_index, fmt));
                }
                if let Some(worker_diag) = new_diagnostics {
                    d.dropped_frames = worker_diag.dropped_frames;
                    refill_metrics(&mut d, &worker_diag, self.backend_label);
                }
            }
        }

        // Ease the exposed pose toward the held target every poll, so the
        // backend's inference cadence renders as fluid ~60 fps motion. `now` is
        // `Time::elapsed` (monotonic), giving the One-Euro filter its dt. When
        // smoothing is disabled, emit the raw held pose for A/B comparison.
        let hands = if self.config.smoothing {
            self.smoother.smooth(&self.target_hands, now)
        } else {
            // NOT an allocation: `Hand` is heap-free (fixed arrays + scalars)
            // and the SmallVec holds ≤ MAX_HANDS inline, so this clone is a
            // stack memcpy. Fine on the per-poll path — do not "fix".
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
    fn idle_throttle_for_activity_throttles_only_unattended_states() {
        use crate::lifecycle::state::SketchActivity;
        // Active audience → full rate.
        assert!(!idle_throttle_for_activity(Some(&SketchActivity::Active)));
        // Idle and Screensaver → throttled.
        assert!(idle_throttle_for_activity(Some(&SketchActivity::Idle)));
        assert!(idle_throttle_for_activity(Some(
            &SketchActivity::Screensaver
        )));
        // Sub-state absent (Home screen): no idle machinery runs there, so a
        // hand could never un-throttle — must stay at full rate.
        assert!(!idle_throttle_for_activity(None));
    }

    #[test]
    fn provider_setter_round_trips_idle_throttle() {
        // The mirror system drives exactly this setter; a fresh provider must
        // start un-throttled (the mirror corrects it within one frame).
        let p = MediaPipeProvider::new(MediaPipeConfig::default());
        assert!(!p.idle_throttle(), "fresh provider starts at full rate");
        p.set_idle_throttle(true);
        assert!(p.idle_throttle());
        p.set_idle_throttle(false);
        assert!(!p.idle_throttle());
    }

    #[test]
    fn idle_metric_label_is_hz_independent() {
        // The dev panel's "Idle throttle" metric uses "requested"/"off"
        // (static strings, no per-poll allocation) rather than embedding the
        // Hz value. This keeps the label honest when max_hz is already slower
        // than IDLE_INFERENCE_HZ — the configured rate is authoritative then,
        // so showing a Hz would be misleading. The actual inference period is
        // always visible via the "Inference interval" metric.
        //
        // Pin IDLE_INFERENCE_HZ here anyway: it drives the wake-latency
        // contract documented in worker.rs; a retune requires updating both
        // the constant's doc and the AGENTS.md comment.
        assert_eq!(
            worker::IDLE_INFERENCE_HZ,
            4,
            "IDLE_INFERENCE_HZ changed: update the wake-latency contract doc in worker.rs"
        );
    }

    #[test]
    fn combined_backend_reports_coreml_cpu_mixed_state() {
        assert_eq!(
            combined_backend(inference_ort::BACKEND_COREML, inference_ort::BACKEND_CPU),
            BACKEND_COREML_CPU
        );
    }

    #[test]
    fn combined_backend_reports_directml_cpu_mixed_state() {
        assert_eq!(
            combined_backend(inference_ort::BACKEND_DIRECTML, inference_ort::BACKEND_CPU),
            BACKEND_DIRECTML_CPU
        );
    }

    #[test]
    fn metrics_backend_uses_selected_backend_label() {
        let mut diagnostics = ProviderDiagnostics::default();
        refill_metrics(
            &mut diagnostics,
            &MediaPipeWorkerDiagnostics::default(),
            inference_ort::BACKEND_DIRECTML,
        );
        assert_eq!(
            diagnostics.metrics.first().map(|metric| metric.value),
            Some(crate::input::state::ProviderMetricValue::Text(
                inference_ort::BACKEND_DIRECTML
            ))
        );
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
