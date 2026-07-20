//! Main-thread systems: request-driven worker lifecycle, ring drain, per-slot
//! presence-hold + fade envelopes + poll-rate smoothing, primary selection,
//! resource publication, and the presence→idle hook.
//!
//! Both systems are cheap no-ops while no [`super::BodyTrackingRequest`]
//! exists (an early-out on an absent resource / empty runtime), which is the
//! sanctioned always-on-listener shape: they must observe the request's
//! insertion in every app state, so they gate internally rather than on
//! `sketch_active`.
//!
//! Per-slot publication: each worker slot gets its own presence-hold
//! ([`super::envelope::presence_decision`]), fade envelope
//! ([`super::envelope::fade_step`]) and One-Euro smoother; a slot's
//! `TrackedBody` entry is removed only once its fade releases to zero, so
//! figures leave the screen gracefully. Primary selection
//! ([`super::selection::PrimarySelect`]) runs here too — the `KeyN` hotkey
//! cycles it via [`BodyTrackingWorker::request_person_cycle`].

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::asset::RenderAssetUsages;
use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use rtrb::{Consumer, Producer};

use super::envelope::{admit_step, fade_step, presence_decision, PresenceDecision};
use super::pipeline::BodyLiveTuning;
use super::selection::{body_motion_measure, motion_ema_step, primary_score, PrimarySelect};
use super::smoothing::BodySmoother;
use super::transport::{
    seed_payload_pool, BodyFramePayload, BodyWorkerMsg, SlotFrame, PAYLOAD_POOL_SIZE,
    RESULT_RING_CAPACITY,
};
use super::worker::{load_pose_pipeline, spawn_body_worker, SourceFactory, WorkerHandle};
use super::{
    BodyTrackingConfig, BodyTrackingDiagnostics, BodyTrackingRequest, BodyTrackingState,
    BodyTrackingStatus, MaskTexture, SilhouetteEdges, TrackedBody, MASK_SIZE_U32,
    MAX_TRACKED_BODIES,
};
use crate::input::capture::{CaptureError, FrameSource};
use crate::lifecycle::idle::InteractionTimer;

/// One slot's publisher-side state: the latest worker result held as the
/// smoothing target (the worker runs at inference cadence; smoothing runs per
/// poll), the presence-hold deadline, the fade envelope, and the slot's own
/// One-Euro smoother.
struct SlotRuntime {
    /// Latest worker result for this slot (held between worker frames).
    target: SlotFrame,
    /// Worker-relative capture timestamp of `target`.
    timestamp: Duration,
    /// `Time::elapsed` value until which presence is held even though the
    /// worker reports no person (see `envelope::PRESENCE_HOLD`).
    hold_until: Duration,
    /// Admission-dwell state (see `envelope::ADMIT_DWELL`): `true` once the
    /// current occupant has persisted long enough for their fade-in to begin.
    /// Kept through the fade-out tail; reset only when the slot fully frees.
    admitted: bool,
    /// When the current continuous-present run began (`None` while absent).
    /// The dwell clock; survives held dropouts.
    present_since: Option<Duration>,
    /// The graceful appearance envelope (`TrackedBody::fade`).
    fade: f32,
    /// Smoothed motion measure (`selection::motion_ema_step` over
    /// `selection::body_motion_measure`), published as `TrackedBody::motion`.
    /// Plain per-slot state — no allocation on the poll path.
    motion_ema: f32,
    /// This slot's poll-rate One-Euro smoother.
    smoother: BodySmoother,
}

/// Everything that exists only while a request is active.
pub(crate) struct BodyRuntime {
    worker: WorkerHandle,
    consumer: Consumer<BodyWorkerMsg>,
    recycle: Producer<Box<BodyFramePayload>>,
    tuning: Arc<BodyLiveTuning>,
    /// Per-slot publisher state, indexed by stable slot.
    slots: [SlotRuntime; MAX_TRACKED_BODIES],
    /// Primary-slot selection (auto hysteresis + `KeyN` manual pin).
    primary: PrimarySelect,
    /// Whether the previous poll had any occupied slot — lets the publisher
    /// log arrival/departure once instead of every frame.
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
    /// Pending `KeyN` person-cycle presses, consumed by `poll_body_worker`.
    /// With stable slots the cycle is a *primary* switch, decided entirely on
    /// the main thread — it never crosses to the worker (unlike the old
    /// single-track design, which had to re-seed the worker's crop).
    cycle_requests: AtomicU32,
    /// Test-injected camera source (used instead of opening a webcam).
    #[cfg(test)]
    pub(crate) injected_source: Mutex<Option<Box<dyn FrameSource + Send>>>,
    /// Test-injected pipeline factory (used instead of loading models).
    #[cfg(test)]
    pub(crate) injected_pipeline: Mutex<Option<super::worker::PipelineFactory>>,
}

