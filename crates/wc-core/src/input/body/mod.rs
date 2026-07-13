//! Webcam body tracking: MediaPipe BlazePose person detection → ROI → 33
//! landmarks, metric world landmarks, and a 256×256 person segmentation mask.
//!
//! A parallel seam beside hand tracking — not a
//! [`crate::input::provider::HandTrackingProvider`] implementation (that trait
//! bakes in 21-landmark hands). One worker thread copies the proven mediapipe
//! worker shape; results publish as plain resources the Radiance sketch
//! consumes. See `BodyTrackingPlugin` (below) for the full data flow.
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
pub mod mask;
pub mod pipeline;
pub mod roi;
pub mod smoothing;
pub mod systems;
pub mod transport;
pub mod worker;

/// Number of `BlazePose` body landmarks published to consumers.
pub const BODY_LANDMARK_COUNT: usize = 33;

/// Fixed capacity of the silhouette edge list ([`SilhouetteEdges`]).
pub const MAX_EDGE_POINTS: usize = 2048;

/// Side length of the person segmentation mask (256×256, `R8Unorm`).
pub const MASK_SIZE: usize = 256;

/// [`MASK_SIZE`] as `u32` for texture extents (pinned equal by a test, so no
/// runtime conversion is ever needed).
pub const MASK_SIZE_U32: u32 = 256;

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
    /// Worker-side temporal EMA factor on the segmentation mask (0 = raw,
    /// higher = steadier/laggier). Read at worker (re)start
    /// (`systems::start_worker` seeds [`pipeline::BodyLiveTuning`] from this
    /// field); Radiance's Dev knob routes here via its `requires_restart`
    /// reload.
    pub mask_ema: f32,
    /// One-Euro landmark filter min-cutoff, Hz. Same routing as
    /// [`Self::mask_ema`]; seeds `systems::start_worker`'s
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

/// Continuous body-tracking snapshot. Always present once
/// [`BodyTrackingPlugin`] is added; `present == false` when there is no
/// request or no person. Landmarks and world landmarks are One-Euro smoothed
/// at poll rate; velocities are the smoothed screen-space derivatives.
#[derive(Resource, Clone, Debug)]
pub struct BodyTrackingState {
    /// Whether a person is currently tracked.
    pub present: bool,
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
}

impl Default for BodyTrackingState {
    fn default() -> Self {
        Self {
            present: false,
            confidence: 0.0,
            landmarks: [BodyLandmark::default(); BODY_LANDMARK_COUNT],
            world_landmarks: [Vec3::ZERO; BODY_LANDMARK_COUNT],
            velocities: [Vec3::ZERO; BODY_LANDMARK_COUNT],
            timestamp: Duration::ZERO,
        }
    }
}

/// Handle to the reused 256×256 `R8Unorm` person-mask image (EMA-smoothed).
/// Mask bytes are written in place each body frame; Bevy re-uploads on
/// mutation. Inserted at startup when `Assets<Image>` exists (i.e. in any app
/// with the asset plugin; absent in bare headless harnesses).
#[derive(Resource, Clone)]
pub struct MaskTexture(pub Handle<Image>);

/// One silhouette edge sample: position + outward normal, both in mask UV
/// space. `#[repr(C)]` + `Pod` so Plan C can upload the whole list as a
/// storage buffer with `bytemuck`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct EdgePoint {
    /// Position in mask UV space `0..1`.
    pub pos: Vec2,
    /// Outward unit normal (points from inside the person toward outside).
    pub normal: Vec2,
}

/// CPU edge list extracted on the worker where the EMA-smoothed mask crosses
/// 0.5. Refilled in place (`clear()`, never realloc — capacity is
/// [`MAX_EDGE_POINTS`] by construction).
#[derive(Resource)]
pub struct SilhouetteEdges {
    /// Edge samples for the latest body frame.
    pub points: Vec<EdgePoint>,
    /// Bumped on each new body frame so consumers can skip re-upload.
    pub generation: u64,
}

impl Default for SilhouetteEdges {
    fn default() -> Self {
        Self {
            points: Vec::with_capacity(MAX_EDGE_POINTS),
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
        }
    }
}

/// Construction-time configuration (camera index, rate cap, model directory).
/// Inserted with defaults by the plugin; override before the first
/// [`BodyTrackingRequest`] to change it.
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
}

impl Default for BodyTrackingConfig {
    fn default() -> Self {
        Self {
            camera_index: 0,
            max_inference_hz: 30,
            model_dir: crate::platform::assets::asset_root().join("models/pose"),
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
    ///   camera FrameSource → PosePipeline (detector → ROI → landmarks/mask/edges)
    ///     → rtrb result ring (BodyWorkerMsg; mask via the recycled payload pool)
    ///   └─ systems::poll_body_worker     — drains the ring, One-Euro smooths,
    ///        writes BodyTrackingState + MaskTexture bytes + SilhouetteEdges,
    ///        recycles payloads, marks InteractionTimer on person-bearing frames
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
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "exact equality against Default-derived zero literals, not computed values"
    )]
    fn state_defaults_are_neutral() {
        let s = BodyTrackingState::default();
        assert!(!s.present);
        assert_eq!(s.confidence, 0.0);
        assert_eq!(s.landmarks[0].visibility, 0.0);
        assert_eq!(s.world_landmarks[32], Vec3::ZERO);
        assert_eq!(s.velocities[15], Vec3::ZERO);
        assert_eq!(s.timestamp, Duration::ZERO);
    }

    #[test]
    fn edge_point_is_pod_with_gpu_layout() {
        // Plan C uploads SilhouetteEdges as a storage buffer via bytemuck; the
        // layout must be two tightly-packed vec2<f32>s (16 bytes).
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
        assert_eq!(e.generation, 0);
    }

    #[test]
    fn mask_size_constants_agree() {
        assert_eq!(usize::try_from(MASK_SIZE_U32), Ok(MASK_SIZE));
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
