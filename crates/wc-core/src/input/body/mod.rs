//! Webcam body tracking: MediaPipe BlazePose person detection → ROI → 33
//! landmarks, metric world landmarks, and a 256×256 per-person segmentation
//! mask — for up to [`MAX_TRACKED_BODIES`](crate::input::body::MAX_TRACKED_BODIES)
//! people simultaneously.
//!
//! (Intra-doc links in this module doc are crate-qualified: the `pub mod
//! body;` declaration carries its own `///` summary, and rustdoc resolves a
//! combined doc's links in the outer scope, where bare item names here would
//! dangle.)
//!
//! A parallel seam beside hand tracking — not a
//! [`crate::input::provider::HandTrackingProvider`] implementation (that trait
//! bakes in 21-landmark hands). One worker thread copies the proven mediapipe
//! worker shape; results publish as plain resources the Radiance sketch
//! consumes. See `BodyTrackingPlugin` (below) for the full data flow.
//!
//! ## Multi-person model
//!
//! Each tracked person occupies a **stable slot** `0..MAX_TRACKED_BODIES` for
//! their whole visit: the worker associates detector candidates to slots by
//! centroid distance
//! ([`selection::assign_slots`](crate::input::body::selection::assign_slots)),
//! holds a lost slot in reserve so a brief occlusion re-acquires the *same*
//! slot, and the publisher frees a slot only once its fade-out envelope
//! ([`TrackedBody::fade`](crate::input::body::TrackedBody::fade)) reaches
//! zero.
//! [`BodyTrackingState::primary`](crate::input::body::BodyTrackingState::primary)
//! names the featured slot — the largest well-framed person, with hysteresis
//! ([`selection::PrimarySelect`](crate::input::body::selection::PrimarySelect)).
//!
//! ## Mask channel convention (pinned)
//!
//! [`MaskTexture`](crate::input::body::MaskTexture) is a 256×256
//! **`Rgba8Unorm`** image; **channel `i` is slot `i`'s segmentation
//! coverage** (slot 0 = R, 1 = G, 2 = B, 3 = A). A consumer renders one body
//! by sampling `dot(textureSample(...), channel_select)` with a one-hot
//! channel-select vector, or all bodies by using all four channels.
//!
//! Activation contract: some sketch (Radiance) INSERTS `BodyTrackingRequest`
//! to start the camera + worker and REMOVES it to stop them. While a request
//! exists, a person-bearing frame resets the idle
//! `InteractionTimer` with the same semantics as hand-bearing frames in
//! `reset_on_interaction` (empty frames are ignored).

use std::path::PathBuf;
use std::time::Duration;

use bevy::prelude::*;
use bytemuck::{Pod, Zeroable};

pub mod detector;
pub mod edges;
pub mod envelope;
pub mod mask;
pub mod pipeline;
pub mod roi;
pub mod selection;
pub mod smoothing;
pub mod systems;
pub mod transport;
pub mod worker;

/// Number of `BlazePose` body landmarks published to consumers.
pub const BODY_LANDMARK_COUNT: usize = 33;

/// Maximum number of simultaneously tracked people. Matches the detector's
/// weighted-NMS candidate cap (`detector::MAX_PERSON_CANDIDATES`) and the
/// four channels of the RGBA [`MaskTexture`]; a fifth person in frame is
/// simply not tracked until a slot frees.
pub const MAX_TRACKED_BODIES: usize = 4;

/// Fixed capacity of the silhouette edge list ([`SilhouetteEdges`]), shared
/// across all slots.
pub const MAX_EDGE_POINTS: usize = 2048;

/// Side length of the person segmentation mask (256×256).
pub const MASK_SIZE: usize = 256;

/// [`MASK_SIZE`] as `u32` for texture extents (pinned equal by a test, so no
/// runtime conversion is ever needed).
pub const MASK_SIZE_U32: u32 = 256;

/// Channels in the [`MaskTexture`] image (`Rgba8Unorm`): channel `i` = slot
/// `i`'s coverage. Pinned equal to [`MAX_TRACKED_BODIES`] by a test.
pub const MASK_CHANNELS: usize = 4;