impl BodyTrackingWorker {
    /// Request that primary cycle to the next present body on the next poll
    /// (the `KeyN` hotkey). A counter, not a bool, so rapid presses each
    /// register; lock-free; a harmless no-op while nothing is tracked. Takes
    /// `&self` so a sketch system can drive it from a plain
    /// `Res<BodyTrackingWorker>`.
    pub fn request_person_cycle(&self) {
        self.cycle_requests.fetch_add(1, Ordering::Relaxed);
    }

    /// Drain the pending cycle presses (publisher-side).
    fn take_cycle_requests(&self) -> u32 {
        self.cycle_requests.swap(0, Ordering::Relaxed)
    }
}

/// Startup: create the reused `Rgba8Unorm` multi-person mask image (channel
/// `i` = slot `i`, the pinned convention — see the module doc on
/// [`super::MaskTexture`]) and publish [`MaskTexture`]. Skipped (with a log
/// line) in bare harnesses without image assets; `poll_body_worker` tolerates
/// the absence.
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
        // One RGBA texel: all four slot channels dark.
        &[0_u8, 0, 0, 0],
        TextureFormat::Rgba8Unorm,
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
            edges.slot_counts = [0; MAX_TRACKED_BODIES];
            edges.generation = edges.generation.wrapping_add(1);
            *diagnostics = BodyTrackingDiagnostics::default();
            tracing::info!("body tracking: request removed, worker stopped");
        }
        (None, false) => {}
    }
}

/// `PreUpdate` (after [`sync_body_tracking`]): drain the worker ring, keep
/// the newest frame as each slot's smoothing target, advance the per-slot
/// presence-hold + fade envelopes, publish [`BodyTrackingState`] (per-slot
/// [`TrackedBody`]s + the primary slot) / mask bytes / [`SilhouetteEdges`],
/// recycle mask payloads, and mark the idle [`InteractionTimer`] on
/// person-bearing frames (empty frames never mark — same semantics as
/// hand-bearing frames in `reset_on_interaction`; see the plugin doc).
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
                    // Copy the RGBA mask bytes into the shared image (Bevy
                    // re-uploads on mutation; 256 KB is trivial)…
                    if let (Some(mask), Some(images)) = (&mask, images.as_deref_mut()) {
                        if let Some(mut image) = images.get_mut(&mask.0) {
                            if let Some(data) = image.data.as_mut() {
                                data.copy_from_slice(&payload.mask);
                            }
                        }
                    }
                    // …refill the slot-partitioned edge list in place
                    // (capacity preserved)…
                    edges.points.clear();
                    edges.points.extend_from_slice(&payload.edges);
                    edges.slot_counts = payload.edge_slot_counts;
                    edges.generation = edges.generation.wrapping_add(1);
                    // …and hand the buffer back to the worker. The recycle
                    // ring is sized pool+1 so this cannot fail; if it ever
                    // did, dropping the box merely shrinks the pool.
                    let _ = rt.recycle.push(payload);
                }
                // Per-slot presence-hold: a present slot re-arms its hold and
                // updates its target; a held slot keeps the last pose; an
                // absent slot starts (or continues) its fade-out. The
                // admission dwell runs beside the hold: fade (and
                // publication) only begin once the occupant has persisted
                // ADMIT_DWELL — see envelope::admit_step for the busy-road
                // rationale and publish_bodies for the gate itself.
                for (slot, incoming) in rt.slots.iter_mut().zip(frame.slots.iter()) {
                    let (decision, hold_until) =
                        presence_decision(incoming.present, now, slot.hold_until);
                    slot.hold_until = hold_until;
                    (slot.admitted, slot.present_since) =
                        admit_step(slot.admitted, slot.present_since, decision, now);
                    match decision {
                        PresenceDecision::Present => {
                            person_frame = true;
                            slot.target = *incoming;
                            slot.timestamp = frame.timestamp;
                        }
                        // Within the hold window after a dropout: keep the
                        // last target untouched (present stays true, last
                        // good pose held) so a momentary detector dropout
                        // does not blank the figure.
                        PresenceDecision::Held => {}
                        PresenceDecision::Absent => {
                            slot.target.present = false;
                        }
                    }
                }
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
    }

    // Per-slot publication: smooth present slots toward their targets every
    // poll; fade absent slots out in place; free a slot only at fade 0.
    publish_bodies(&mut rt.slots, &mut state, now, time.delta_secs());

    // Arrival/departure logging, once per transition.
    let occupied = state.bodies.iter().any(Option::is_some);
    if occupied && !rt.had_person {
        tracing::info!("body tracking: person detected");
    } else if !occupied && rt.had_person {
        tracing::info!("body tracking: all people left");
    }
    rt.had_person = occupied;

    // Primary selection: manual `KeyN` cycles first, then the score-based
    // auto policy (size × crop penalty × motion weight, with switch
    // hysteresis — closer, moving people win; see selection::primary_score).
    let mut present = [false; MAX_TRACKED_BODIES];
    let mut scores: [Option<f32>; MAX_TRACKED_BODIES] = [None; MAX_TRACKED_BODIES];
    for (i, body) in state.bodies.iter().enumerate() {
        if let Some(body) = body {
            if body.present {
                present[i] = true;
                scores[i] = Some(primary_score(body.size, body.crop_fraction, body.motion));
            }
        }
    }
    for _ in 0..worker.take_cycle_requests() {
        rt.primary.cycle(&present);
    }
    rt.primary.update(&scores, now);
    state.primary = rt.primary.current();
}

