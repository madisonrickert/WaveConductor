//! Background worker that runs the [`super::pipeline::Pipeline`] off the Bevy
//! main thread and publishes results over a lock-free `rtrb` ring.
//!
//! The worker owns the camera ([`super::capture::FrameSource`]) and both ONNX
//! sessions; it captures, runs the two-stage pipeline at a capped rate, and
//! pushes [`WorkerMsg`]s to the provider. The provider's `poll` drains the ring
//! on the main thread without blocking or allocating — mirroring how the Leap
//! provider keeps device I/O off the render path.
//!
//! Two rate caps share one mechanism (drop over-budget frames undecoded):
//! the configured `max_hz` always applies, and while the app sits in
//! `Idle`/`Screensaver` the shared [`MediaPipeLiveTuning`] idle-throttle flag
//! lowers the cap to [`IDLE_INFERENCE_HZ`] to shed sustained CPU/thermal load
//! (see the constant's wake-latency contract).
//!
//! Foundation module: wired into the provider in [`super::MediaPipeProvider`];
//! exercised by a mock-source plumbing test.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rtrb::Producer;
use smallvec::SmallVec;

use super::capture::{CaptureError, Frame, FrameSource};
use super::pipeline::Pipeline;
use super::pipeline::{MediaPipeLiveTuning, PipelineDiagnostics};
use crate::input::hand::Hand;
use crate::input::state::{
    DeviceHealth, DevicePresence, ProviderStatus, ServiceConnection, TrackingFlow, MAX_HANDS,
};

/// Idle backoff when the source has no frame ready (or errored), so a
/// non-blocking source can't busy-spin a core. A real (blocking) camera read
/// rarely hits this path; it mainly guards the test/mock sources.
const IDLE_POLL: Duration = Duration::from_millis(2);

/// Creates the frame source on the worker thread. Deferring construction lets
/// `!Send` camera backends (e.g. `nokhwa`'s `AVFoundation` camera) be built
/// where they are used; the factory itself is `Send` (it captures only the
/// camera index or a `Send` mock).
pub type SourceFactory = Box<dyn FnOnce() -> Result<Box<dyn FrameSource>, CaptureError> + Send>;

/// A message from the worker to the provider.
// The `Hands` payload (up to two hands × 21 landmarks) dwarfs `Status`; boxing
// it would add a per-frame heap allocation on the worker for a ring that is
// only 256 entries deep, so the size asymmetry is the better trade here.
#[allow(clippy::large_enum_variant)]
pub enum WorkerMsg {
    /// Hands from one processed frame, with the worker-clock capture time.
    Hands {
        /// The tracked hands (empty when none are in view).
        hands: SmallVec<[Hand; MAX_HANDS]>,
        /// Worker-relative capture timestamp.
        timestamp: Duration,
    },
    /// A status update (camera/streaming lifecycle).
    Status(ProviderStatus),
    /// Pipeline diagnostics for the most recently processed frame.
    Diagnostics(MediaPipeWorkerDiagnostics),
    /// A pipeline error string. Sent only on the (rare) error path, so the
    /// `String` allocation never touches the steady-state frame loop.
    Error(String),
    /// The negotiated camera format label, sent once when the source opens.
    CameraFormat(String),
}

/// Worker/pipeline diagnostics sent to the provider.
#[derive(Debug, Clone, Copy, Default)]
pub struct MediaPipeWorkerDiagnostics {
    /// Pipeline-stage metrics.
    pub pipeline: PipelineDiagnostics,
    /// Cumulative *camera*-frame drops since worker start: over-budget frames
    /// discarded undecoded by the rate cap or idle throttle (see
    /// [`drop_over_budget_frame`]). Mirrors `TrackingFlow::Streaming::dropped_since_start`
    /// in the worker's status messages. Deliberately excludes ring-buffer
    /// backpressure (a `WorkerMsg` push failing because the provider's `poll`
    /// has not drained fast enough) — see [`Self::ring_full_drops`], which
    /// that distinct failure mode is reported under.
    pub dropped_frames: u64,
    /// Cumulative ring-buffer backpressure drops since worker start: a
    /// [`WorkerMsg`] failed to `push` because the 256-entry `rtrb` ring was
    /// full (see [`push_msg`]). This is a slow-consumer symptom (the main
    /// thread's `poll` is not draining fast enough), not a camera problem, so
    /// it is counted separately from [`Self::dropped_frames`] rather than
    /// inflating it — folding the two together would misattribute backpressure
    /// as camera drops in diagnostics.
    pub ring_full_drops: u64,
    /// Wall time spent acquiring + decoding the frame that was just processed.
    /// Separates a slow camera/decode from slow inference on hardware.
    pub capture_decode: Duration,
    /// Wall time since the previous processed frame: the effective inference
    /// period (its inverse is the achieved inference rate, distinct from the
    /// camera's capture rate).
    pub inference_interval: Duration,
    /// Cumulative count of pipeline (inference) errors since worker start.
    pub pipeline_errors: u64,
    /// Whether the idle throttle ([`IDLE_INFERENCE_HZ`]) was *requested* for
    /// this frame (app is in `Idle`/`Screensaver`). Surfaced as the dev
    /// panel's "Idle throttle" metric ("requested" / "off"). When `true`, the
    /// effective cap is `max(configured max_hz interval, IDLE_INFERENCE_HZ
    /// interval)` — if `max_hz` is already slower than [`IDLE_INFERENCE_HZ`],
    /// the configured rate stays authoritative; the actual inference period is
    /// always visible via [`Self::inference_interval`].
    pub idle_throttled: bool,
}

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

