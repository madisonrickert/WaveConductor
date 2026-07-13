//! Background worker running the [`super::pipeline::PosePipeline`] off the
//! Bevy main thread, publishing results over a lock-free `rtrb` ring.
//!
//! Adapted from the hand-tracking worker (same rate-cap mechanism: the
//! budget decision happens BEFORE capture, and over-budget frames are
//! drained **undecoded** so the camera stream stays fresh — newest frame
//! wins — while the decode cost is skipped). Body-specific differences:
//!
//! - **Models are built on this thread** via the [`PipelineFactory`]: body
//!   tracking starts on sketch entry, and a first-launch `CoreML` compile must
//!   not hitch the render thread. The backend label crosses back as
//!   [`BodyWorkerMsg::Backend`].
//! - **Payload pool client:** full frames claim a pooled
//!   [`super::transport::BodyFramePayload`] from the recycle ring; idle
//!   probes never touch the pool; pool exhaustion degrades to payload-less
//!   frames (landmarks stay fresh, the mask skips a frame) instead of
//!   blocking or allocating.
//! - The idle throttle selects the pipeline's detector-only probe in
//!   addition to lowering the rate to the shared
//!   `capture::IDLE_INFERENCE_HZ`; the hardware capture throttle is
//!   dispatched edge-triggered exactly like the hand worker.
//! - **Deviation from the hand worker:** each over-budget drop pushes a
//!   [`BodyWorkerMsg::Diagnostics`] carrying the updated drop count
//!   immediately (the hand worker instead folds the count into its next
//!   `Status`). Without an immediate push here, a slow `max_hz` (e.g. 1 Hz)
//!   would leave a consumer unaware a frame was dropped until the next
//!   *processed* frame's diagnostics arrive, seconds later.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rtrb::{Consumer, Producer};

use super::pipeline::{BodyLiveTuning, PoseConfig, PosePipeline};
use super::transport::{BodyFrame, BodyFramePayload, BodyWorkerDiagnostics, BodyWorkerMsg};
use super::BodyTrackingStatus;
use crate::input::capture::{CaptureError, Frame, FrameSource, IDLE_INFERENCE_HZ};
use crate::input::onnx::ModelInference;

/// Vendored detector filename under the model directory.
pub const POSE_DETECTION_MODEL: &str = "pose_detection.onnx";

/// Vendored landmark/segmentation filename under the model directory.
pub const POSE_LANDMARK_MODEL: &str = "pose_landmark_full.onnx";

/// Idle backoff when the source has no frame ready, so a non-blocking source
/// can't busy-spin a core (mainly guards mock sources).
const IDLE_POLL: Duration = Duration::from_millis(2);

/// Cooldown between camera-open attempts after a failure. Mirrors the audio
/// capture path's `RETRY_COOLDOWN_S`: a webcam momentarily held by another
/// process — or a kiosk camera still enumerating shortly after boot — should
/// be reacquired within a couple of seconds rather than permanently killing
/// the worker.
const OPEN_RETRY_COOLDOWN: Duration = Duration::from_secs(2);

/// Poll granularity while waiting out the open-retry cooldown, so the stop
/// flag is observed within this bound during shutdown (prompt join).
const RETRY_STOP_POLL: Duration = Duration::from_millis(50);

/// Creates the frame source on the worker thread (deferred so `!Send` camera
/// backends are built where they are used; the factory itself is `Send`).
/// `FnMut` — not `FnOnce` — so the open-retry loop can reattempt after a
/// transient camera-open failure (see `open_source_with_retry`).
pub type SourceFactory = Box<dyn FnMut() -> Result<Box<dyn FrameSource>, CaptureError> + Send>;