/// Total byte length of one mask image / pooled mask payload
/// (`MASK_SIZE² × MASK_CHANNELS`, RGBA interleaved).
pub const MASK_BYTES: usize = MASK_SIZE * MASK_SIZE * MASK_CHANNELS;

/// `MediaPipe` pose landmark indices for the subset Plan C uses as limb-impulse
/// sources. The full 33-point topology is the standard `BlazePose` layout.
pub mod landmark_index {
    /// Head reference point.
    pub const NOSE: usize = 0;
    /// Left wrist.
    pub const LEFT_WRIST: usize = 15;
    /// Right wrist.
    pub const RIGHT_WRIST: usize = 16;
    /// Left hip.
    pub const LEFT_HIP: usize = 23;
    /// Right hip.
    pub const RIGHT_HIP: usize = 24;
    /// Left ankle.
    pub const LEFT_ANKLE: usize = 27;
    /// Right ankle.
    pub const RIGHT_ANKLE: usize = 28;
}

/// Activation contract: INSERT this resource to start the worker + camera;
/// REMOVE it to stop. Sketch-agnostic — Plan C inserts it
/// `OnEnter(Radiance)` and removes it `OnExit`.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct BodyTrackingRequest {
    /// `true` during `Idle`/`Screensaver`: the worker drops to a detector-only
    /// presence probe at the shared idle rate (4 Hz class, hardware capture
    /// throttle included) so a person walking up still re-activates the
    /// sketch. Driven by Plan C from `SketchActivity`.
    pub idle_throttle: bool,
    /// Worker-side mask temporal-blend strength — `MediaPipe`'s
    /// uncertainty-weighted `combine_with_previous_ratio` (0 = raw new frame,
    /// higher = more previous frame blended into boundary pixels =
    /// steadier/laggier). Field name kept `mask_ema` for continuity; it is a
    /// combine ratio, not an EMA alpha. Read at worker (re)start
    /// (`systems::start_worker` seeds [`pipeline::BodyLiveTuning`] from this
    /// field); Radiance's Dev knob routes here via its `requires_restart`
    /// reload.
    pub mask_ema: f32,
    /// One-Euro landmark filter min-cutoff, Hz. Same routing as
    /// [`Self::mask_ema`]; seeds `systems::start_worker`'s per-slot
    /// [`smoothing::BodySmoother`] construction.
    pub one_euro_min_cutoff: f32,
    /// One-Euro landmark filter beta (speed coefficient). Same routing as
    /// [`Self::mask_ema`].
    pub one_euro_beta: f32,
}

/// One tracked body landmark in mask-UV space.
#[derive(Clone, Copy, Debug, Default)]
pub struct BodyLandmark {
    /// `x`,`y` screen-normalized `0..1` in mask UV space (the camera content
    /// rect — the same space [`MaskTexture`] texels live in); `z` is the
    /// model's relative depth (ROI-scaled, not metric).
    pub pos: Vec3,
    /// Per-landmark visibility probability in `0..1`.
    pub visibility: f32,
}