/// Inference cap (Hz) applied while [`MediaPipeLiveTuning::idle_throttle`] is
/// set, i.e. the sketch is in `Idle`/`Screensaver` with no audience present.
///
/// Wake-latency contract: 4 Hz means a worst-case wake of one throttle period
/// (250 ms) plus one full pipeline run (tens of ms) ≈ **300 ms** before the
/// first hand-bearing frame reaches `lifecycle::idle::reset_on_interaction`
/// (which wakes only on hand-*bearing* frames — empty frames never reset the
/// idle timer) and flips the app back to `Active`/full rate. Against the 30 s
/// idle threshold that entry latency is imperceptible, while the sustained
/// load drop (30 Hz → 4 Hz of capture-decode + palm inference) is the bulk of
/// the multi-hour idle thermal win.
///
/// On backends that honor [`FrameSource::set_capture_throttle`] (macOS
/// `AVFoundation`), the *camera* drops to this same rate while idle, so the
/// freshest frame is at most one period (250 ms) old when processed — the
/// identical staleness bound the inference cap already imposes. No added wake
/// latency; the sensor/ISP simply do less work.
pub const IDLE_INFERENCE_HZ: u32 = 4;

/// Spawn the worker thread. It runs until [`WorkerHandle::stop`] (or the handle
/// drops), capturing + processing frames at up to `max_hz` and pushing results
/// to `producer`. `tuning` is the provider's shared lock-free cell; the loop
/// re-reads its idle-throttle flag every iteration and drops to
/// [`IDLE_INFERENCE_HZ`] while it is set.
///
/// If the OS itself fails to create the thread (e.g. the process is out of
/// thread resources), that failure is not silently swallowed: it is logged at
/// `error!` and reported to the provider the same way a camera failure is —
/// pushing a [`no_camera_status`] [`WorkerMsg::Status`] onto `producer` — so
/// `MediaPipeProvider::poll` surfaces it instead of leaving the provider's
/// status frozen at whatever it was before `start()` forever.
#[must_use]
pub fn spawn_worker(
    make_source: SourceFactory,
    pipeline: Pipeline,
    max_hz: u32,
    tuning: Arc<MediaPipeLiveTuning>,
    producer: Producer<WorkerMsg>,
) -> WorkerHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    // The processing rate is capped by dropping over-budget captured frames, not
    // by sleeping after inference. The old sleep-based cap let the camera buffer
    // fill while we slept, so we processed ever-staler frames. This keeps the
    // single-thread invariant MediaPipe's FlowLimiter gives us: newest frame wins.
    let min_inference_interval = inference_interval(max_hz);

    // `producer` must move into the spawned closure for the success path, but
    // `std::thread::Builder::spawn` drops the closure — and everything it
    // captured — without handing it back when thread creation fails, so a bare
    // move would silently lose the ring producer right alongside the error.
    // Routing it through a shared slot lets the failure branch below reclaim
    // it: the closure never got to run, so the slot still holds it.
    let producer_slot = Arc::new(Mutex::new(Some(producer)));
    let producer_for_thread = Arc::clone(&producer_slot);

    let spawn_result = std::thread::Builder::new()
        .name("wc-mediapipe-worker".into())
        .spawn(move || {
            let Some(mut producer) = producer_for_thread
                .lock()
                .ok()
                .and_then(|mut slot| slot.take())
            else {
                // Unreachable in practice: this closure is the only code that
                // ever runs while the slot can still be non-empty (the failure
                // branch below only fires when this closure never started).
                // Guarded rather than unwrapped so a future refactor can't
                // turn a logic error into a worker-thread panic.
                return;
            };
            // Build the source on this thread (so !Send backends are fine).
            let Ok(source) = make_source() else {
                let _ = producer.push(WorkerMsg::Status(no_camera_status()));
                return;
            };
            run_worker_loop(
                &stop_thread,
                source,
                pipeline,
                min_inference_interval,
                &tuning,
                producer,
            );
        });

    let join = match spawn_result {
        Ok(handle) => Some(handle),
        Err(e) => {
            tracing::error!("failed to spawn MediaPipe worker thread: {e}");
            // The closure above never ran, so the slot still holds the
            // producer: reclaim it and report the failure the same way the
            // running loop reports a camera failure, so the provider's status
            // does not stay frozen at "Connecting" forever.
            if let Ok(mut slot) = producer_slot.lock() {
                if let Some(mut producer) = slot.take() {
                    let _ = producer.push(WorkerMsg::Status(no_camera_status()));
                }
            }
            None
        }
    };

    WorkerHandle { stop, join }
}