/// Per-slot publication step of [`poll_body_worker`]: advance each slot's
/// fade envelope, smooth present slots toward their targets, hold fading-out
/// slots' last pose in place with a decaying fade (velocities zeroed — a
/// frozen figure sheds no impulses), and free a slot's [`TrackedBody`] entry
/// only once its fade reaches exactly zero (the graceful-disappearance
/// contract). No allocation: `Some(TrackedBody { .. })` writes inline into
/// the fixed `bodies` array.
///
/// Admission gate: a present-but-not-yet-admitted slot (still inside the
/// `envelope::ADMIT_DWELL`) is treated as absent here — its fade stays 0 and
/// no `TrackedBody` publishes, so a road-traffic walk-through never reaches
/// any consumer. Because the fade never leaves 0, a candidate that vanishes
/// mid-dwell needs no release tail: the slot is clean the moment the worker
/// reports it absent.
fn publish_bodies(
    slots: &mut [SlotRuntime; MAX_TRACKED_BODIES],
    state: &mut BodyTrackingState,
    now: Duration,
    dt: f32,
) {
    for (i, slot) in slots.iter_mut().enumerate() {
        // Present AND past the admission dwell: only then does the envelope
        // attack / the body publish.
        let engaged = slot.target.present && slot.admitted;
        slot.fade = fade_step(slot.fade, engaged, dt);
        if engaged {
            let smoothed =
                slot.smoother
                    .smooth(&slot.target.landmarks, &slot.target.world_landmarks, now);
            // Motion envelope: distance-normalized torso speed through the
            // slow EMA (see selection::body_motion_measure). Updated only
            // while engaged; the fade-out branch below holds the last value.
            slot.motion_ema = motion_ema_step(
                slot.motion_ema,
                body_motion_measure(&smoothed.velocities, slot.target.size),
                dt,
            );
            state.bodies[i] = Some(TrackedBody {
                slot: i,
                present: true,
                fade: slot.fade,
                confidence: slot.target.confidence,
                landmarks: smoothed.landmarks,
                world_landmarks: smoothed.world,
                velocities: smoothed.velocities,
                timestamp: slot.timestamp,
                crop_fraction: slot.target.crop_fraction,
                size: slot.target.size,
                motion: slot.motion_ema,
            });
        } else if slot.fade > 0.0 {
            if let Some(body) = state.bodies[i].as_mut() {
                body.present = false;
                body.fade = slot.fade;
                body.velocities = [Vec3::ZERO; super::BODY_LANDMARK_COUNT];
            } else {
                // Never published (e.g. a held-only blip): nothing to fade.
                slot.fade = 0.0;
            }
        } else if state.bodies[i].is_some() {
            // Fade complete: free the slot; a returning person starts fresh
            // (no stale filter momentum, a fresh admission dwell, and a cold
            // motion envelope), mirroring the hand smoother.
            state.bodies[i] = None;
            slot.smoother.clear();
            slot.admitted = false;
            slot.motion_ema = 0.0;
        }
    }
}