/// Builds the pose pipeline (model files + ort sessions) on the worker
/// thread, returning it with the combined inference backend label. The
/// error string is what crosses the ring as [`BodyWorkerMsg::Error`].
pub type PipelineFactory = Box<dyn FnOnce() -> Result<(PosePipeline, &'static str), String> + Send>;

/// Handle to a running worker; dropping or [`Self::stop`] joins the thread.
pub struct WorkerHandle {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl WorkerHandle {
    /// Signal the worker to stop and join it.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Load the two vendored pose models and build the pipeline (worker-thread
/// only — the `CoreML` compile can take seconds on first launch). Returns the
/// pipeline plus the combined backend label for diagnostics.
///
/// # Errors
/// Returns a human-readable string when a model file is unreadable or a
/// session fails to build.
pub fn load_pose_pipeline(model_dir: &Path) -> Result<(PosePipeline, &'static str), String> {
    let (detector, det_backend) = load_model(model_dir, POSE_DETECTION_MODEL)?;
    let (landmark, lm_backend) = load_model(model_dir, POSE_LANDMARK_MODEL)?;
    let backend = combined_backend(det_backend, lm_backend);
    Ok((
        PosePipeline::new(detector, landmark, PoseConfig::default()),
        backend,
    ))
}

/// Load one ONNX model as a boxed [`ModelInference`] with its backend label.
fn load_model(dir: &Path, name: &str) -> Result<(Box<dyn ModelInference>, &'static str), String> {
    let path = dir.join(name);
    let bytes = std::fs::read(&path).map_err(|e| format!("read model {}: {e}", path.display()))?;
    let model = crate::input::onnx::ort::OrtInference::load(&bytes).map_err(|e| e.to_string())?;
    let backend = model.backend();
    let boxed: Box<dyn ModelInference> = Box::new(model);
    Ok((boxed, backend))
}

/// Combine the two stages' backend labels (they normally agree; a mixed
/// state must not hide the slow path — same policy as the hand provider).
fn combined_backend(detector: &'static str, landmark: &'static str) -> &'static str {
    if detector == landmark {
        detector
    } else {
        "ort/mixed"
    }
}

/// Spawn the worker thread. Runs until [`WorkerHandle::stop`] (or drop):
/// builds the pipeline (via `make_pipeline`) and the camera (via
/// `make_source`) on the worker thread, then captures + processes at up to
/// `max_hz` (or the idle cap while `tuning.idle_throttle()` holds), pushing
/// [`BodyWorkerMsg`]s to `producer` and claiming mask payloads from
/// `recycle`. OS thread-spawn failure is reported through the ring rather
/// than swallowed (same producer-slot reclaim as the hand worker).
#[must_use]
pub fn spawn_body_worker(
    make_source: SourceFactory,
    make_pipeline: PipelineFactory,
    max_hz: u32,
    tuning: Arc<BodyLiveTuning>,
    producer: Producer<BodyWorkerMsg>,
    recycle: Consumer<Box<BodyFramePayload>>,
) -> WorkerHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let min_inference_interval = inference_interval(max_hz);

    // `producer` must move into the closure, but a failed thread spawn drops
    // the closure without handing it back; the shared slot lets the failure
    // branch reclaim it and still report the error (hand-worker pattern).
    let producer_slot = Arc::new(Mutex::new(Some(producer)));
    let producer_for_thread = Arc::clone(&producer_slot);

    let spawn_result = std::thread::Builder::new()
        .name("wc-body-worker".into())
        .spawn(move || {
            let Some(mut producer) = producer_for_thread
                .lock()
                .ok()
                .and_then(|mut slot| slot.take())
            else {
                // Unreachable in practice (see the hand worker's rationale);
                // guarded so a refactor can't turn it into a thread panic.
                return;
            };
            // Build the models/sessions HERE so CoreML compiles off the main
            // thread; failure is a Failed status + the error string.
            let mut pipeline = match make_pipeline() {
                Ok((pipeline, backend)) => {
                    let _ = producer.push(BodyWorkerMsg::Backend(backend));
                    pipeline
                }
                Err(e) => {
                    tracing::error!("body worker: pipeline build failed: {e}");
                    let _ = producer.push(BodyWorkerMsg::Error(e));
                    let _ = producer.push(BodyWorkerMsg::Status(BodyTrackingStatus::Failed));
                    return;
                }
            };
            pipeline.set_live_tuning_source(Arc::clone(&tuning));
            // Build the source on this thread (!Send backends are fine). Camera
            // open is retried on a fixed cooldown so a webcam momentarily held
            // by another process — or a kiosk camera still enumerating at boot
            // — does not permanently kill the worker; the audio capture path
            // reacquires a mic the same way. The stop flag is checked between
            // attempts so shutdown still joins promptly. (Pipeline build stays
            // one-shot above: a CoreML recompile is expensive to retry, and a
            // model-load failure signals a missing/corrupt vendored model — a
            // deploy error, not a transient condition.)
            let mut make_source = make_source;
            let Some(source) = open_source_with_retry(
                &mut make_source,
                &stop_thread,
                &mut producer,
                OPEN_RETRY_COOLDOWN,
            ) else {
                // Stop fired during an open-retry cooldown: exit quietly.
                return;
            };
            run_worker_loop(
                &stop_thread,
                source,
                pipeline,
                min_inference_interval,
                &tuning,
                producer,
                recycle,
            );
        });

    let join = match spawn_result {
        Ok(handle) => Some(handle),
        Err(e) => {
            tracing::error!("failed to spawn body worker thread: {e}");
            if let Ok(mut slot) = producer_slot.lock() {
                if let Some(mut producer) = slot.take() {
                    let _ = producer.push(BodyWorkerMsg::Status(BodyTrackingStatus::Failed));
                }
            }
            None
        }
    };

    WorkerHandle { stop, join }
}

/// Open the camera source, retrying on failure with a fixed cooldown until it
/// succeeds or `stop` is set. Mirrors the audio capture path's 2 s reacquire
/// (`audio::input::capture::RETRY_COOLDOWN_S`). Between attempts it surfaces
/// the error string and a `CameraUnavailable` status — a status that persists
/// across the cooldown is the "still retrying" signal to the main thread.
/// Returns `None` only when `stop` fires first, so shutdown joins promptly.
fn open_source_with_retry(
    make_source: &mut SourceFactory,
    stop: &AtomicBool,
    producer: &mut Producer<BodyWorkerMsg>,
    cooldown: Duration,
) -> Option<Box<dyn FrameSource>> {
    loop {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        match make_source() {
            Ok(source) => return Some(source),
            Err(e) => {
                tracing::warn!(
                    "body worker: camera open failed, retrying in {}s: {e}",
                    cooldown.as_secs()
                );
                let _ = producer.push(BodyWorkerMsg::Error(e.to_string()));
                let _ = producer.push(BodyWorkerMsg::Status(BodyTrackingStatus::CameraUnavailable));
                // A stop during the cooldown short-circuits the wait.
                if !sleep_unless_stopped(stop, cooldown) {
                    return None;
                }
            }
        }
    }
}

/// Sleep up to `dur`, waking early and returning `false` the moment `stop` is
/// observed, so a worker parked in its open-retry cooldown still joins within
/// [`RETRY_STOP_POLL`] on shutdown. Returns `true` if the full duration
/// elapsed without a stop.
fn sleep_unless_stopped(stop: &AtomicBool, dur: Duration) -> bool {
    let deadline = Instant::now() + dur;
    loop {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let now = Instant::now();
        if now >= deadline {
            return true;
        }
        std::thread::sleep(RETRY_STOP_POLL.min(deadline - now));
    }
}

/// Cumulative drop counters, split by cause (camera rate-cap drops vs ring
/// backpressure — the same must-not-fold split as the hand worker).
#[derive(Debug, Default)]
struct DropCounters {
    camera: u64,
    ring_full: u64,
}

/// The capture→process→publish loop (worker thread until `stop`).
#[allow(clippy::too_many_arguments, reason = "worker wiring, called once")]
#[allow(
    clippy::too_many_lines,
    reason = "one linear capture->process->publish loop; splitting the budget \
              decision, the success path, and the error path across functions \
              would scatter the DropCounters/spare-payload state each branch \
              shares, hurting readability more than the line count helps"
)]
fn run_worker_loop(
    stop: &AtomicBool,
    mut source: Box<dyn FrameSource>,
    mut pipeline: PosePipeline,
    min_inference_interval: Option<Duration>,
    tuning: &BodyLiveTuning,
    mut producer: Producer<BodyWorkerMsg>,
    mut recycle: Consumer<Box<BodyFramePayload>>,
) {
    let start = Instant::now();
    let mut frame = Frame::default();
    let mut last_inference: Option<Instant> = None;
    let mut drops = DropCounters::default();
    let mut pipeline_errors = 0_u64;
    // The payload currently held by the worker (claimed from the pool,
    // handed off inside a Frame message on success, retained on error).
    let mut spare: Option<Box<BodyFramePayload>> = None;
    let idle_inference_interval = idle_capped_interval(min_inference_interval);
    // Edge-triggered hardware capture throttle (see the hand worker).
    let mut last_throttle: Option<bool> = None;

    if let Some(label) = source.format_label().map(str::to_owned) {
        push_msg(
            &mut producer,
            BodyWorkerMsg::CameraFormat(label),
            &mut drops,
        );
    }
    push_msg(
        &mut producer,
        BodyWorkerMsg::Status(BodyTrackingStatus::Streaming),
        &mut drops,
    );

    while !stop.load(Ordering::Relaxed) {
        let loop_start = Instant::now();
        // Re-read the idle flag every iteration (Relaxed; one-iteration
        // staleness is harmless).
        let idle_throttled = tuning.idle_throttle();
        if last_throttle != Some(idle_throttled) {
            source.set_capture_throttle(idle_throttled);
            last_throttle = Some(idle_throttled);
        }
        let min_interval = if idle_throttled {
            idle_inference_interval
        } else {
            min_inference_interval
        };
        // Budget decision BEFORE capture: over-budget frames drain undecoded
        // (newest frame wins, decode cost skipped — the throttle's thermal win).
        if !should_process_frame(last_inference, loop_start, min_interval) {
            match source.discard_frame() {
                Ok(true) => {
                    drops.camera = drops.camera.saturating_add(1);
                    // Surface the updated drop count immediately (mirrors the
                    // hand worker's per-drop status push): without this, a
                    // consumer never learns a frame was dropped until the
                    // NEXT processed frame's diagnostics — which, under a low
                    // max_hz, may be seconds away.
                    let diag = worker_diag(
                        &pipeline,
                        &drops,
                        Duration::ZERO,
                        Duration::ZERO,
                        pipeline_errors,
                        idle_throttled,
                    );
                    push_msg(&mut producer, BodyWorkerMsg::Diagnostics(diag), &mut drops);
                }
                Ok(false) => {}
                Err(_) => {
                    let _ =
                        producer.push(BodyWorkerMsg::Status(BodyTrackingStatus::CameraUnavailable));
                }
            }
            std::thread::sleep(IDLE_POLL);
            continue;
        }

        match source.next_frame(&mut frame) {
            Ok(true) => {
                let capture_decode = loop_start.elapsed();
                let now = loop_start.duration_since(start);
                let dt =
                    last_inference.map_or(Duration::ZERO, |last| loop_start.duration_since(last));
                last_inference = Some(loop_start);
                // Full frames claim a pooled payload; idle probes never do.
                if !idle_throttled && spare.is_none() {
                    spare = recycle.pop().ok();
                }
                let payload_ref = if idle_throttled {
                    None
                } else {
                    spare.as_deref_mut()
                };
                match pipeline.process(&frame, idle_throttled, payload_ref) {
                    Ok(result) => {
                        let payload = if idle_throttled { None } else { spare.take() };
                        let diag = worker_diag(
                            &pipeline,
                            &drops,
                            capture_decode,
                            dt,
                            pipeline_errors,
                            idle_throttled,
                        );
                        push_msg(&mut producer, BodyWorkerMsg::Diagnostics(diag), &mut drops);
                        // The frame carries a pooled payload; a ring-full drop
                        // must hand it back to `spare` (which `spare.take()`
                        // above just emptied) so the pool — PAYLOAD_POOL_SIZE
                        // boxes, seeded once — is not permanently depleted. A
                        // ~1s main-thread stall would otherwise destroy every
                        // pooled payload and kill mask updates for the rest of
                        // the session (see `push_frame`).
                        if let Some(reclaimed) = push_frame(
                            &mut producer,
                            BodyFrame {
                                present: result.present,
                                confidence: result.confidence,
                                landmarks: result.landmarks,
                                world_landmarks: result.world_landmarks,
                                timestamp: now,
                                payload,
                            },
                            &mut drops,
                        ) {
                            spare = Some(reclaimed);
                        }
                    }
                    Err(e) => {
                        // Count + forward (rare path; the spare payload is
                        // retained for the next frame).
                        pipeline_errors = pipeline_errors.saturating_add(1);
                        push_msg(
                            &mut producer,
                            BodyWorkerMsg::Error(e.to_string()),
                            &mut drops,
                        );
                        let diag = worker_diag(
                            &pipeline,
                            &drops,
                            capture_decode,
                            dt,
                            pipeline_errors,
                            idle_throttled,
                        );
                        push_msg(&mut producer, BodyWorkerMsg::Diagnostics(diag), &mut drops);
                    }
                }
            }
            Ok(false) => {
                std::thread::sleep(IDLE_POLL);
            }
            Err(_) => {
                let _ = producer.push(BodyWorkerMsg::Status(BodyTrackingStatus::CameraUnavailable));
                std::thread::sleep(IDLE_POLL);
            }
        }
    }
}

