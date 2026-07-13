//! Main-thread systems: request-driven worker lifecycle, ring drain,
//! poll-rate smoothing, resource publication, and the presence→idle hook.
//!
//! Both systems are cheap no-ops while no [`super::BodyTrackingRequest`]
//! exists (an early-out on an absent resource / empty runtime), which is the
//! sanctioned always-on-listener shape: they must observe the request's
//! insertion in every app state, so they gate internally rather than on
//! `sketch_active`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::asset::RenderAssetUsages;
use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use rtrb::{Consumer, Producer};

use super::pipeline::BodyLiveTuning;
use super::smoothing::BodySmoother;
use super::transport::{
    seed_payload_pool, BodyFramePayload, BodyWorkerMsg, PAYLOAD_POOL_SIZE, RESULT_RING_CAPACITY,
};
use super::worker::{load_pose_pipeline, spawn_body_worker, SourceFactory, WorkerHandle};
use super::{
    BodyLandmark, BodyTrackingConfig, BodyTrackingDiagnostics, BodyTrackingRequest,
    BodyTrackingState, BodyTrackingStatus, MaskTexture, SilhouetteEdges, BODY_LANDMARK_COUNT,
    MASK_SIZE_U32,
};
use crate::input::capture::{CaptureError, FrameSource};
use crate::lifecycle::idle::InteractionTimer;

/// The latest worker result, held between worker frames as the smoothing
/// target (the worker runs at inference cadence; smoothing runs per poll).
struct BodyTarget {
    present: bool,
    confidence: f32,
    landmarks: [BodyLandmark; BODY_LANDMARK_COUNT],
    world: [Vec3; BODY_LANDMARK_COUNT],
    timestamp: Duration,
}

impl Default for BodyTarget {
    fn default() -> Self {
        Self {
            present: false,
            confidence: 0.0,
            landmarks: [BodyLandmark::default(); BODY_LANDMARK_COUNT],
            world: [Vec3::ZERO; BODY_LANDMARK_COUNT],
            timestamp: Duration::ZERO,
        }
    }
}

/// Everything that exists only while a request is active.
pub(crate) struct BodyRuntime {
    worker: WorkerHandle,
    consumer: Consumer<BodyWorkerMsg>,
    recycle: Producer<Box<BodyFramePayload>>,
    tuning: Arc<BodyLiveTuning>,
    smoother: BodySmoother,
    target: BodyTarget,
    /// Whether the previous poll published a person — lets the state emit a
    /// single clearing write when the person leaves, then stay quiet.
    had_person: bool,
}

/// Owns the worker runtime. `rtrb` endpoints are `Send` but not `Sync`, and
/// Bevy resources must be `Sync`; the `Mutex` provides that (main-thread-only
/// access, so there is never contention — the same shape as the hand
/// provider's `runtime` field).
#[derive(Resource, Default)]
pub struct BodyTrackingWorker {
    /// `Some` while a request is active.
    pub(crate) runtime: Mutex<Option<BodyRuntime>>,
    /// Test-injected camera source (used instead of opening a webcam).
    #[cfg(test)]
    pub(crate) injected_source: Mutex<Option<Box<dyn FrameSource + Send>>>,
    /// Test-injected pipeline factory (used instead of loading models).
    #[cfg(test)]
    pub(crate) injected_pipeline: Mutex<Option<super::worker::PipelineFactory>>,
}

/// Startup: create the reused `R8Unorm` mask image and publish
/// [`MaskTexture`]. Skipped (with a log line) in bare harnesses without
/// image assets; `poll_body_worker` tolerates the absence.
pub fn init_mask_texture(
    mut commands: Commands<'_, '_>,
    images: Option<ResMut<'_, Assets<Image>>>,
) {
    let Some(mut images) = images else {
        tracing::info!("body tracking: no Assets<Image>; MaskTexture disabled (headless)");
        return;
    };
    let image = Image::new_fill(
        Extent3d {
            width: MASK_SIZE_U32,
            height: MASK_SIZE_U32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0_u8],
        TextureFormat::R8Unorm,
        // MAIN_WORLD (CPU bytes rewritten each body frame) + RENDER_WORLD
        // (sampled by the silhouette material).
        RenderAssetUsages::default(),
    );
    commands.insert_resource(MaskTexture(images.add(image)));
}