/// One tracked person, published per slot in [`BodyTrackingState::bodies`].
///
/// The `slot` index is **stable for the person's whole visit**: it is chosen
/// on first detection, survives brief occlusions (worker-side reservation +
/// re-association), and is freed only once [`Self::fade`] has released to
/// zero. Landmarks and world landmarks are One-Euro smoothed at poll rate;
/// velocities are the smoothed screen-space derivatives.
#[derive(Clone, Debug)]
pub struct TrackedBody {
    /// This body's stable slot index (`0..MAX_TRACKED_BODIES`). Also selects
    /// the body's [`MaskTexture`] channel and its `SilhouetteEdges` range.
    pub slot: usize,
    /// Debounced presence (post presence-hold): `true` while the person is
    /// tracked or briefly held; `false` during the fade-out tail.
    pub present: bool,
    /// Graceful appearance envelope `0..1`: eases up over
    /// ~[`envelope::FADE_ATTACK_TAU`] while present, releases over
    /// ~[`envelope::FADE_RELEASE_TAU`] when the person leaves. The slot (and
    /// this entry) is removed only when it reaches 0 — render alpha for this
    /// body should ride it.
    pub fade: f32,
    /// Track confidence (the landmark model's pose-presence probability, or
    /// the detector score while in the idle detector-only probe).
    pub confidence: f32,
    /// Screen-normalized landmarks + visibility (mask UV space).
    pub landmarks: [BodyLandmark; BODY_LANDMARK_COUNT],
    /// Metric world landmarks (metres, hip-centred), One-Euro smoothed.
    pub world_landmarks: [Vec3; BODY_LANDMARK_COUNT],
    /// Smoothed landmark velocities, screen-normalized units/sec.
    pub velocities: [Vec3; BODY_LANDMARK_COUNT],
    /// Worker-relative capture timestamp of the underlying inference frame.
    pub timestamp: Duration,
    /// Fraction of this person's bbox inside the camera frame (`1.0` = fully
    /// visible, `≈ 0.5` = half off-edge). Feeds the primary-selection crop
    /// penalty ([`selection::crop_weight`]).
    pub crop_fraction: f32,
    /// Normalized bbox area (square-norm units²) — the closest-person proxy
    /// used by [`selection::primary_score`].
    pub size: f32,
}

impl Default for TrackedBody {
    fn default() -> Self {
        Self {
            slot: 0,
            present: false,
            fade: 0.0,
            confidence: 0.0,
            landmarks: [BodyLandmark::default(); BODY_LANDMARK_COUNT],
            world_landmarks: [Vec3::ZERO; BODY_LANDMARK_COUNT],
            velocities: [Vec3::ZERO; BODY_LANDMARK_COUNT],
            timestamp: Duration::ZERO,
            crop_fraction: 0.0,
            size: 0.0,
        }
    }
}

/// Continuous multi-body tracking snapshot. Always present once
/// [`BodyTrackingPlugin`] is added; all slots `None` when there is no request
/// or no people. Indexed by stable slot: `bodies[i]` is `Some` from a
/// person's first appearance until their fade-out completes.
#[derive(Resource, Clone, Debug, Default)]
pub struct BodyTrackingState {
    /// Per-slot tracked bodies (`None` = slot free).
    pub bodies: [Option<TrackedBody>; MAX_TRACKED_BODIES],
    /// Slot of the featured person (the largest well-framed body, with switch
    /// hysteresis; the `KeyN` debug hotkey cycles it). `None` when nobody is
    /// present. Always indexes a `Some`, *present* body.
    pub primary: Option<usize>,
}

impl BodyTrackingState {
    /// The featured body, if any — the back-compat accessor single-body
    /// consumers migrate to (`state.primary()` where they used the old
    /// whole-resource fields).
    #[must_use]
    pub fn primary(&self) -> Option<&TrackedBody> {
        self.primary
            .and_then(|slot| self.bodies.get(slot))
            .and_then(Option::as_ref)
    }

    /// Iterate every occupied slot (present bodies and fading-out ones).
    pub fn iter_bodies(&self) -> impl Iterator<Item = &TrackedBody> {
        self.bodies.iter().flatten()
    }

    /// Whether any slot holds a *present* (post-hold) body.
    #[must_use]
    pub fn any_present(&self) -> bool {
        self.iter_bodies().any(|b| b.present)
    }

    /// Number of present bodies.
    #[must_use]
    pub fn present_count(&self) -> usize {
        self.iter_bodies().filter(|b| b.present).count()
    }
}

/// Handle to the reused 256×256 **`Rgba8Unorm`** multi-person mask image
/// (temporally-blended). **Channel `i` = slot `i`'s coverage** (slot 0 = R,
/// 1 = G, 2 = B, 3 = A) — the pinned channel convention (see the module doc).
/// Mask bytes are written in place each body frame; Bevy re-uploads on
/// mutation. Inserted at startup when `Assets<Image>` exists (i.e. in any app
/// with the asset plugin; absent in bare headless harnesses).
#[derive(Resource, Clone)]
pub struct MaskTexture(pub Handle<Image>);

