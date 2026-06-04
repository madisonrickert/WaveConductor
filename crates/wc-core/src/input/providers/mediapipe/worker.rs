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
use crate::input::hand::Hand;
use crate::input::state::{
    DeviceHealth, DevicePresence, ProviderStatus, ServiceConnection, TrackingFlow, MAX_HANDS,
};

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
    let min_dt =
        Duration::from_secs_f32(1.0 / f32::from(u16::try_from(max_hz.max(1)).unwrap_or(30)));

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
            let mut last_process = start;
            let _ = producer.push(WorkerMsg::Status(streaming_status(Duration::ZERO, 0)));

            while !stop_thread.load(Ordering::Relaxed) {
                let loop_start = Instant::now();
                match source.next_frame(&mut frame) {
                    Ok(true) => {
                        let now = loop_start.duration_since(start);
                        let dt = loop_start.duration_since(last_process);
                        last_process = loop_start;
                        if let Ok(hands) = pipeline.process(&frame, dt) {
                            let _ = producer.push(WorkerMsg::Hands {
                                hands,
                                timestamp: now,
                            });
                            let _ = producer
                                .push(WorkerMsg::Status(streaming_status(Duration::ZERO, 0)));
                        }
                    }
                    Ok(false) => {}
                    Err(_) => {
                        let _ = producer.push(WorkerMsg::Status(no_camera_status()));
                    }
                }
                // Rate-cap: sleep the remainder of the frame budget (0 if over).
                std::thread::sleep(min_dt.saturating_sub(loop_start.elapsed()));
            }
        })
        .ok();

    WorkerHandle { stop, join }
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
    use super::super::inference::TractInference;
    use super::super::pipeline::PipelineConfig;
    use super::*;
    use crate::input::state::PrimaryState;
    use std::path::PathBuf;

    fn model(name: &str, shape: &[usize]) -> Box<dyn super::super::inference::HandInference> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/models/hand")
            .join(name);
        let bytes = std::fs::read(path).expect("read model");
        Box::new(TractInference::load(&bytes, shape).expect("load model"))
    }

    #[test]
    fn worker_publishes_status_and_frames_for_a_mock_source() {
        let pipeline = Pipeline::new(
            model("palm_detection.onnx", &[1, 192, 192, 3]),
            model("hand_landmark.onnx", &[1, 224, 224, 3]),
            PipelineConfig::default(),
        );
        // Looping solid frames → worker keeps producing (0 hands, healthy status).
        let make_source: SourceFactory = Box::new(|| {
            let mut f = Frame::default();
            f.fit_to(64, 48);
            let src: Box<dyn FrameSource> = Box::new(MockFrameSource::looping(vec![f]));
            Ok(src)
        });
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
}