/// `PreUpdate`: reconcile the worker with the request — start on insertion,
/// stop on removal (join + clear published state), and mirror
/// `idle_throttle` into the shared tuning cell every frame (one Relaxed
/// store; unconditional so a rebuilt worker picks the true state up within
/// one frame, matching the hand provider's mirror rationale).
pub fn sync_body_tracking(
    request: Option<Res<'_, BodyTrackingRequest>>,
    config: Res<'_, BodyTrackingConfig>,
    worker: Res<'_, BodyTrackingWorker>,
    mut state: ResMut<'_, BodyTrackingState>,
    mut edges: ResMut<'_, SilhouetteEdges>,
    mut diagnostics: ResMut<'_, BodyTrackingDiagnostics>,
) {
    let Ok(mut runtime) = worker.runtime.lock() else {
        return;
    };
    match (request, runtime.is_some()) {
        (Some(req), true) => {
            if let Some(rt) = runtime.as_ref() {
                rt.tuning.set_idle_throttle(req.idle_throttle);
            }
            diagnostics.idle_throttled = req.idle_throttle;
        }
        (Some(req), false) => {
            *runtime = Some(start_worker(&worker, &config, &req));
            *diagnostics = BodyTrackingDiagnostics {
                status: BodyTrackingStatus::Starting,
                idle_throttled: req.idle_throttle,
                ..BodyTrackingDiagnostics::default()
            };
            tracing::info!("body tracking: request received, worker starting");
        }
        (None, true) => {
            // Explicit stop (mirrors the hand provider's `stop()`) rather
            // than relying solely on WorkerHandle's Drop impl: it still
            // joins on drop as a backstop, but stopping explicitly here
            // reads the handle so intent is visible at the call site.
            if let Some(mut rt) = runtime.take() {
                rt.worker.stop();
            }
            *state = BodyTrackingState::default();
            edges.points.clear();
            edges.generation = edges.generation.wrapping_add(1);
            *diagnostics = BodyTrackingDiagnostics::default();
            tracing::info!("body tracking: request removed, worker stopped");
        }
        (None, false) => {}
    }
}