/// Build the rings, seed the payload pool, and spawn the worker. Reads the
/// three Dev-panel tuning fields off `request` (Plan C Task 14): the mask
/// combine-with-previous ratio seeds the shared live-tuning cell the worker
/// polls each frame, and the One-Euro min-cutoff/beta seed the per-slot
/// main-thread smoothers constructed below. Because these fields are
/// `requires_restart` (Plan C Task 2), `sync_body_tracking` only reaches this
/// function on a fresh request insert — including the re-insert after a
/// Dev-panel reload — so a settings change always takes effect on the next
/// worker (re)start. `config.max_tracked_bodies` rides into the worker's
/// pipeline the same way.
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
        let max_tracked = config.max_tracked_bodies;
        Box::new(move || load_pose_pipeline(&model_dir, max_tracked))
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
        // One-Euro params seeded straight from the request (see the function
        // doc), one smoother per slot so bodies never share filter momentum.
        // BodySmoother::set_params exists for retuning an already-running
        // smoother without resetting filter state, but nothing currently
        // calls it mid-run: these fields are requires_restart, so a change
        // always arrives via a fresh start_worker call instead.
        slots: std::array::from_fn(|_| SlotRuntime {
            target: SlotFrame::default(),
            timestamp: Duration::ZERO,
            hold_until: Duration::ZERO,
            admitted: false,
            present_since: None,
            fade: 0.0,
            motion_ema: 0.0,
            smoother: BodySmoother::new(request.one_euro_min_cutoff, request.one_euro_beta),
        }),
        primary: PrimarySelect::default(),
        had_person: false,
    }
}