/// Cumulative worker-loop drop counters, split by WHY a frame or message never
/// reached the provider.
///
/// [`Self::camera`] and [`Self::ring_full`] are distinct failure modes that
/// must not be folded into one number: a camera drop means the worker chose
/// not to process a frame (rate cap or idle throttle, working as intended); a
/// ring-full drop means a [`WorkerMsg`] the worker DID produce never reached
/// the provider because the main thread's `poll` has not drained the 256-entry
/// `rtrb` ring fast enough (backpressure — a symptom of a slow consumer, not
/// the camera). Reported on [`MediaPipeWorkerDiagnostics`] as
/// [`MediaPipeWorkerDiagnostics::dropped_frames`] and
/// [`MediaPipeWorkerDiagnostics::ring_full_drops`] respectively.
#[derive(Debug, Default)]
struct DropCounters {
    /// Real camera-frame drops (see [`drop_over_budget_frame`]).
    camera: u64,
    /// Ring-buffer backpressure drops (see [`push_msg`]).
    ring_full: u64,
}

/// The worker's capture→process→publish loop, run on the worker thread until
/// `stop` is set. Split out of [`spawn_worker`] so each stays readable.
/// Decides the frame budget *first* (re-reading the idle-throttle flag every
/// iteration), drains over-budget frames undecoded via
/// [`FrameSource::discard_frame`] (thermal cap, newest frame wins), captures +
/// decodes only budgeted frames, runs the pipeline, and publishes hands,
/// status, and diagnostics (including the rare error path) over the ring.
fn run_worker_loop(
    stop: &AtomicBool,
    mut source: Box<dyn FrameSource>,
    mut pipeline: Pipeline,
    min_inference_interval: Option<Duration>,
    tuning: &MediaPipeLiveTuning,
    mut producer: Producer<WorkerMsg>,
) {
    let start = Instant::now();
    let mut frame = Frame::default();
    let mut last_inference = None;
    let mut drops = DropCounters::default();
    let mut pipeline_errors = 0_u64;
    // Computed once: the idle cap can only ever *lower* the rate (a configured
    // active cap slower than IDLE_INFERENCE_HZ stays authoritative).
    let idle_inference_interval = idle_capped_interval(min_inference_interval);
    // Edge-triggered hardware-throttle dispatch: tell the source when the idle
    // flag flips so a capable backend (macOS AVFoundation) drops its hardware
    // capture rate. `None` forces a sync call on the first iteration.
    let mut last_throttle: Option<bool> = None;
    announce_source(source.as_ref(), &mut producer, &mut drops);

    while !stop.load(Ordering::Relaxed) {
        let loop_start = Instant::now();
        // Re-read the idle flag every iteration: the Bevy-side mirror system
        // flips it on Idle/Screensaver transitions and the very next frame
        // budget honors the new rate (lock-free, Relaxed — a one-iteration
        // stale read is harmless).
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
        // Budget decision BEFORE capture: an over-budget frame is drained
        // undecoded (discard_frame), so a throttled worker never pays the
        // MJPEG/YUYV→RGB decode — the dominant per-dropped-frame CPU cost and
        // most of the idle throttle's thermal win — while the drain itself
        // keeps the camera stream fresh (newest frame wins; sleeping instead
        // would let the buffer fill with stale frames, see spawn_worker).
        if !should_process_frame(last_inference, loop_start, min_interval) {
            drop_over_budget_frame(source.as_mut(), &mut producer, &mut drops);
            // Fast mocks and some non-blocking camera sources can return
            // frames immediately; after dropping one, back off briefly so
            // the cap doesn't busy-spin a core.
            std::thread::sleep(IDLE_POLL);
            continue;
        }

        let next = source.next_frame(&mut frame);
        // Time from the budget decision to the frame being decoded in `frame`.
        let capture_decode = loop_start.elapsed();
        match next {
            Ok(true) => {
                // A budgeted frame is in hand: process it immediately. No retained
                // frame ages while we wait for the next budget window; all
                // over-budget frames have already been dropped.
                let now = loop_start.duration_since(start);
                let dt =
                    last_inference.map_or(Duration::ZERO, |last| loop_start.duration_since(last));
                last_inference = Some(loop_start);
                match pipeline.process(&frame, dt) {
                    Ok(hands) => {
                        let diag = worker_diag(
                            &pipeline,
                            &drops,
                            capture_decode,
                            dt,
                            pipeline_errors,
                            idle_throttled,
                        );
                        push_msg(&mut producer, WorkerMsg::Diagnostics(diag), &mut drops);
                        push_msg(
                            &mut producer,
                            WorkerMsg::Hands {
                                hands,
                                timestamp: now,
                            },
                            &mut drops,
                        );
                        // last_frame_ago = the inter-frame interval, so the dev
                        // panel shows the achieved cadence rather than 0.
                        push_msg(
                            &mut producer,
                            WorkerMsg::Status(streaming_status(dt, drops.camera)),
                            &mut drops,
                        );
                    }
                    Err(e) => {
                        // Surface the error rather than silently dropping the
                        // frame; count it and forward the (rare) string.
                        pipeline_errors = pipeline_errors.saturating_add(1);
                        push_msg(&mut producer, WorkerMsg::Error(e.to_string()), &mut drops);
                        let diag = worker_diag(
                            &pipeline,
                            &drops,
                            capture_decode,
                            dt,
                            pipeline_errors,
                            idle_throttled,
                        );
                        push_msg(&mut producer, WorkerMsg::Diagnostics(diag), &mut drops);
                    }
                }
            }
            Ok(false) => {
                // No frame ready (e.g. an exhausted mock): brief sleep so a
                // non-blocking source can't busy-spin a core.
                std::thread::sleep(IDLE_POLL);
            }
            Err(_) => {
                let _ = producer.push(WorkerMsg::Status(no_camera_status()));
                std::thread::sleep(IDLE_POLL);
            }
        }
    }
}