/// `PreUpdate` (after [`sync_body_tracking`]): drain the worker ring, keep
/// the newest frame as the smoothing target, publish
/// [`BodyTrackingState`] / mask bytes / [`SilhouetteEdges`], recycle mask
/// payloads, and mark the idle [`InteractionTimer`] on person-bearing frames
/// (empty frames never mark — same semantics as hand-bearing frames in
/// `reset_on_interaction`; see the plugin doc).
///
/// [`BodyWorkerMsg::Diagnostics`] snapshots pushed from the worker's
/// drop-triggered path (over-budget camera drops, see
/// `worker::run_worker_loop`) carry a zeroed `inference_interval` (no frame
/// was actually processed). This system still copies it through
/// unconditionally — `BodyWorkerDiagnostics` is a whole-struct snapshot, and
/// filtering here would need to distinguish "genuinely instant" from
/// "unmeasured" with no signal to do so reliably. A display reading
/// [`BodyTrackingDiagnostics::inference_interval`] is the layer that must
/// treat `Duration::ZERO` as "not a measurement" rather than "instant frame".
#[allow(
    clippy::too_many_arguments,
    reason = "publication fan-out; one system keeps the drain atomic"
)]
pub fn poll_body_worker(
    time: Res<'_, Time>,
    worker: Res<'_, BodyTrackingWorker>,
    mut state: ResMut<'_, BodyTrackingState>,
    mut edges: ResMut<'_, SilhouetteEdges>,
    mut diagnostics: ResMut<'_, BodyTrackingDiagnostics>,
    mask: Option<Res<'_, MaskTexture>>,
    mut images: Option<ResMut<'_, Assets<Image>>>,
    mut timer: Option<ResMut<'_, InteractionTimer>>,
) {
    let Ok(mut runtime) = worker.runtime.lock() else {
        return;
    };
    let Some(rt) = runtime.as_mut() else {
        return;
    };
    let now = time.elapsed();
    let mut person_frame = false;

    while let Ok(msg) = rt.consumer.pop() {
        match msg {
            BodyWorkerMsg::Frame(mut frame) => {
                if let Some(payload) = frame.payload.take() {
                    // Copy the mask bytes into the shared image (Bevy
                    // re-uploads on mutation; 256 KB is trivial)…
                    if let (Some(mask), Some(images)) = (&mask, images.as_deref_mut()) {
                        if let Some(mut image) = images.get_mut(&mask.0) {
                            if let Some(data) = image.data.as_mut() {
                                data.copy_from_slice(&payload.mask);
                            }
                        }
                    }
                    // …refill the edge list in place (capacity preserved)…
                    edges.points.clear();
                    edges.points.extend_from_slice(&payload.edges);
                    edges.generation = edges.generation.wrapping_add(1);
                    // …and hand the buffer back to the worker. The recycle
                    // ring is sized pool+1 so this cannot fail; if it ever
                    // did, dropping the box merely shrinks the pool.
                    let _ = rt.recycle.push(payload);
                }
                if frame.present {
                    person_frame = true;
                } else if rt.target.present {
                    // Person left: reset the smoother so a return starts
                    // fresh (no stale momentum), mirroring the hand smoother.
                    rt.smoother.clear();
                }
                rt.target = BodyTarget {
                    present: frame.present,
                    confidence: frame.confidence,
                    landmarks: frame.landmarks,
                    world: frame.world_landmarks,
                    timestamp: frame.timestamp,
                };
            }
            BodyWorkerMsg::Backend(backend) => {
                diagnostics.backend = backend;
                tracing::info!("body inference backend: {backend} (detector+landmark)");
            }
            BodyWorkerMsg::Status(status) => diagnostics.status = status,
            BodyWorkerMsg::Diagnostics(d) => {
                diagnostics.inference_interval = d.inference_interval;
                diagnostics.dropped_frames = d.dropped_frames;
                diagnostics.ring_full_drops = d.ring_full_drops;
                diagnostics.pipeline_errors = d.pipeline_errors;
                diagnostics.idle_throttled = d.idle_throttled;
                diagnostics.capture_decode = d.capture_decode;
                diagnostics.pipeline = d.pipeline;
            }
            BodyWorkerMsg::Error(e) => diagnostics.last_error = Some(e),
            BodyWorkerMsg::CameraFormat(f) => diagnostics.camera_format = Some(f),
        }
    }

    // Presence → interaction: identical semantics to hand-bearing frames in
    // reset_on_interaction (both end in InteractionTimer::mark; empty frames
    // are ignored by construction).
    if person_frame {
        if let Some(timer) = timer.as_mut() {
            timer.mark(now);
        }
        if !rt.had_person {
            tracing::info!("body tracking: person detected");
        }
    }

    // Ease the exposed pose toward the held target every poll so the
    // inference cadence renders as fluid motion.
    if rt.target.present {
        let smoothed = rt
            .smoother
            .smooth(&rt.target.landmarks, &rt.target.world, now);
        state.present = true;
        state.confidence = rt.target.confidence;
        state.landmarks = smoothed.landmarks;
        state.world_landmarks = smoothed.world;
        state.velocities = smoothed.velocities;
        state.timestamp = rt.target.timestamp;
        rt.had_person = true;
    } else if rt.had_person {
        // One clearing write when the person leaves, then quiet.
        rt.had_person = false;
        *state = BodyTrackingState::default();
        rt.smoother.clear();
        tracing::info!("body tracking: person lost");
    }
}