/// Open a real webcam source on the calling (worker) thread, or error. The
/// same per-platform selection as the hand provider, gated on this
/// modality's camera feature. The returned source is wrapped in a
/// [`crate::input::camera_preview::PreviewTap`] so the settings dock's
/// camera-preview toggle can observe the frames (a single atomic check per
/// frame while the toggle is off).
pub fn open_camera_source(camera_index: u32) -> Result<Box<dyn FrameSource>, CaptureError> {
    #[cfg(all(feature = "body-tracking-camera", target_os = "macos"))]
    {
        let source = crate::input::capture::AvfFrameSource::open(camera_index)?;
        Ok(crate::input::camera_preview::PreviewTap::wrap(Box::new(
            source,
        )))
    }
    #[cfg(all(feature = "body-tracking-camera", not(target_os = "macos")))]
    {
        // Prefer the OBSBot by name, mirroring the hand provider's default: both
        // webcam modalities target the same physical camera on the deployment,
        // and MSMF does not reliably place it at index 0 (a virtual camera or an
        // RDP camera bus may be enumerated first). Falls back to `camera_index`
        // on any box with no matching camera, so non-OBSBot hosts are unchanged.
        let source = crate::input::capture::NokhwaFrameSource::open(camera_index, Some("OBSBOT"))?;
        Ok(crate::input::camera_preview::PreviewTap::wrap(Box::new(
            source,
        )))
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
        two_person_detector_outputs,
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

    fn default_request() -> BodyTrackingRequest {
        BodyTrackingRequest {
            idle_throttle: false,
            mask_ema: super::super::mask::DEFAULT_MASK_EMA_ALPHA,
            one_euro_min_cutoff: DEFAULT_MIN_CUTOFF,
            one_euro_beta: DEFAULT_BETA,
        }
    }

    #[test]
    fn request_starts_worker_and_publishes_state_mask_edges_and_presence() {
        let mut app = body_app(hot_person_detector_outputs(), confident_landmark_outputs());
        app.insert_resource(default_request());

        // Wait for a present beat that also carries edges: admission (the
        // ADMIT_DWELL gate) can first publish presence on a *bridged-dropout*
        // beat of the mock's crop-wobble oscillation, whose payload is a
        // decay frame with an empty edge list — a one-beat transient the
        // next full frame refills.
        let tracked = update_until(&mut app, |w| {
            w.resource::<BodyTrackingState>().any_present()
                && !w.resource::<SilhouetteEdges>().points.is_empty()
        });
        assert!(tracked, "state never reported a person with edges");

        let world = app.world();
        let state = world.resource::<BodyTrackingState>();
        assert_eq!(state.primary, Some(0), "single person occupies slot 0");
        let body = state.primary().expect("primary body");
        assert_eq!(body.slot, 0);
        assert!(body.present);
        assert!(body.fade > 0.0, "fade envelope attacking");
        assert!(body.confidence > 0.8);
        assert!(body.landmarks[0].pos.x.is_finite());
        assert!(body.landmarks[0].visibility > 0.7);
        assert!((body.world_landmarks[0].x - 0.1).abs() < 1e-4);
        assert!(body.crop_fraction > 0.9, "fully-framed synthetic person");
        assert!(body.size > 0.0);

        let edges = world.resource::<SilhouetteEdges>();
        assert!(edges.generation > 0, "edges never refreshed");
        assert!(!edges.points.is_empty());
        assert_eq!(
            edges.slot_counts.iter().sum::<usize>(),
            edges.points.len(),
            "slot counts partition the edge list"
        );
        assert!(edges.slot_counts[0] > 0, "slot 0 owns the edges");

        let mask = world.resource::<MaskTexture>();
        let images = world.resource::<Assets<Image>>();
        let image = images.get(&mask.0).expect("mask image");
        let data = image.data.as_ref().expect("mask image holds CPU data");
        assert_eq!(data.len(), super::super::MASK_BYTES, "RGBA mask bytes");
        assert!(
            data.chunks_exact(4).any(|t| t[0] > 200),
            "slot-0 channel (R) never written to the image"
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
    fn two_people_publish_two_bodies_with_a_primary() {
        let mut app = body_app(
            two_person_detector_outputs(4.0, 2.0),
            confident_landmark_outputs(),
        );
        app.insert_resource(default_request());
        let both = update_until(&mut app, |w| {
            w.resource::<BodyTrackingState>().present_count() >= 2
        });
        assert!(both, "two people never published");
        let state = app.world().resource::<BodyTrackingState>();
        let primary = state.primary.expect("a primary is chosen");
        assert!(state.bodies[primary].as_ref().is_some_and(|b| b.present));
        // Slot indices are stable and distinct.
        let slots: Vec<usize> = state.iter_bodies().map(|b| b.slot).collect();
        assert_eq!(slots, vec![0, 1]);
    }

    #[test]
    fn empty_frames_do_not_touch_the_interaction_timer() {
        let mut app = body_app(empty_detector_outputs(), confident_landmark_outputs());
        app.insert_resource(default_request());
        // Give the worker ample time to stream empty frames.
        for _ in 0..40 {
            app.update();
            std::thread::sleep(Duration::from_millis(5));
        }
        let world = app.world();
        assert!(!world.resource::<BodyTrackingState>().any_present());
        assert_eq!(
            world.resource::<InteractionTimer>().last_interaction(),
            Duration::ZERO,
            "empty frames must never reset the idle timer"
        );
    }

    #[test]
    fn worker_start_reads_tuning_fields_from_the_request() {
        // Plan C Task 14: the three Dev-panel tuning fields on the request
        // must reach the worker's live-tuning cell (mask combine ratio) and
        // every per-slot main-thread smoother (One-Euro min-cutoff/beta) at
        // worker start, not just sit on the struct unread.
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
        for slot in &rt.slots {
            let (min_cutoff, beta) = slot.smoother.params();
            assert!(
                (min_cutoff - 3.5).abs() < 1e-6,
                "one_euro_min_cutoff did not reach a slot smoother: {min_cutoff}"
            );
            assert!(
                (beta - 12.0).abs() < 1e-6,
                "one_euro_beta did not reach a slot smoother: {beta}"
            );
        }
    }

    #[test]
    fn person_cycle_requests_accumulate_and_drain() {
        // The `KeyN` hotkey path: presses accumulate on the lock-free counter
        // and poll_body_worker drains them (each press = one primary cycle;
        // see selection::PrimarySelect::cycle for the policy itself).
        let worker = BodyTrackingWorker::default();
        assert_eq!(worker.take_cycle_requests(), 0);
        worker.request_person_cycle();
        worker.request_person_cycle();
        assert_eq!(worker.take_cycle_requests(), 2, "two presses register");
        assert_eq!(worker.take_cycle_requests(), 0, "drain resets");
    }

    #[test]
    fn removing_the_request_stops_the_worker_and_clears_state() {
        let mut app = body_app(hot_person_detector_outputs(), confident_landmark_outputs());
        app.insert_resource(default_request());
        assert!(update_until(&mut app, |w| w
            .resource::<BodyTrackingState>()
            .any_present()));

        app.world_mut().remove_resource::<BodyTrackingRequest>();
        app.update();

        let world = app.world();
        let state = world.resource::<BodyTrackingState>();
        assert!(!state.any_present());
        assert!(state.bodies.iter().all(Option::is_none));
        assert!(state.primary.is_none());
        assert!(world.resource::<SilhouetteEdges>().points.is_empty());
        let worker = world.resource::<BodyTrackingWorker>();
        assert!(
            worker.runtime.lock().expect("runtime lock").is_none(),
            "worker must be stopped and joined on request removal"
        );
    }
}