/// One-time start-of-loop announcements: the negotiated capture format (when
/// the source knows one) and the initial healthy streaming status.
fn announce_source(
    source: &dyn FrameSource,
    producer: &mut Producer<WorkerMsg>,
    drops: &mut DropCounters,
) {
    if let Some(label) = source.format_label().map(str::to_owned) {
        push_msg(producer, WorkerMsg::CameraFormat(label), drops);
    }
    push_msg(
        producer,
        WorkerMsg::Status(streaming_status(Duration::ZERO, 0)),
        drops,
    );
}

/// Drain one over-budget frame **undecoded** ([`FrameSource::discard_frame`])
/// and report the drop. Called when the rate cap (configured `max_hz` or the
/// idle throttle) rejects the current budget window: the drain keeps the
/// camera stream fresh (newest frame wins) while skipping the decode — the
/// dominant per-dropped-frame CPU cost.
fn drop_over_budget_frame(
    source: &mut dyn FrameSource,
    producer: &mut Producer<WorkerMsg>,
    drops: &mut DropCounters,
) {
    match source.discard_frame() {
        Ok(true) => {
            drops.camera = drops.camera.saturating_add(1);
            push_msg(
                producer,
                WorkerMsg::Status(streaming_status(Duration::ZERO, drops.camera)),
                drops,
            );
        }
        // No frame was waiting: nothing to drop (the caller backs off).
        Ok(false) => {}
        Err(_) => {
            let _ = producer.push(WorkerMsg::Status(no_camera_status()));
        }
    }
}

/// Minimum interval between inference runs for a requested max rate.
fn inference_interval(max_hz: u32) -> Option<Duration> {
    (max_hz > 0).then(|| Duration::from_secs_f64(1.0 / f64::from(max_hz)))
}