/// One silhouette edge sample: position + outward normal, both in mask UV
/// space. `#[repr(C)]` + `Pod` so Plan C can upload the whole list as a
/// storage buffer with `bytemuck`.
///
/// Deliberately carries **no slot field**: the 16-byte two-`vec2<f32>` layout
/// is a pinned GPU contract (Plan C's edge storage buffer + WGSL struct).
/// Slot ownership lives in [`SilhouetteEdges::slot_counts`] instead — the
/// list is concatenated in ascending slot order, so a consumer that needs
/// per-body edges slices by the counts, and the existing whole-list GPU
/// upload path is unchanged.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct EdgePoint {
    /// Position in mask UV space `0..1`.
    pub pos: Vec2,
    /// Outward unit normal (points from inside the person toward outside).
    pub normal: Vec2,
}

/// CPU edge list extracted on the worker where each slot's temporally-blended
/// mask crosses 0.5. Refilled in place (`clear()`, never realloc — capacity
/// is [`MAX_EDGE_POINTS`] by construction).
///
/// `points` holds all slots' edges **concatenated in ascending slot order**;
/// `slot_counts[i]` is slot `i`'s length, so slot `i`'s edges are
/// `points[slot_counts[..i].sum() .. +slot_counts[i]]`. The total is capped
/// at [`MAX_EDGE_POINTS`] (earlier slots fill first when crowded).
#[derive(Resource)]
pub struct SilhouetteEdges {
    /// Edge samples for the latest body frame, slot-ordered (see above).
    pub points: Vec<EdgePoint>,
    /// Per-slot edge counts partitioning `points`.
    pub slot_counts: [usize; MAX_TRACKED_BODIES],
    /// Bumped on each new body frame so consumers can skip re-upload.
    pub generation: u64,
}

impl Default for SilhouetteEdges {
    fn default() -> Self {
        Self {
            points: Vec::with_capacity(MAX_EDGE_POINTS),
            slot_counts: [0; MAX_TRACKED_BODIES],
            generation: 0,
        }
    }
}

/// Coarse body-tracking lifecycle state, surfaced in diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BodyTrackingStatus {
    /// No request; nothing running.
    #[default]
    Inactive,
    /// Worker spawned; models/camera still coming up.
    Starting,
    /// Camera frames flowing through the pipeline.
    Streaming,
    /// The camera could not be opened or a read failed.
    CameraUnavailable,
    /// Model load/session build failed (see `last_error`).
    Failed,
}

impl BodyTrackingStatus {
    /// Static label for panels/logs (no per-frame allocation).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::Starting => "starting",
            Self::Streaming => "streaming",
            Self::CameraUnavailable => "camera unavailable",
            Self::Failed => "failed",
        }
    }
}

/// Body-tracking diagnostics: backend label (a silent CPU fallback must be
/// visible, matching hand-tracking practice), status, and worker counters.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct BodyTrackingDiagnostics {
    /// Lifecycle status.
    pub status: BodyTrackingStatus,
    /// Inference backend label (`"ort/CoreML"`, `"ort/CPU"`, mixed states) as
    /// reported by the worker after building its sessions.
    pub backend: &'static str,
    /// Negotiated camera format label, when the source reports one.
    pub camera_format: Option<String>,
    /// Most recent worker/pipeline error string.
    pub last_error: Option<String>,
    /// Wall time between the last two processed frames (effective inference
    /// period).
    pub inference_interval: Duration,
    /// Cumulative camera frames dropped by the rate cap / idle throttle.
    pub dropped_frames: u64,
    /// Cumulative ring-buffer backpressure drops (slow consumer, not camera).
    pub ring_full_drops: u64,
    /// Cumulative pipeline (inference) errors.
    pub pipeline_errors: u64,
    /// Whether the idle detector-only throttle is currently requested.
    pub idle_throttled: bool,
    /// Wall time acquiring + decoding the last processed frame. Separates a
    /// slow camera/decode from slow inference on hardware — the same
    /// thermal-diagnosis split the hand provider surfaces in its dev panel.
    pub capture_decode: Duration,
    /// Per-stage pipeline metrics (total/preprocess/detector/landmark) for
    /// the latest processed frame.
    pub pipeline: pipeline::PoseDiagnostics,
}

