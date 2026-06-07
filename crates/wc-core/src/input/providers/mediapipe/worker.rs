//! Background worker that runs the [`super::pipeline::Pipeline`] off the Bevy
//! main thread and publishes results over a lock-free `rtrb` ring.
//!
//! The worker owns the camera ([`super::capture::FrameSource`]) and both ONNX
//! sessions; it captures, runs the two-stage pipeline at a capped rate, and
//! pushes [`WorkerMsg`]s to the provider. The provider's `poll` drains the ring
//! on the main thread without blocking or allocating — mirroring how the Leap
//! provider keeps device I/O off the render path.
//!
//! Foundation module: wired into the provider in [`super::MediaPipeProvider`];
//! exercised by a mock-source plumbing test.
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rtrb::Producer;
use smallvec::SmallVec;

use super::capture::{CaptureError, Frame, FrameSource};
use super::pipeline::Pipeline;
use super::pipeline::PipelineDiagnostics;
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
}

/// Worker/pipeline diagnostics sent to the provider.
#[derive(Debug, Clone, Copy, Default)]
pub struct MediaPipeWorkerDiagnostics {
    /// Pipeline-stage metrics.
    pub pipeline: PipelineDiagnostics,
    /// Cumulative worker-side dropped messages/frames.
    pub dropped_frames: u64,
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

/// Spawn the worker thread. It runs until [`WorkerHandle::stop`] (or the handle
/// drops), capturing + processing frames at up to `max_hz` and pushing results
/// to `producer`.
#[must_use]
pub fn spawn_worker(
    make_source: SourceFactory,
    mut pipeline: Pipeline,
    max_hz: u32,
    mut producer: Producer<WorkerMsg>,
) -> WorkerHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    // The processing rate is capped by dropping over-budget captured frames, not
    // by sleeping after inference. The old sleep-based cap let the camera buffer
    // fill while we slept, so we processed ever-staler frames. This keeps the
    // single-thread invariant MediaPipe's FlowLimiter gives us: newest frame wins.
    let min_inference_interval = inference_interval(max_hz);

    let join = std::thread::Builder::new()
        .name("wc-mediapipe-worker".into())
        .spawn(move || {
            // Build the source on this thread (so !Send backends are fine).
            let Ok(mut source) = make_source() else {
                let _ = producer.push(WorkerMsg::Status(no_camera_status()));
                return;
            };

            let start = Instant::now();
            let mut frame = Frame::default();
            let mut last_inference = None;
            let mut dropped_frames = 0_u64;
            push_msg(
                &mut producer,
                WorkerMsg::Status(streaming_status(Duration::ZERO, 0)),
                &mut dropped_frames,
            );

            while !stop_thread.load(Ordering::Relaxed) {
                let loop_start = Instant::now();
                match source.next_frame(&mut frame) {
                    Ok(true) => {
                        if !should_process_frame(last_inference, loop_start, min_inference_interval)
                        {
                            dropped_frames = dropped_frames.saturating_add(1);
                            push_msg(
                                &mut producer,
                                WorkerMsg::Status(streaming_status(Duration::ZERO, dropped_frames)),
                                &mut dropped_frames,
                            );
                            // Fast mocks and some non-blocking camera sources can
                            // return frames immediately; after dropping one, back
                            // off briefly so the cap doesn't busy-spin a core.
                            std::thread::sleep(IDLE_POLL);
                            continue;
                        }

                        // A budgeted frame is in hand: process it immediately.
                        // No retained frame ages while we wait for the next budget
                        // window; all over-budget frames have already been dropped.
                        let now = loop_start.duration_since(start);
                        let dt = last_inference
                            .map_or(Duration::ZERO, |last| loop_start.duration_since(last));
                        last_inference = Some(loop_start);
                        if let Ok(hands) = pipeline.process(&frame, dt) {
                            push_msg(
                                &mut producer,
                                WorkerMsg::Diagnostics(MediaPipeWorkerDiagnostics {
                                    pipeline: pipeline.diagnostics(),
                                    dropped_frames,
                                }),
                                &mut dropped_frames,
                            );
                            push_msg(
                                &mut producer,
                                WorkerMsg::Hands {
                                    hands,
                                    timestamp: now,
                                },
                                &mut dropped_frames,
                            );
                            push_msg(
                                &mut producer,
                                WorkerMsg::Status(streaming_status(Duration::ZERO, dropped_frames)),
                                &mut dropped_frames,
                            );
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
        })
        .ok();

    WorkerHandle { stop, join }
}

/// Minimum interval between inference runs for a requested max rate.
fn inference_interval(max_hz: u32) -> Option<Duration> {
    (max_hz > 0).then(|| Duration::from_secs_f64(1.0 / f64::from(max_hz)))
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

/// Push a message and count ring-overwrite drops without blocking the worker.
fn push_msg(producer: &mut Producer<WorkerMsg>, msg: WorkerMsg, dropped_frames: &mut u64) {
    if producer.push(msg).is_err() {
        *dropped_frames = dropped_frames.saturating_add(1);
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
    use super::super::pipeline::PipelineConfig;
    use super::*;
    use crate::input::state::{PrimaryState, TrackingFlow};

    #[derive(Clone)]
    struct StaticInference {
        outputs: Vec<Tensor>,
    }

    impl HandInference for StaticInference {
        fn run(&mut self, _input: &Tensor) -> Result<Vec<Tensor>, InferenceError> {
            Ok(self.outputs.clone())
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
                Tensor::zeros(vec![1, 1]),
                Tensor::zeros(vec![1, 1]),
                Tensor::zeros(vec![1, 63]),
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

    #[test]
    fn worker_publishes_status_and_frames_for_a_mock_source() {
        let pipeline = empty_pipeline();
        let make_source = looping_solid_source();
        let (producer, mut consumer) = rtrb::RingBuffer::<WorkerMsg>::new(64);
        let mut handle = spawn_worker(make_source, pipeline, 30, producer);

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
        let mut handle = spawn_worker(make_source, pipeline, 1, producer);

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
                    WorkerMsg::Diagnostics(_) => {}
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
}