/// Minimum inference interval while the idle throttle is engaged.
///
/// `max(active interval, idle interval)`: the [`IDLE_INFERENCE_HZ`] cap may
/// only *slow* inference. With an uncapped active rate (`max_hz == 0` →
/// `active == None`) the idle interval applies alone; a configured active cap
/// already slower than the idle rate stays authoritative.
fn idle_capped_interval(active: Option<Duration>) -> Option<Duration> {
    // IDLE_INFERENCE_HZ is a non-zero constant, so this is always Some; the
    // Option shape is kept to compose with `should_process_frame`.
    inference_interval(IDLE_INFERENCE_HZ).map(|idle| active.map_or(idle, |active| active.max(idle)))
}

/// Whether a freshly captured frame is allowed to run inference now.
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

/// Assemble a diagnostics snapshot for one processed frame (shared by the
/// success and error paths so the field set stays in lockstep).
fn worker_diag(
    pipeline: &Pipeline,
    drops: &DropCounters,
    capture_decode: Duration,
    inference_interval: Duration,
    pipeline_errors: u64,
    idle_throttled: bool,
) -> MediaPipeWorkerDiagnostics {
    MediaPipeWorkerDiagnostics {
        pipeline: pipeline.diagnostics(),
        dropped_frames: drops.camera,
        ring_full_drops: drops.ring_full,
        capture_decode,
        inference_interval,
        pipeline_errors,
        idle_throttled,
    }
}

/// Push a message, counting a ring-full push failure as backpressure
/// ([`DropCounters::ring_full`]) — never as a camera drop
/// ([`DropCounters::camera`]), which only [`drop_over_budget_frame`] touches.
/// Never blocks the worker.
fn push_msg(producer: &mut Producer<WorkerMsg>, msg: WorkerMsg, drops: &mut DropCounters) {
    if producer.push(msg).is_err() {
        drops.ring_full = drops.ring_full.saturating_add(1);
    }
}

/// Healthy streaming status for the webcam provider.
fn streaming_status(last_frame_ago: Duration, dropped: u64) -> ProviderStatus {
    ProviderStatus {
        service: ServiceConnection::Connected,
        device: DevicePresence::Attached,
        health: DeviceHealth::STREAMING,
        streaming: TrackingFlow::Streaming {
            last_frame_ago,
            dropped_since_start: dropped,
        },
        ..ProviderStatus::default()
    }
}