/// Minimum interval between inference runs for a requested max rate.
fn inference_interval(max_hz: u32) -> Option<Duration> {
    (max_hz > 0).then(|| Duration::from_secs_f64(1.0 / f64::from(max_hz)))
}

/// Minimum interval while the idle throttle is engaged: `max(active, idle)` —
/// the idle cap may only ever *slow* inference (hand-worker semantics).
fn idle_capped_interval(active: Option<Duration>) -> Option<Duration> {
    inference_interval(IDLE_INFERENCE_HZ).map(|idle| active.map_or(idle, |a| a.max(idle)))
}

/// Whether a fresh frame is allowed to run inference now.
fn should_process_frame(
    last_inference: Option<Instant>,
    now: Instant,
    min_interval: Option<Duration>,
) -> bool {
    match (last_inference, min_interval) {
        (_, None) | (None, Some(_)) => true,
        (Some(last), Some(interval)) => now.duration_since(last) >= interval,
    }
}

/// Assemble a diagnostics snapshot (shared by success and error paths).
fn worker_diag(
    pipeline: &PosePipeline,
    drops: &DropCounters,
    capture_decode: Duration,
    inference_interval: Duration,
    pipeline_errors: u64,
    idle_throttled: bool,
) -> BodyWorkerDiagnostics {
    BodyWorkerDiagnostics {
        pipeline: pipeline.diagnostics(),
        dropped_frames: drops.camera,
        ring_full_drops: drops.ring_full,
        capture_decode,
        inference_interval,
        pipeline_errors,
        idle_throttled,
    }
}