impl Default for BodyTrackingDiagnostics {
    fn default() -> Self {
        Self {
            status: BodyTrackingStatus::Inactive,
            backend: "not started",
            camera_format: None,
            last_error: None,
            inference_interval: Duration::ZERO,
            dropped_frames: 0,
            ring_full_drops: 0,
            pipeline_errors: 0,
            idle_throttled: false,
            capture_decode: Duration::ZERO,
            pipeline: pipeline::PoseDiagnostics::default(),
        }
    }
}

/// Construction-time configuration (camera index, rate cap, model directory,
/// tracked-body cap). Inserted with defaults by the plugin; override before
/// the first [`BodyTrackingRequest`] to change it.
#[derive(Resource, Clone, Debug)]
pub struct BodyTrackingConfig {
    /// Camera index to open (0 = default device).
    pub camera_index: u32,
    /// Full-rate inference cap in Hz (0 = uncapped). 30 matches the hand
    /// provider: body tracking does not need full frame rate, and capping
    /// leaves CPU/thermal headroom.
    pub max_inference_hz: u32,
    /// Directory holding `pose_detection.onnx` and `pose_landmark_full.onnx`.
    /// Resolved at runtime via `platform::assets::asset_root` so the path is
    /// correct in dev, release, and macOS `.app` bundle deployments.
    pub model_dir: PathBuf,
    /// Maximum concurrently tracked people, clamped to
    /// `1..=`[`MAX_TRACKED_BODIES`] at worker start. With ≤ 2 active tracks
    /// every track runs landmark/mask inference every frame; with ≥ 3 the
    /// worker interleaves (round-robin, ~2 tracks per frame), holding the
    /// last mask/landmarks for skipped tracks (they are generation-gated
    /// downstream anyway).
    pub max_tracked_bodies: usize,
}

impl Default for BodyTrackingConfig {
    fn default() -> Self {
        Self {
            camera_index: 0,
            max_inference_hz: 30,
            model_dir: crate::platform::assets::asset_root().join("models/pose"),
            max_tracked_bodies: MAX_TRACKED_BODIES,
        }
    }
}

/// Wires body tracking into the Bevy [`App`].
pub struct BodyTrackingPlugin;