/// Build the rings, seed the payload pool, and spawn the worker. Reads the
/// three Dev-panel tuning fields off `request` (Plan C Task 14): the mask
/// combine-with-previous ratio seeds the shared live-tuning cell the worker
/// polls each frame, and
/// the One-Euro min-cutoff/beta seed the main-thread smoother constructed
/// below. Because these fields are `requires_restart` (Plan C Task 2),
/// `sync_body_tracking` only reaches this function on a fresh request insert
/// — including the re-insert after a Dev-panel reload — so a settings change
/// always takes effect on the next worker (re)start.
fn start_worker(
    worker: &BodyTrackingWorker,
    config: &BodyTrackingConfig,
    request: &BodyTrackingRequest,
) -> BodyRuntime {
    let (result_tx, result_rx) = rtrb::RingBuffer::new(RESULT_RING_CAPACITY);
    // Sized PAYLOAD_POOL_SIZE + 1: the transport seeding invariant (seed_payload_pool
    // pushes PAYLOAD_POOL_SIZE boxes and must never see the ring report full).
    let (mut recycle_tx, recycle_rx) = rtrb::RingBuffer::new(PAYLOAD_POOL_SIZE + 1);
    debug_assert!(
        recycle_tx.buffer().capacity() > PAYLOAD_POOL_SIZE,
        "recycle ring must hold PAYLOAD_POOL_SIZE + 1 so seeding + one in-flight payload never fails"
    );
    seed_payload_pool(&mut recycle_tx);
    let tuning = Arc::new(BodyLiveTuning::new(request.mask_ema));
    tuning.set_idle_throttle(request.idle_throttle);

    #[cfg(test)]
    let injected_source = worker
        .injected_source
        .lock()
        .ok()
        .and_then(|mut slot| slot.take());
    #[cfg(not(test))]
    let injected_source: Option<Box<dyn FrameSource + Send>> = None;
    #[cfg(not(test))]
    let _ = worker; // only the test slots read it here
    let camera_index = config.camera_index;
    let make_source: SourceFactory = match injected_source {
        // The injected source can only be handed out once; the retry loop
        // never re-asks after a successful open, and a second call (should one
        // ever happen) degrades to a clean error rather than double-taking.
        Some(src) => {
            let mut slot = Some(src);
            Box::new(move || match slot.take() {
                // `Box<dyn FrameSource + Send>` unsizes to `Box<dyn FrameSource>`
                // via the let-binding's target type (no `as`, which is denied).
                Some(src) => {
                    let boxed: Box<dyn FrameSource> = src;
                    Ok(boxed)
                }
                None => Err(CaptureError::NoCamera("injected source consumed".into())),
            })
        }
        None => Box::new(move || open_camera_source(camera_index)),
    };

    #[cfg(test)]
    let injected_pipeline = worker
        .injected_pipeline
        .lock()
        .ok()
        .and_then(|mut slot| slot.take());
    #[cfg(not(test))]
    let injected_pipeline: Option<super::worker::PipelineFactory> = None;
    let make_pipeline: super::worker::PipelineFactory = if let Some(pipeline) = injected_pipeline {
        pipeline
    } else {
        let model_dir = config.model_dir.clone();
        Box::new(move || load_pose_pipeline(&model_dir))
    };

    let handle = spawn_body_worker(
        make_source,
        make_pipeline,
        config.max_inference_hz,
        Arc::clone(&tuning),
        result_tx,
        recycle_rx,
    );
    BodyRuntime {
        worker: handle,
        consumer: result_rx,
        recycle: recycle_tx,
        tuning,
        // One-Euro params seeded straight from the request (see the
        // function doc). BodySmoother::set_params exists for retuning an
        // already-running smoother without resetting filter state, but
        // nothing currently calls it mid-run: these fields are
        // requires_restart, so a change always arrives via a fresh
        // start_worker call instead.
        smoother: BodySmoother::new(request.one_euro_min_cutoff, request.one_euro_beta),
        target: BodyTarget::default(),
        had_person: false,
    }
}