/// Push a message, counting a ring-full failure as backpressure (never as a
/// camera drop). Never blocks the worker.
fn push_msg(producer: &mut Producer<BodyWorkerMsg>, msg: BodyWorkerMsg, drops: &mut DropCounters) {
    if producer.push(msg).is_err() {
        drops.ring_full = drops.ring_full.saturating_add(1);
    }
}

/// Push a [`BodyWorkerMsg::Frame`]; on ring-full backpressure, reclaim its
/// pooled payload (if any) and return it so the caller can restore it to the
/// worker's `spare` slot. rtrb hands the rejected value back through
/// [`rtrb::PushError::Full`]; without reclaiming it, each dropped frame would
/// destroy a pooled `Box<BodyFramePayload>` (pool of
/// [`super::transport::PAYLOAD_POOL_SIZE`], seeded once), so a burst of drops —
/// e.g. a ~1s main-thread stall filling the ring — would permanently starve
/// mask updates. Counts the drop as backpressure; never blocks.
fn push_frame(
    producer: &mut Producer<BodyWorkerMsg>,
    frame: BodyFrame,
    drops: &mut DropCounters,
) -> Option<Box<BodyFramePayload>> {
    match producer.push(BodyWorkerMsg::Frame(frame)) {
        Ok(()) => None,
        Err(rtrb::PushError::Full(msg)) => {
            drops.ring_full = drops.ring_full.saturating_add(1);
            match msg {
                BodyWorkerMsg::Frame(mut f) => f.payload.take(),
                // Unreachable: we only ever hand this function a Frame.
                _ => None,
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use std::time::Instant;

    use super::super::pipeline::fixtures::{
        confident_landmark_outputs, empty_detector_outputs, hot_person_detector_outputs,
    };
    use super::super::pipeline::{PoseConfig, PosePipeline};
    use super::super::transport::{seed_payload_pool, PAYLOAD_POOL_SIZE};
    use super::*;
    use crate::input::capture::MockFrameSource;
    use crate::input::onnx::{InferenceError, ModelInference, Tensor};

    #[derive(Clone)]
    struct StaticInference {
        outputs: Vec<Tensor>,
    }

    impl ModelInference for StaticInference {
        fn run(&mut self, _input: &Tensor, out: &mut Vec<Tensor>) -> Result<(), InferenceError> {
            out.clone_from(&self.outputs);
            Ok(())
        }
    }

    struct FailingInference;

    impl ModelInference for FailingInference {
        fn run(&mut self, _input: &Tensor, _out: &mut Vec<Tensor>) -> Result<(), InferenceError> {
            Err(InferenceError::Run("boom".into()))
        }
    }

    fn looping_solid_source() -> SourceFactory {
        Box::new(|| {
            let mut f = crate::input::capture::Frame::default();
            f.fit_to(64, 48);
            let src: Box<dyn crate::input::capture::FrameSource> =
                Box::new(MockFrameSource::looping(vec![f]));
            Ok(src)
        })
    }

    fn person_pipeline_factory() -> PipelineFactory {
        Box::new(|| {
            Ok((
                PosePipeline::new(
                    Box::new(StaticInference {
                        outputs: hot_person_detector_outputs(),
                    }),
                    Box::new(StaticInference {
                        outputs: confident_landmark_outputs(),
                    }),
                    PoseConfig::default(),
                ),
                "mock/backend",
            ))
        })
    }

    fn empty_pipeline_factory() -> PipelineFactory {
        Box::new(|| {
            Ok((
                PosePipeline::new(
                    Box::new(StaticInference {
                        outputs: empty_detector_outputs(),
                    }),
                    Box::new(FailingInference),
                    PoseConfig::default(),
                ),
                "mock/backend",
            ))
        })
    }

    /// Build the rings + tuning a worker needs; returns everything the test
    /// drives.
    #[allow(
        clippy::type_complexity,
        reason = "test-only harness returning the worker's own ring/tuning \
                  types unmodified; a type alias here would only be read by \
                  this one function"
    )]
    fn harness(
        idle: bool,
    ) -> (
        std::sync::Arc<super::super::pipeline::BodyLiveTuning>,
        rtrb::Producer<Box<super::super::transport::BodyFramePayload>>,
        rtrb::Consumer<super::super::transport::BodyWorkerMsg>,
        rtrb::Producer<super::super::transport::BodyWorkerMsg>,
        rtrb::Consumer<Box<super::super::transport::BodyFramePayload>>,
    ) {
        let tuning = std::sync::Arc::new(super::super::pipeline::BodyLiveTuning::new(0.35));
        tuning.set_idle_throttle(idle);
        let (mut recycle_tx, recycle_rx) = rtrb::RingBuffer::new(PAYLOAD_POOL_SIZE + 1);
        seed_payload_pool(&mut recycle_tx);
        let (result_tx, result_rx) = rtrb::RingBuffer::new(64);
        (tuning, recycle_tx, result_rx, result_tx, recycle_rx)
    }

    /// Drain messages until `deadline`, recycling payloads and tallying.
    struct Tally {
        frames: u64,
        person_frames: u64,
        payload_frames: u64,
        backend: Option<&'static str>,
        statuses: Vec<super::super::BodyTrackingStatus>,
        errors: u64,
        max_dropped: u64,
        mask_ptrs: std::collections::HashSet<*const u8>,
    }

    fn drain(
        consumer: &mut rtrb::Consumer<super::super::transport::BodyWorkerMsg>,
        recycle: &mut rtrb::Producer<Box<super::super::transport::BodyFramePayload>>,
        deadline: Instant,
    ) -> Tally {
        use super::super::transport::BodyWorkerMsg;
        let mut t = Tally {
            frames: 0,
            person_frames: 0,
            payload_frames: 0,
            backend: None,
            statuses: Vec::new(),
            errors: 0,
            max_dropped: 0,
            mask_ptrs: std::collections::HashSet::new(),
        };
        while Instant::now() < deadline {
            while let Ok(msg) = consumer.pop() {
                match msg {
                    BodyWorkerMsg::Frame(mut f) => {
                        t.frames += 1;
                        if f.present {
                            t.person_frames += 1;
                        }
                        if let Some(payload) = f.payload.take() {
                            t.payload_frames += 1;
                            t.mask_ptrs.insert(payload.mask.as_ptr());
                            let _ = recycle.push(payload);
                        }
                    }
                    BodyWorkerMsg::Backend(b) => t.backend = Some(b),
                    BodyWorkerMsg::Status(s) => t.statuses.push(s),
                    BodyWorkerMsg::Diagnostics(d) => {
                        t.max_dropped = t.max_dropped.max(d.dropped_frames);
                    }
                    BodyWorkerMsg::Error(_) => t.errors += 1,
                    BodyWorkerMsg::CameraFormat(_) => {}
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        t
    }

    #[test]
    fn worker_streams_person_frames_with_recycled_payloads() {
        let (tuning, mut recycle_tx, mut result_rx, result_tx, recycle_rx) = harness(false);
        let mut handle = spawn_body_worker(
            looping_solid_source(),
            person_pipeline_factory(),
            30,
            tuning,
            result_tx,
            recycle_rx,
        );
        let t = drain(
            &mut result_rx,
            &mut recycle_tx,
            Instant::now() + std::time::Duration::from_millis(600),
        );
        handle.stop();
        assert!(t.person_frames >= 3, "person frames: {}", t.person_frames);
        assert!(
            t.payload_frames >= 3,
            "payload frames: {}",
            t.payload_frames
        );
        assert_eq!(t.backend, Some("mock/backend"));
        assert!(
            t.statuses
                .contains(&super::super::BodyTrackingStatus::Streaming),
            "streaming status never reported: {:?}",
            t.statuses
        );
        assert!(
            t.mask_ptrs.len() <= PAYLOAD_POOL_SIZE,
            "steady state must reuse the pooled buffers, saw {} distinct",
            t.mask_ptrs.len()
        );
    }

    #[test]
    fn worker_honors_max_hz_by_dropping_over_budget_frames() {
        let (tuning, mut recycle_tx, mut result_rx, result_tx, recycle_rx) = harness(false);
        let mut handle = spawn_body_worker(
            looping_solid_source(),
            empty_pipeline_factory(),
            1,
            tuning,
            result_tx,
            recycle_rx,
        );
        let t = drain(
            &mut result_rx,
            &mut recycle_tx,
            Instant::now() + std::time::Duration::from_millis(120),
        );
        handle.stop();
        assert!(
            t.frames <= 1,
            "1 Hz cap processed {} frames in 120 ms",
            t.frames
        );
        assert!(
            t.max_dropped > 0,
            "over-budget frames were not reported dropped"
        );
    }

    #[test]
    fn idle_probe_still_emits_person_bearing_frames() {
        // Wake contract: the idle throttle runs detector-only, and a person
        // seen by the detector must still cross the ring so presence can
        // reset the idle timer. The landmark stage is a FailingInference —
        // if the probe ever invoked it, frames would turn into errors.
        let (tuning, mut recycle_tx, mut result_rx, result_tx, recycle_rx) = harness(true);
        let factory: PipelineFactory = Box::new(|| {
            Ok((
                PosePipeline::new(
                    Box::new(StaticInference {
                        outputs: hot_person_detector_outputs(),
                    }),
                    Box::new(FailingInference),
                    PoseConfig::default(),
                ),
                "mock/backend",
            ))
        });
        let mut handle = spawn_body_worker(
            looping_solid_source(),
            factory,
            30,
            tuning,
            result_tx,
            recycle_rx,
        );
        let t = drain(
            &mut result_rx,
            &mut recycle_tx,
            Instant::now() + std::time::Duration::from_millis(600),
        );
        handle.stop();
        assert!(t.person_frames >= 1, "idle probe never emitted presence");
        assert_eq!(t.errors, 0, "landmark stage must not run while idle");
        assert_eq!(t.payload_frames, 0, "idle probes must not claim payloads");
    }

    #[test]
    fn pipeline_factory_failure_reports_failed_status() {
        let (tuning, mut recycle_tx, mut result_rx, result_tx, recycle_rx) = harness(false);
        let factory: PipelineFactory = Box::new(|| Err("no models".into()));
        let mut handle = spawn_body_worker(
            looping_solid_source(),
            factory,
            30,
            tuning,
            result_tx,
            recycle_rx,
        );
        let t = drain(
            &mut result_rx,
            &mut recycle_tx,
            Instant::now() + std::time::Duration::from_millis(200),
        );
        handle.stop();
        assert!(
            t.statuses
                .contains(&super::super::BodyTrackingStatus::Failed),
            "factory failure must surface as Failed: {:?}",
            t.statuses
        );
        assert!(t.errors >= 1, "the error string must cross the ring");
    }

    #[test]
    fn open_source_retries_until_success() {
        // A camera that fails its first two open attempts then succeeds must
        // not kill the worker: the retry loop reattempts on the cooldown and
        // surfaces a CameraUnavailable status per failure (the "still trying"
        // signal), then hands back the source.
        use super::super::transport::BodyWorkerMsg;
        use std::sync::atomic::AtomicU32;

        let attempts = Arc::new(AtomicU32::new(0));
        let a = Arc::clone(&attempts);
        let mut factory: SourceFactory = Box::new(move || {
            let n = a.fetch_add(1, Ordering::Relaxed);
            if n < 2 {
                Err(CaptureError::NoCamera("not yet".into()))
            } else {
                let mut f = crate::input::capture::Frame::default();
                f.fit_to(64, 48);
                let src: Box<dyn FrameSource> = Box::new(MockFrameSource::looping(vec![f]));
                Ok(src)
            }
        });
        let stop = AtomicBool::new(false);
        let (mut producer, mut consumer) = rtrb::RingBuffer::<BodyWorkerMsg>::new(64);
        let source =
            open_source_with_retry(&mut factory, &stop, &mut producer, Duration::from_millis(5));
        assert!(source.is_some(), "retry must eventually open the camera");
        assert_eq!(
            attempts.load(Ordering::Relaxed),
            3,
            "two failures then one success"
        );
        let mut unavailable = 0;
        while let Ok(msg) = consumer.pop() {
            if matches!(
                msg,
                BodyWorkerMsg::Status(BodyTrackingStatus::CameraUnavailable)
            ) {
                unavailable += 1;
            }
        }
        assert_eq!(
            unavailable, 2,
            "one CameraUnavailable status per failed open"
        );
    }

    #[test]
    fn stop_short_circuits_open_retry_cooldown() {
        // With stop already set, the retry loop must return without sleeping
        // the (deliberately long) cooldown — the mechanism that keeps shutdown
        // prompt while a worker is parked waiting to reacquire a camera.
        use super::super::transport::BodyWorkerMsg;

        let mut factory: SourceFactory = Box::new(|| Err(CaptureError::NoCamera("nope".into())));
        let stop = AtomicBool::new(true);
        let (mut producer, _consumer) = rtrb::RingBuffer::<BodyWorkerMsg>::new(8);
        let start = Instant::now();
        let source =
            open_source_with_retry(&mut factory, &stop, &mut producer, Duration::from_mins(1));
        assert!(source.is_none(), "a set stop flag must abandon the open");
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "stop must short-circuit the cooldown, took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn stop_during_camera_open_retry_joins_promptly() {
        // End-to-end: a camera that never opens leaves the worker parked in the
        // real OPEN_RETRY_COOLDOWN; stopping it must still join within a small
        // multiple of RETRY_STOP_POLL, not after the full 2 s cooldown.
        let (tuning, _recycle_tx, _result_rx, result_tx, recycle_rx) = harness(false);
        let failing_source: SourceFactory =
            Box::new(|| Err(CaptureError::NoCamera("no camera".into())));
        let mut handle = spawn_body_worker(
            failing_source,
            person_pipeline_factory(),
            30,
            tuning,
            result_tx,
            recycle_rx,
        );
        // Let it build the (mock) pipeline and reach the open-retry cooldown.
        std::thread::sleep(Duration::from_millis(80));
        let start = Instant::now();
        handle.stop();
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "stop during the open-retry cooldown must join promptly, took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn ring_full_frame_drop_reclaims_pooled_payload() {
        // Regression: a dropped Frame must NOT destroy its pooled payload.
        // rtrb returns the rejected value on backpressure; `push_frame` hands
        // the pooled `Box<BodyFramePayload>` back so the pool (seeded once)
        // survives a full ring. Pointer identity proves it's the same buffer.
        use super::super::transport::{BodyFrame, BodyFramePayload, BodyWorkerMsg};
        use super::super::{BodyLandmark, BodyTrackingStatus, BODY_LANDMARK_COUNT};
        use bevy::math::Vec3;

        // A one-slot ring, pre-filled, so the next push is guaranteed full.
        let (mut producer, _consumer) = rtrb::RingBuffer::<BodyWorkerMsg>::new(1);
        producer
            .push(BodyWorkerMsg::Status(BodyTrackingStatus::Streaming))
            .expect("first push fills the sole slot");

        let payload = Box::new(BodyFramePayload::new());
        let payload_ptr = payload.mask.as_ptr();
        let mut drops = DropCounters::default();
        let reclaimed = push_frame(
            &mut producer,
            BodyFrame {
                present: true,
                confidence: 1.0,
                landmarks: [BodyLandmark::default(); BODY_LANDMARK_COUNT],
                world_landmarks: [Vec3::ZERO; BODY_LANDMARK_COUNT],
                timestamp: std::time::Duration::ZERO,
                payload: Some(payload),
            },
            &mut drops,
        );

        let reclaimed = reclaimed.expect("ring-full Frame drop must hand its payload back");
        assert_eq!(
            reclaimed.mask.as_ptr(),
            payload_ptr,
            "the reclaimed payload must be the very buffer the dropped frame held"
        );
        assert_eq!(
            drops.ring_full, 1,
            "the drop is counted as ring backpressure"
        );
        assert_eq!(drops.camera, 0, "a backpressure drop is not a camera drop");
    }

    #[test]
    fn successful_frame_push_keeps_payload_in_flight() {
        // The mirror of the reclaim test: a push that fits must NOT return the
        // payload to the caller (it now rides the ring toward the consumer).
        use super::super::transport::{BodyFrame, BodyFramePayload, BodyWorkerMsg};
        use super::super::{BodyLandmark, BODY_LANDMARK_COUNT};
        use bevy::math::Vec3;

        let (mut producer, _consumer) = rtrb::RingBuffer::<BodyWorkerMsg>::new(2);
        let mut drops = DropCounters::default();
        let reclaimed = push_frame(
            &mut producer,
            BodyFrame {
                present: true,
                confidence: 1.0,
                landmarks: [BodyLandmark::default(); BODY_LANDMARK_COUNT],
                world_landmarks: [Vec3::ZERO; BODY_LANDMARK_COUNT],
                timestamp: std::time::Duration::ZERO,
                payload: Some(Box::new(BodyFramePayload::new())),
            },
            &mut drops,
        );
        assert!(
            reclaimed.is_none(),
            "a fitting push keeps the payload in flight"
        );
        assert_eq!(drops.ring_full, 0, "no backpressure when the ring has room");
    }

    #[test]
    fn pipeline_errors_are_counted_and_surfaced() {
        let (tuning, mut recycle_tx, mut result_rx, result_tx, recycle_rx) = harness(false);
        let factory: PipelineFactory = Box::new(|| {
            Ok((
                PosePipeline::new(
                    Box::new(FailingInference),
                    Box::new(FailingInference),
                    PoseConfig::default(),
                ),
                "mock/backend",
            ))
        });
        let mut handle = spawn_body_worker(
            looping_solid_source(),
            factory,
            30,
            tuning,
            result_tx,
            recycle_rx,
        );
        let t = drain(
            &mut result_rx,
            &mut recycle_tx,
            Instant::now() + std::time::Duration::from_millis(200),
        );
        handle.stop();
        assert!(t.errors >= 1, "pipeline error not surfaced");
    }
}