/// Status when the camera read fails / no device.
fn no_camera_status() -> ProviderStatus {
    ProviderStatus {
        service: ServiceConnection::Errored,
        device: DevicePresence::Failed,
        health: DeviceHealth::BAD_TRANSPORT,
        streaming: TrackingFlow::NotStreaming,
        ..ProviderStatus::default()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::super::capture::MockFrameSource;
    use super::super::inference::{HandInference, InferenceError, Tensor};
    use super::super::pipeline::fixtures as pipeline_fixtures;
    use super::super::pipeline::PipelineConfig;
    use super::*;
    use crate::input::state::{PrimaryState, TrackingFlow};

    #[derive(Clone)]
    struct StaticInference {
        outputs: Vec<Tensor>,
    }

    impl HandInference for StaticInference {
        fn run(&mut self, _input: &Tensor, out: &mut Vec<Tensor>) -> Result<(), InferenceError> {
            out.clone_from(&self.outputs);
            Ok(())
        }
    }

    fn empty_pipeline() -> Pipeline {
        let palm = StaticInference {
            outputs: vec![
                Tensor::zeros(vec![1, 2016, 18]),
                Tensor {
                    data: vec![-100.0; 2016],
                    shape: vec![1, 2016, 1],
                },
            ],
        };
        let landmark = StaticInference {
            outputs: vec![
                Tensor::zeros(vec![1, 63]),
                Tensor::zeros(vec![1, 1]), // presence 0 → no hand kept
                Tensor::zeros(vec![1, 1]), // handedness 0 → reads Left (moot: presence drops it)
                // Plausible open-hand WORLD landmarks, never zeros: gesture
                // signals divide by the world hand_scale, so a degenerate
                // all-zeros world hand would read as epsilon-scale garbage if
                // this mock's output ever reached the signal path.
                Tensor {
                    data: super::super::pipeline::fixtures::open_world_tensor(),
                    shape: vec![1, 63],
                },
            ],
        };
        Pipeline::new(
            Box::new(palm),
            Box::new(landmark),
            PipelineConfig::default(),
        )
    }

    fn looping_solid_source() -> SourceFactory {
        // Looping solid frames → worker keeps producing (0 hands, healthy status).
        Box::new(|| {
            let mut f = Frame::default();
            f.fit_to(64, 48);
            let src: Box<dyn FrameSource> = Box::new(MockFrameSource::looping(vec![f]));
            Ok(src)
        })
    }

    /// A shared tuning cell with the idle-throttle flag pre-set.
    fn tuning(idle: bool) -> Arc<MediaPipeLiveTuning> {
        let cfg = PipelineConfig::default();
        let cell = Arc::new(MediaPipeLiveTuning::new(
            cfg.grab_rest_deadzone,
            cfg.depth_calibration_k,
        ));
        cell.set_idle_throttle(idle);
        cell
    }

    /// A pipeline whose mocks emit ONE confident hand on every processed frame.
    /// Uses the shared [`super::pipeline::fixtures`] for the palm and landmark
    /// mock outputs: one hot central palm anchor → a 0.2×0.2 detection; the
    /// landmark stage returns well-spread image landmarks with presence 0.98.
    /// Used to pin the wake contract: a throttled frame that sees a hand must
    /// still emit a hand-bearing `WorkerMsg::Hands`.
    fn hand_pipeline() -> Pipeline {
        let palm = StaticInference {
            outputs: pipeline_fixtures::hot_anchor_palm_outputs(),
        };
        let landmark = StaticInference {
            outputs: pipeline_fixtures::confident_spread_landmark_outputs(),
        };
        Pipeline::new(
            Box::new(palm),
            Box::new(landmark),
            PipelineConfig::default(),
        )
    }

    #[test]
    fn worker_publishes_status_and_frames_for_a_mock_source() {
        let pipeline = empty_pipeline();
        let make_source = looping_solid_source();
        let (producer, mut consumer) = rtrb::RingBuffer::<WorkerMsg>::new(64);
        let mut handle = spawn_worker(make_source, pipeline, 30, tuning(false), producer);

        // Spin briefly until we see a healthy streaming status.
        let mut saw_streaming = false;
        for _ in 0..200 {
            while let Ok(msg) = consumer.pop() {
                if let WorkerMsg::Status(s) = msg {
                    if s.primary() == PrimaryState::Streaming {
                        saw_streaming = true;
                    }
                }
            }
            if saw_streaming {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        handle.stop();
        assert!(saw_streaming, "worker never reported a streaming status");
    }

    #[test]
    fn worker_honors_max_hz_by_dropping_over_budget_frames() {
        let pipeline = empty_pipeline();
        let make_source = looping_solid_source();
        let (producer, mut consumer) = rtrb::RingBuffer::<WorkerMsg>::new(64);
        let mut handle = spawn_worker(make_source, pipeline, 1, tuning(false), producer);

        let deadline = Instant::now() + Duration::from_millis(120);
        let mut hand_messages = 0_u64;
        let mut max_dropped = 0_u64;
        while Instant::now() < deadline {
            while let Ok(msg) = consumer.pop() {
                match msg {
                    WorkerMsg::Hands { .. } => {
                        hand_messages = hand_messages.saturating_add(1);
                    }
                    WorkerMsg::Status(status) => {
                        if let TrackingFlow::Streaming {
                            dropped_since_start,
                            ..
                        } = status.streaming
                        {
                            max_dropped = max_dropped.max(dropped_since_start);
                        }
                    }
                    WorkerMsg::Diagnostics(_)
                    | WorkerMsg::Error(_)
                    | WorkerMsg::CameraFormat(_) => {}
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        handle.stop();

        assert!(
            hand_messages <= 1,
            "1 Hz cap processed {hand_messages} inference frames in 120 ms"
        );
        assert!(
            max_dropped > 0,
            "over-budget fresh frames were not reported as dropped"
        );
    }

    /// Drain `consumer` until `deadline`, counting hand messages and the
    /// highest dropped-frame count reported in streaming statuses.
    fn drain_until(consumer: &mut rtrb::Consumer<WorkerMsg>, deadline: Instant) -> (u64, u64) {
        let mut hand_messages = 0_u64;
        let mut max_dropped = 0_u64;
        while Instant::now() < deadline {
            while let Ok(msg) = consumer.pop() {
                match msg {
                    WorkerMsg::Hands { .. } => {
                        hand_messages = hand_messages.saturating_add(1);
                    }
                    WorkerMsg::Status(status) => {
                        if let TrackingFlow::Streaming {
                            dropped_since_start,
                            ..
                        } = status.streaming
                        {
                            max_dropped = max_dropped.max(dropped_since_start);
                        }
                    }
                    WorkerMsg::Diagnostics(_)
                    | WorkerMsg::Error(_)
                    | WorkerMsg::CameraFormat(_) => {}
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        (hand_messages, max_dropped)
    }

    #[test]
    fn idle_capped_interval_only_ever_slows_inference() {
        // 4 Hz ↔ 250 ms period.
        let idle_period = Duration::from_millis(250);
        // Typical: 30 Hz active cap (~33 ms) → the idle period wins.
        assert_eq!(
            idle_capped_interval(inference_interval(30)),
            Some(idle_period)
        );
        // Active cap already slower than idle (1 Hz) → it stays authoritative.
        assert_eq!(
            idle_capped_interval(inference_interval(1)),
            Some(Duration::from_secs(1))
        );
        // Uncapped active rate (max_hz = 0) → idle period applies alone.
        assert_eq!(idle_capped_interval(None), Some(idle_period));
    }

    #[test]
    fn push_msg_counts_ring_full_separately_from_camera_drops() {
        // T16(b) regression: a ring-full push (backpressure — the provider's
        // `poll` not draining fast enough) must NOT inflate the same counter
        // `streaming_status` reports as camera drops (`DropCounters::camera`,
        // only ever touched by `drop_over_budget_frame`). Capacity 1 so the
        // second push is guaranteed to find the ring full.
        let (mut producer, _consumer) = rtrb::RingBuffer::<WorkerMsg>::new(1);
        let mut drops = DropCounters::default();

        // First push succeeds (fills the one slot).
        push_msg(
            &mut producer,
            WorkerMsg::Status(no_camera_status()),
            &mut drops,
        );
        assert_eq!(drops.ring_full, 0, "the first push had room");
        assert_eq!(
            drops.camera, 0,
            "push_msg must never touch the camera counter"
        );

        // Second push finds the ring full (nothing drained it).
        push_msg(
            &mut producer,
            WorkerMsg::Status(no_camera_status()),
            &mut drops,
        );
        assert_eq!(
            drops.ring_full, 1,
            "a ring-full push must count as backpressure"
        );
        assert_eq!(
            drops.camera, 0,
            "ring-full backpressure must not be misattributed as a camera drop"
        );
    }

    #[test]
    fn idle_throttle_caps_inference_at_idle_hz() {
        // With the idle flag set, a 30 Hz active cap must behave like the 4 Hz
        // idle cap: the 250 ms idle period fits only the first (immediate)
        // inference inside a 120 ms window; everything else is dropped.
        let pipeline = empty_pipeline();
        let make_source = looping_solid_source();
        let (producer, mut consumer) = rtrb::RingBuffer::<WorkerMsg>::new(64);
        let mut handle = spawn_worker(make_source, pipeline, 30, tuning(true), producer);

        let (hand_messages, max_dropped) =
            drain_until(&mut consumer, Instant::now() + Duration::from_millis(120));
        handle.stop();

        assert!(
            hand_messages <= 1,
            "idle throttle processed {hand_messages} inference frames in 120 ms \
             (expected at most the immediate first frame at {IDLE_INFERENCE_HZ} Hz)"
        );
        assert!(
            max_dropped > 0,
            "idle-throttled over-budget frames were not reported as dropped"
        );
    }

    #[test]
    fn clearing_idle_throttle_restores_full_rate_live() {
        // The flag is re-read every loop iteration: clearing it mid-run (what
        // the activity-mirror system does on wake) must restore the 30 Hz cap
        // without a worker restart.
        let pipeline = empty_pipeline();
        let make_source = looping_solid_source();
        let cell = tuning(true);
        let (producer, mut consumer) = rtrb::RingBuffer::<WorkerMsg>::new(64);
        let mut handle = spawn_worker(make_source, pipeline, 30, Arc::clone(&cell), producer);

        // Wait for the first (throttled) inference frame so the worker is
        // demonstrably up before un-throttling.
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut saw_first = false;
        while Instant::now() < deadline && !saw_first {
            while let Ok(msg) = consumer.pop() {
                if matches!(msg, WorkerMsg::Hands { .. }) {
                    saw_first = true;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(saw_first, "worker never produced its first frame");

        cell.set_idle_throttle(false);
        let (hand_messages, _) =
            drain_until(&mut consumer, Instant::now() + Duration::from_millis(300));
        handle.stop();

        // Still throttled, 300 ms would fit at most 2 frames (4 Hz period);
        // at the restored 30 Hz cap ~9 arrive. ≥ 3 separates the two cleanly
        // even under scheduler jitter.
        assert!(
            hand_messages >= 3,
            "after clearing the idle flag only {hand_messages} frames arrived in 300 ms \
             (full rate not restored)"
        );
    }

    #[test]
    fn throttled_inference_still_emits_hand_bearing_frames() {
        // Wake contract: `lifecycle::idle::reset_on_interaction` wakes ONLY on
        // a hand-bearing tracking frame, so a throttled frame that sees a palm
        // must run the full pipeline and emit its hand — the idle throttle
        // lowers the inference *rate*, never the per-frame behavior.
        let pipeline = hand_pipeline();
        let make_source = looping_solid_source();
        let (producer, mut consumer) = rtrb::RingBuffer::<WorkerMsg>::new(64);
        let mut handle = spawn_worker(make_source, pipeline, 30, tuning(true), producer);

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut saw_hand = false;
        while Instant::now() < deadline && !saw_hand {
            while let Ok(msg) = consumer.pop() {
                if let WorkerMsg::Hands { hands, .. } = msg {
                    if !hands.is_empty() {
                        saw_hand = true;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        handle.stop();
        assert!(
            saw_hand,
            "throttled worker never emitted a hand-bearing frame; wake-from-idle would be broken"
        );
    }

    /// Test source that serves looping solid frames and records throttle toggles.
    struct ThrottleRecordingSource {
        inner: MockFrameSource,
        log: Arc<std::sync::Mutex<Vec<bool>>>,
    }

    impl FrameSource for ThrottleRecordingSource {
        fn next_frame(&mut self, out: &mut Frame) -> Result<bool, CaptureError> {
            self.inner.next_frame(out)
        }
        fn discard_frame(&mut self) -> Result<bool, CaptureError> {
            self.inner.discard_frame()
        }
        fn set_capture_throttle(&mut self, throttled: bool) {
            self.log
                .lock()
                .expect("throttle log poisoned")
                .push(throttled);
        }
    }

    fn throttle_recording_source(log: Arc<std::sync::Mutex<Vec<bool>>>) -> SourceFactory {
        Box::new(move || {
            let mut f = Frame::default();
            f.fit_to(64, 48);
            let src: Box<dyn FrameSource> = Box::new(ThrottleRecordingSource {
                inner: MockFrameSource::looping(vec![f]),
                log,
            });
            Ok(src)
        })
    }

    #[test]
    fn worker_edge_triggers_capture_throttle_on_idle_change() {
        let log = Arc::new(std::sync::Mutex::new(Vec::<bool>::new()));
        let cell = tuning(false);
        let (producer, _consumer) = rtrb::RingBuffer::<WorkerMsg>::new(64);
        let mut handle = spawn_worker(
            throttle_recording_source(Arc::clone(&log)),
            empty_pipeline(),
            30,
            Arc::clone(&cell),
            producer,
        );

        // Wait for the initial sync call (false), then flip to idle, then back.
        let wait_len = |n: usize| {
            for _ in 0..200 {
                if log.lock().expect("throttle log poisoned").len() >= n {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            false
        };
        assert!(wait_len(1), "no initial throttle sync");
        cell.set_idle_throttle(true);
        assert!(wait_len(2), "idle transition not dispatched");
        cell.set_idle_throttle(false);
        assert!(wait_len(3), "active transition not dispatched");
        handle.stop();

        assert_eq!(
            *log.lock().expect("throttle log poisoned"),
            vec![false, true, false]
        );
    }

    /// Inference backend that always fails, so `Pipeline::process` returns `Err`.
    struct FailingInference;

    impl HandInference for FailingInference {
        fn run(&mut self, _input: &Tensor, _out: &mut Vec<Tensor>) -> Result<(), InferenceError> {
            Err(InferenceError::Run("boom".into()))
        }
    }

    #[test]
    fn worker_retains_pipeline_errors() {
        let pipeline = Pipeline::new(
            Box::new(FailingInference),
            Box::new(FailingInference),
            PipelineConfig::default(),
        );
        let make_source = looping_solid_source();
        let (producer, mut consumer) = rtrb::RingBuffer::<WorkerMsg>::new(64);
        let mut handle = spawn_worker(make_source, pipeline, 30, tuning(false), producer);

        let deadline = Instant::now() + Duration::from_millis(120);
        let mut saw_error = false;
        let mut max_errors = 0_u64;
        while Instant::now() < deadline {
            while let Ok(msg) = consumer.pop() {
                match msg {
                    WorkerMsg::Error(_) => saw_error = true,
                    WorkerMsg::Diagnostics(d) => max_errors = max_errors.max(d.pipeline_errors),
                    _ => {}
                }
            }
            if saw_error && max_errors > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        handle.stop();

        assert!(
            saw_error,
            "pipeline error was not surfaced as WorkerMsg::Error"
        );
        assert!(max_errors > 0, "pipeline_errors counter did not increment");
    }
}