/// Open a real webcam source on the calling (worker) thread, or error. The
/// same per-platform selection as the hand provider, gated on this
/// modality's camera feature.
pub fn open_camera_source(camera_index: u32) -> Result<Box<dyn FrameSource>, CaptureError> {
    #[cfg(all(feature = "body-tracking-camera", target_os = "macos"))]
    {
        let source = crate::input::capture::AvfFrameSource::open(camera_index)?;
        let boxed: Box<dyn FrameSource> = Box::new(source);
        Ok(boxed)
    }
    #[cfg(all(feature = "body-tracking-camera", not(target_os = "macos")))]
    {
        let source = crate::input::capture::NokhwaFrameSource::open(camera_index)?;
        let boxed: Box<dyn FrameSource> = Box::new(source);
        Ok(boxed)
    }
    #[cfg(not(feature = "body-tracking-camera"))]
    {
        let _ = camera_index;
        Err(CaptureError::NoCamera(
            "build with the body-tracking-camera feature".into(),
        ))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use std::time::Duration;

    use bevy::asset::AssetPlugin;
    use bevy::prelude::*;

    use super::super::pipeline::fixtures::{
        confident_landmark_outputs, empty_detector_outputs, hot_person_detector_outputs,
    };
    use super::super::pipeline::{PoseConfig, PosePipeline};
    use super::super::smoothing::{DEFAULT_BETA, DEFAULT_MIN_CUTOFF};
    use super::super::{
        BodyTrackingDiagnostics, BodyTrackingPlugin, BodyTrackingRequest, BodyTrackingState,
        MaskTexture, SilhouetteEdges,
    };
    use super::*;
    use crate::input::capture::{Frame, MockFrameSource};
    use crate::input::onnx::{InferenceError, ModelInference, Tensor};
    use crate::lifecycle::idle::InteractionTimer;

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

    /// A headless app with the plugin, an interaction timer, image assets,
    /// and injected mock camera + inference.
    fn body_app(detector: Vec<Tensor>, landmark: Vec<Tensor>) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        app.init_asset::<Image>();
        app.init_resource::<InteractionTimer>();
        app.add_plugins(BodyTrackingPlugin);
        {
            let worker = app.world().resource::<BodyTrackingWorker>();
            let mut frame = Frame::default();
            frame.fit_to(64, 48);
            *worker.injected_source.lock().expect("source slot") =
                Some(Box::new(MockFrameSource::looping(vec![frame])));
            *worker.injected_pipeline.lock().expect("pipeline slot") = Some(Box::new(move || {
                Ok((
                    PosePipeline::new(
                        Box::new(StaticInference { outputs: detector }),
                        Box::new(StaticInference { outputs: landmark }),
                        PoseConfig::default(),
                    ),
                    "mock/backend",
                ))
            }));
        }
        app
    }

    /// Update until `pred` holds or ~2 s elapse (the worker is asynchronous).
    fn update_until(app: &mut App, pred: impl Fn(&World) -> bool) -> bool {
        for _ in 0..200 {
            app.update();
            if pred(app.world()) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn request_starts_worker_and_publishes_state_mask_edges_and_presence() {
        let mut app = body_app(hot_person_detector_outputs(), confident_landmark_outputs());
        app.insert_resource(BodyTrackingRequest {
            idle_throttle: false,
            mask_ema: super::super::mask::DEFAULT_MASK_EMA_ALPHA,
            one_euro_min_cutoff: DEFAULT_MIN_CUTOFF,
            one_euro_beta: DEFAULT_BETA,
        });

        let tracked = update_until(&mut app, |w| w.resource::<BodyTrackingState>().present);
        assert!(tracked, "state never reported a person");

        let world = app.world();
        let state = world.resource::<BodyTrackingState>();
        assert!(state.confidence > 0.8);
        assert!(state.landmarks[0].pos.x.is_finite());
        assert!(state.landmarks[0].visibility > 0.7);
        assert!((state.world_landmarks[0].x - 0.1).abs() < 1e-4);

        let edges = world.resource::<SilhouetteEdges>();
        assert!(edges.generation > 0, "edges never refreshed");
        assert!(!edges.points.is_empty());

        let mask = world.resource::<MaskTexture>();
        let images = world.resource::<Assets<Image>>();
        let image = images.get(&mask.0).expect("mask image");
        let data = image.data.as_ref().expect("mask image holds CPU data");
        assert!(
            data.iter().any(|&b| b > 200),
            "mask bytes never written to the image"
        );

        let diagnostics = world.resource::<BodyTrackingDiagnostics>();
        assert_eq!(diagnostics.backend, "mock/backend");
        // The worker's per-frame timing split must survive the poll copy
        // (thermal diagnostic parity with the hand provider): the pipeline
        // snapshot reflects the person-bearing frame just processed.
        assert!(
            diagnostics.pipeline.present,
            "per-stage pipeline diagnostics were not copied from the worker"
        );
        assert!(diagnostics.pipeline.confidence > 0.8);

        // Presence marked the interaction timer (design decision 1).
        let timer = world.resource::<InteractionTimer>();
        assert!(
            timer.last_interaction() > Duration::ZERO,
            "person-bearing frames must reset the idle timer"
        );
    }

    #[test]
    fn empty_frames_do_not_touch_the_interaction_timer() {
        let mut app = body_app(empty_detector_outputs(), confident_landmark_outputs());
        app.insert_resource(BodyTrackingRequest {
            idle_throttle: false,
            mask_ema: super::super::mask::DEFAULT_MASK_EMA_ALPHA,
            one_euro_min_cutoff: DEFAULT_MIN_CUTOFF,
            one_euro_beta: DEFAULT_BETA,
        });
        // Give the worker ample time to stream empty frames.
        for _ in 0..40 {
            app.update();
            std::thread::sleep(Duration::from_millis(5));
        }
        let world = app.world();
        assert!(!world.resource::<BodyTrackingState>().present);
        assert_eq!(
            world.resource::<InteractionTimer>().last_interaction(),
            Duration::ZERO,
            "empty frames must never reset the idle timer"
        );
    }

    #[test]
    fn worker_start_reads_tuning_fields_from_the_request() {
        // Plan C Task 14: the three Dev-panel tuning fields on the request
        // must reach the worker's live-tuning cell (mask combine ratio) and the
        // main-thread smoother (One-Euro min-cutoff/beta) at worker start,
        // not just sit on the struct unread.
        let mut app = body_app(hot_person_detector_outputs(), confident_landmark_outputs());
        app.insert_resource(BodyTrackingRequest {
            idle_throttle: false,
            mask_ema: 0.9,
            one_euro_min_cutoff: 3.5,
            one_euro_beta: 12.0,
        });
        // One update is enough for sync_body_tracking to observe the fresh
        // request and call start_worker.
        app.update();

        let world = app.world();
        let worker = world.resource::<BodyTrackingWorker>();
        let runtime = worker.runtime.lock().expect("runtime lock");
        let rt = runtime.as_ref().expect("worker must have started");
        assert!(
            (rt.tuning.mask_ema_alpha() - 0.9).abs() < 1e-6,
            "mask_ema did not reach BodyLiveTuning: {}",
            rt.tuning.mask_ema_alpha()
        );
        let (min_cutoff, beta) = rt.smoother.params();
        assert!(
            (min_cutoff - 3.5).abs() < 1e-6,
            "one_euro_min_cutoff did not reach BodySmoother: {min_cutoff}"
        );
        assert!(
            (beta - 12.0).abs() < 1e-6,
            "one_euro_beta did not reach BodySmoother: {beta}"
        );
    }

    #[test]
    fn removing_the_request_stops_the_worker_and_clears_state() {
        let mut app = body_app(hot_person_detector_outputs(), confident_landmark_outputs());
        app.insert_resource(BodyTrackingRequest {
            idle_throttle: false,
            mask_ema: super::super::mask::DEFAULT_MASK_EMA_ALPHA,
            one_euro_min_cutoff: DEFAULT_MIN_CUTOFF,
            one_euro_beta: DEFAULT_BETA,
        });
        assert!(update_until(&mut app, |w| w
            .resource::<BodyTrackingState>()
            .present));

        app.world_mut().remove_resource::<BodyTrackingRequest>();
        app.update();

        let world = app.world();
        assert!(!world.resource::<BodyTrackingState>().present);
        assert!(world.resource::<SilhouetteEdges>().points.is_empty());
        let worker = world.resource::<BodyTrackingWorker>();
        assert!(
            worker.runtime.lock().expect("runtime lock").is_none(),
            "worker must be stopped and joined on request removal"
        );
    }
}