impl Plugin for BodyTrackingPlugin {
    /// Data flow:
    ///
    /// ```text
    /// BodyTrackingRequest (sketch inserts/removes; idle_throttle mirrors SketchActivity)
    ///   └─ systems::sync_body_tracking   — spawns/stops the worker, mirrors the throttle
    /// worker thread (systems-spawned):
    ///   camera FrameSource → PosePipeline (detector → slot association →
    ///     per-slot ROI → per-slot landmarks/mask-channel/edges, round-robin
    ///     when > 2 active) → rtrb result ring (BodyWorkerMsg; RGBA mask via
    ///     the recycled payload pool)
    ///   └─ systems::poll_body_worker     — drains the ring, per-slot
    ///        presence-hold + fade envelope + One-Euro smoothing, primary
    ///        selection, writes BodyTrackingState + MaskTexture bytes +
    ///        SilhouetteEdges, recycles payloads, marks InteractionTimer on
    ///        person-bearing frames
    /// ```
    ///
    /// Both `PreUpdate` systems run under `InputSystems` (like the hand
    /// subsystem) and are internally gated on the request/runtime: with no
    /// request they are two early-outs per frame — the sanctioned always-on
    /// listener shape (they must observe a request inserted from any state).
    fn build(&self, app: &mut App) {
        app.init_resource::<BodyTrackingState>()
            .init_resource::<BodyTrackingDiagnostics>()
            .init_resource::<SilhouetteEdges>()
            .init_resource::<BodyTrackingConfig>()
            .init_resource::<systems::BodyTrackingWorker>()
            .add_systems(Startup, systems::init_mask_texture)
            .add_systems(
                PreUpdate,
                (systems::sync_body_tracking, systems::poll_body_worker)
                    .chain()
                    .in_set(bevy::input::InputSystems),
            );
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;

    #[test]
    fn state_defaults_are_neutral() {
        let s = BodyTrackingState::default();
        assert!(s.bodies.iter().all(Option::is_none));
        assert!(s.primary.is_none());
        assert!(s.primary().is_none());
        assert!(!s.any_present());
        assert_eq!(s.present_count(), 0);
    }

    #[test]
    fn primary_accessor_indexes_the_bodies_array() {
        let mut s = BodyTrackingState::default();
        s.bodies[2] = Some(TrackedBody {
            slot: 2,
            present: true,
            fade: 1.0,
            confidence: 0.9,
            ..TrackedBody::default()
        });
        s.primary = Some(2);
        let p = s.primary().expect("primary body");
        assert_eq!(p.slot, 2);
        assert!(p.present);
        assert_eq!(s.present_count(), 1);
        // A primary index onto an empty slot degrades to None, not a panic.
        s.primary = Some(0);
        assert!(s.primary().is_none());
    }

    #[test]
    fn edge_point_is_pod_with_gpu_layout() {
        // Plan C uploads SilhouetteEdges as a storage buffer via bytemuck; the
        // layout must be two tightly-packed vec2<f32>s (16 bytes). This is the
        // pinned contract that keeps EdgePoint slot-free (slot ownership lives
        // in SilhouetteEdges::slot_counts).
        assert_eq!(std::mem::size_of::<EdgePoint>(), 16);
        assert_eq!(std::mem::offset_of!(EdgePoint, pos), 0);
        assert_eq!(std::mem::offset_of!(EdgePoint, normal), 8);
        let p = EdgePoint {
            pos: Vec2::new(0.25, 0.5),
            normal: Vec2::new(0.0, -1.0),
        };
        assert_eq!(bytemuck::bytes_of(&p).len(), 16);
    }

    #[test]
    fn silhouette_edges_preallocates_full_capacity() {
        let e = SilhouetteEdges::default();
        assert!(e.points.is_empty());
        assert_eq!(e.points.capacity(), MAX_EDGE_POINTS);
        assert_eq!(e.slot_counts, [0; MAX_TRACKED_BODIES]);
        assert_eq!(e.generation, 0);
    }

    #[test]
    fn mask_size_constants_agree() {
        assert_eq!(usize::try_from(MASK_SIZE_U32), Ok(MASK_SIZE));
        assert_eq!(MASK_CHANNELS, MAX_TRACKED_BODIES);
        assert_eq!(MASK_BYTES, MASK_SIZE * MASK_SIZE * 4);
        assert_eq!(MAX_TRACKED_BODIES, detector::MAX_PERSON_CANDIDATES);
    }

    #[test]
    fn diagnostics_default_shows_not_started_backend() {
        let d = BodyTrackingDiagnostics::default();
        assert_eq!(d.status, BodyTrackingStatus::Inactive);
        assert_eq!(d.backend, "not started");
        assert!(d.last_error.is_none());
    }

    #[test]
    fn config_defaults_mirror_the_hand_provider() {
        let c = BodyTrackingConfig::default();
        assert_eq!(c.camera_index, 0);
        assert_eq!(c.max_inference_hz, 30);
        assert_eq!(c.max_tracked_bodies, MAX_TRACKED_BODIES);
        assert!(c.model_dir.ends_with("models/pose"));
    }

    #[test]
    fn plugin_shell_initializes_resources() {
        let mut app = App::new();
        app.add_plugins(BodyTrackingPlugin);
        assert!(app.world().contains_resource::<BodyTrackingState>());
        assert!(app.world().contains_resource::<BodyTrackingDiagnostics>());
        assert!(app.world().contains_resource::<SilhouetteEdges>());
        assert!(app.world().contains_resource::<BodyTrackingConfig>());
    }
}
