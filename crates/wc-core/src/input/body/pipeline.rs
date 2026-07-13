//! Two-stage `BlazePose` pipeline: a camera `Frame` in, landmarks + world
//! landmarks + a warped/temporally-blended mask + silhouette edges out.
//!
//! Flow per frame: square-pad the frame; run the person detector ONLY when no
//! track is carried (detect-then-track — the aux landmark rows 33/34 supply
//! next frame's ROI, so a healthy track never pays the detector); warp the
//! rotated ROI into a 256² crop; run the landmark model; gate on its
//! pose-presence scalar; project the 39 rows back to square-norm; heavily
//! One-Euro filter the aux alignment rows before deriving next frame's
//! tracking ROI so the crop does not jitter; publish the first 33 in
//! content-norm (mask UV space); de-rotate the metric world landmarks by the
//! ROI rotation; warp + uncertainty-blend the segmentation mask; extract
//! silhouette edges into the pooled payload.
//!
//! The **idle detector-only probe** (`detector_only = true`) runs just the
//! detector as a presence sensor at the idle rate: landmarks/mask stages are
//! skipped, the carried track is cleared (stale after idle), and the mask
//! EMA decays so no stale silhouette lingers.
//!
//! All scratch (pad/resize/warp images, input/output tensors, decode buffer,
//! mask processor) is owned by the pipeline and refilled in place — the
//! steady-state frame path allocates nothing. Image helpers are adapted from
//! the validated hand pipeline (same conventions: `/255` RGB NHWC, square-pad
//! to the larger side, bilinear warp/resize with clamp-to-edge).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bevy::math::{Vec2, Vec3};
use image::RgbImage;

use super::detector::{
    best_person, decode_pose_detections_into, generate_pose_anchors, sigmoid, Anchor,
    PersonDetection, DETECTOR_INPUT, POSE_ANCHOR_COUNT, POSE_REGRESSION_LEN,
};
use super::edges::extract_edges;
use super::mask::{MaskProcessor, DEFAULT_MASK_EMA_ALPHA};
use super::roi::{
    project_body_landmarks, roi_from_alignment_points, roi_from_detection, roi_trackable,
    ContentRect, RoiRect, AUX_CENTER_ROW, AUX_SCALE_ROW, LANDMARK_INPUT, LANDMARK_ROWS,
    LANDMARK_VALUES,
};
use super::smoothing::OneEuroFilter;
use super::transport::BodyFramePayload;
use super::{BodyLandmark, BODY_LANDMARK_COUNT, MASK_SIZE};
use crate::input::capture::Frame;
use crate::input::onnx::{InferenceError, ModelInference, Tensor};

/// Landmark model input side as `u32` (the warp target).
const LM_SIZE: u32 = 256;

/// Heatmap tensor side: the pose landmark model emits a
/// `[1, HEATMAP_SIZE, HEATMAP_SIZE, LANDMARK_ROWS]` refinement heatmap
/// (NHWC, batch 1) alongside the regression head.
const HEATMAP_SIZE: usize = 64;

/// Refinement kernel window side. `MediaPipe`'s pose graph
/// (`modules/pose_landmark/tensors_to_pose_landmarks_and_segmentation.pbtxt`)
/// sets `RefineLandmarksFromHeatmapCalculator { kernel_size: 7 }`.
const HEATMAP_KERNEL_SIZE: usize = 7;

/// Minimum in-window max sigmoid confidence to accept a refinement
/// (`min_confidence_to_refine`). The pose graph leaves it unset, so the proto
/// default `0.5` applies (`refine_landmarks_from_heatmap_calculator.proto`).
/// Below this the landmark keeps its raw regression-head x/y.
const HEATMAP_MIN_CONFIDENCE: f32 = 0.5;

/// `IoU` threshold for blending detections around the argmax seed
/// (`MediaPipe`'s `min_suppression_threshold: 0.3`).
const PERSON_BLEND_IOU: f32 = 0.3;

/// Aux-landmark One-Euro min cutoff (Hz). `MediaPipe` smooths the aux
/// alignment rows *much harder* than the main landmarks so the tracking crop
/// stays rock-steady when the subject is still yet stays responsive to sudden
/// movement (`pose_landmark_filtering.pbtxt` aux bank: `one_euro_filter
/// { min_cutoff: 0.01 beta: 10.0 derivate_cutoff: 1.0 }`).
const AUX_MIN_CUTOFF: f32 = 0.01;

/// Aux-landmark One-Euro speed coefficient (`beta: 10.0`; see
/// [`AUX_MIN_CUTOFF`]).
const AUX_BETA: f32 = 10.0;

/// Floor for the aux object scale so a degenerate (collapsed) aux pair never
/// divides the adaptive-cutoff speed by ~0.
const AUX_MIN_OBJECT_SCALE: f32 = 0.05;

/// Heavy two-point One-Euro filter over the aux alignment rows (33 = tracking
/// ROI centre, 34 = circumscribing-circle point) applied **before** the
/// next-frame tracking ROI is derived. Without it, the raw per-frame aux rows
/// jitter the crop centre/size/rotation, and everything warped from that crop
/// (landmarks + mask + edges) inherits the jitter.
///
/// Mirrors `MediaPipe`'s aux `LandmarksSmoothingCalculator` (see the `AUX_*`
/// consts). Upstream connects `OBJECT_SCALE_ROI` to the aux bank with **no**
/// `disable_value_scaling`, so — like the main landmarks — the speed driving
/// the adaptive cutoff is normalized by the pose's apparent object scale. That
/// scale is the aux alignment box side, `2·|scale − centre|`, computed from the
/// **raw** points each frame (matching upstream's `OBJECT_SCALE_ROI`, built
/// from the unfiltered aux landmarks). Fixed arrays — no allocation on the
/// frame path.
struct AuxRoiFilter {
    /// Per point (0 = centre, 1 = scale) x-channel One-Euro filters.
    x: [OneEuroFilter; 2],
    /// Per point y-channel One-Euro filters.
    y: [OneEuroFilter; 2],
    /// Monotonic time of the previous filtered frame; `None` until the first.
    last_now: Option<Duration>,
}

impl AuxRoiFilter {
    /// Build the four aux channels with the fixed upstream aux params.
    fn new() -> Self {
        let ch = || OneEuroFilter::new(AUX_MIN_CUTOFF, AUX_BETA);
        Self {
            x: [ch(), ch()],
            y: [ch(), ch()],
            last_now: None,
        }
    }

    /// Forget history so a newly-acquired track starts fresh: a returning (or
    /// different) person must not inherit the previous track's filter state.
    fn reset(&mut self) {
        for f in &mut self.x {
            f.reset();
        }
        for f in &mut self.y {
            f.reset();
        }
        self.last_now = None;
    }

    /// Filter the raw aux `center`/`scale_point` at time `now`, returning the
    /// smoothed pair for ROI derivation. The first sample after a
    /// [`Self::reset`] passes through (cold start).
    fn filter(&mut self, center: Vec2, scale_point: Vec2, now: Duration) -> (Vec2, Vec2) {
        let dt = self
            .last_now
            .map_or(0.0, |prev| now.saturating_sub(prev).as_secs_f32());
        self.last_now = Some(now);
        // Object scale from the RAW points (upstream's OBJECT_SCALE_ROI is
        // built from unfiltered aux landmarks); value_scale divides the speed.
        let object_scale = (2.0 * (scale_point - center).length()).max(AUX_MIN_OBJECT_SCALE);
        let value_scale = 1.0 / object_scale;
        let c = Vec2::new(
            self.x[0].filter(center.x, dt, value_scale),
            self.y[0].filter(center.y, dt, value_scale),
        );
        let s = Vec2::new(
            self.x[1].filter(scale_point.x, dt, value_scale),
            self.y[1].filter(scale_point.y, dt, value_scale),
        );
        (c, s)
    }
}

/// Tunables for the pose pipeline.
#[derive(Debug, Clone)]
pub struct PoseConfig {
    /// Minimum detector score to accept a person (`min_score_thresh: 0.5`).
    pub detector_score_threshold: f32,
    /// Minimum pose-presence probability from the landmark model to keep the
    /// track (matches `MediaPipe`'s default tracking confidence).
    pub presence_threshold: f32,
    /// Mask temporal-blend combine-with-previous ratio (see
    /// `mask::DEFAULT_MASK_EMA_ALPHA` and `mask::uncertainty_blend`);
    /// live-tunable through [`BodyLiveTuning`]. Field name kept as `mask_ema*`
    /// for continuity; its meaning is the combine ratio, not an EMA alpha.
    pub mask_ema_alpha: f32,
    /// Skip the heatmap landmark refinement pass (upstream
    /// `RefineLandmarksFromHeatmapCalculator`). Seeded at worker build from
    /// `WC_DEBUG_DISABLE_HEATMAP_REFINE` (debug builds only; release always
    /// refines) so the hardware session can A/B refined vs raw landmarks;
    /// directly settable in tests.
    pub disable_heatmap_refine: bool,
}

impl Default for PoseConfig {
    fn default() -> Self {
        Self {
            detector_score_threshold: 0.5,
            presence_threshold: 0.5,
            mask_ema_alpha: DEFAULT_MASK_EMA_ALPHA,
            disable_heatmap_refine: false,
        }
    }
}

/// Live (lock-free) tunables shared between the Bevy main thread and the
/// worker: the idle-throttle flag read by the worker *loop* and the mask
/// combine-with-previous ratio read by this pipeline each frame. Same shape as
/// the hand provider's
/// `MediaPipeLiveTuning` (f32 bit patterns in `AtomicU32`, all `Relaxed` —
/// independent scalars, one-frame-stale reads are harmless).
#[derive(Debug)]
pub struct BodyLiveTuning {
    /// Worker caps at the shared idle rate + detector-only probe while set.
    idle_throttle: AtomicBool,
    /// [`PoseConfig::mask_ema_alpha`] as `f32` bits.
    mask_ema_alpha: AtomicU32,
}

impl BodyLiveTuning {
    /// Build a tuning cell. The idle flag starts cleared (full rate).
    #[must_use]
    pub fn new(mask_ema_alpha: f32) -> Self {
        Self {
            idle_throttle: AtomicBool::new(false),
            mask_ema_alpha: AtomicU32::new(mask_ema_alpha.to_bits()),
        }
    }

    /// Live-set the idle-throttle flag (cheap Relaxed store; safe every frame).
    pub fn set_idle_throttle(&self, idle: bool) {
        self.idle_throttle.store(idle, Ordering::Relaxed);
    }

    /// Whether the idle detector-only throttle is requested.
    #[must_use]
    pub fn idle_throttle(&self) -> bool {
        self.idle_throttle.load(Ordering::Relaxed)
    }

    /// Live-set the mask combine-with-previous ratio.
    pub fn set_mask_ema_alpha(&self, alpha: f32) {
        self.mask_ema_alpha
            .store(alpha.to_bits(), Ordering::Relaxed);
    }

    /// The current mask combine-with-previous ratio.
    #[must_use]
    pub fn mask_ema_alpha(&self) -> f32 {
        f32::from_bits(self.mask_ema_alpha.load(Ordering::Relaxed))
    }
}

/// Why the detector ran or was skipped for the latest processed frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DetectorRunReason {
    /// No carried track: the detector ran to (re)acquire.
    #[default]
    ColdStart,
    /// A carried track supplied the ROI; the detector was skipped.
    Tracking,
    /// Idle detector-only presence probe (landmark stage skipped).
    IdleProbe,
    /// The frame was invalid; no model stage ran.
    InvalidFrame,
}

impl DetectorRunReason {
    /// Static label for diagnostics.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ColdStart => "cold_start",
            Self::Tracking => "tracking",
            Self::IdleProbe => "idle_probe",
            Self::InvalidFrame => "invalid_frame",
        }
    }
}

/// Timing and tracking metrics for the latest processed frame.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PoseDiagnostics {
    /// Total process time for one frame.
    pub total: Duration,
    /// Square-pad / preprocessing time.
    pub preprocess: Duration,
    /// Detector-stage time (zero when skipped).
    pub detector: Duration,
    /// Landmark/mask-stage time (zero when skipped).
    pub landmark: Duration,
    /// Why the detector ran or was skipped.
    pub detector_reason: DetectorRunReason,
    /// Whether a person was tracked this frame.
    pub present: bool,
    /// The frame's confidence (detector score or landmark presence).
    pub confidence: f32,
}

/// The published outcome of one processed frame.
pub struct PoseResult {
    /// Whether a person is tracked (idle probes report detector hits here).
    pub present: bool,
    /// Track confidence.
    pub confidence: f32,
    /// Content-normalized landmarks + visibility (all defaults when absent
    /// or in the idle probe).
    pub landmarks: [BodyLandmark; BODY_LANDMARK_COUNT],
    /// Metric world landmarks (metres, hip-centred).
    pub world_landmarks: [Vec3; BODY_LANDMARK_COUNT],
}

impl PoseResult {
    /// A no-person result.
    fn absent() -> Self {
        Self {
            present: false,
            confidence: 0.0,
            landmarks: [BodyLandmark::default(); BODY_LANDMARK_COUNT],
            world_landmarks: [Vec3::ZERO; BODY_LANDMARK_COUNT],
        }
    }
}

/// The two-stage pose pipeline: model sessions, anchors, carried track, mask
/// processor, and reused scratch buffers.
pub struct PosePipeline {
    detector: Box<dyn ModelInference>,
    landmark: Box<dyn ModelInference>,
    anchors: Vec<Anchor>,
    config: PoseConfig,
    /// Landmark-derived ROI carried to the next frame (detect-then-track).
    /// While present, `process` skips the detector; dropped when presence
    /// falls below threshold, the ROI leaves the content, the frame is
    /// unusable, or an idle probe runs.
    tracked: Option<RoiRect>,
    /// Optional live tuning shared with the provider systems.
    live_tuning: Option<Arc<BodyLiveTuning>>,
    /// Heavy One-Euro filter on the aux alignment rows, applied before the
    /// next-frame tracking ROI is derived so the crop does not jitter. Reset
    /// on every fresh track (detector re-run / track drop).
    aux_filter: AuxRoiFilter,
    /// Mask warp/temporal-blend state (owns its 3×256 KB f32 buffers).
    mask: MaskProcessor,
    /// Diagnostics for the most recent processed frame.
    last_diagnostics: PoseDiagnostics,
    // --- reused scratch (see module docs; allocated once) ---
    square_buf: RgbImage,
    detector_resize_buf: RgbImage,
    warp_buf: RgbImage,
    detector_input: Tensor,
    landmark_input: Tensor,
    detector_outputs: Vec<Tensor>,
    landmark_outputs: Vec<Tensor>,
    detections: Vec<PersonDetection>,
}

impl PosePipeline {
    /// Build a pipeline from the two model stages.
    #[must_use]
    pub fn new(
        detector: Box<dyn ModelInference>,
        landmark: Box<dyn ModelInference>,
        config: PoseConfig,
    ) -> Self {
        Self {
            detector,
            landmark,
            anchors: generate_pose_anchors(),
            config,
            tracked: None,
            live_tuning: None,
            aux_filter: AuxRoiFilter::new(),
            mask: MaskProcessor::new(),
            last_diagnostics: PoseDiagnostics::default(),
            square_buf: RgbImage::default(),
            detector_resize_buf: RgbImage::new(DETECTOR_INPUT, DETECTOR_INPUT),
            warp_buf: RgbImage::new(LM_SIZE, LM_SIZE),
            detector_input: Tensor {
                data: Vec::with_capacity(idx(DETECTOR_INPUT) * idx(DETECTOR_INPUT) * 3),
                shape: vec![1, idx(DETECTOR_INPUT), idx(DETECTOR_INPUT), 3],
            },
            landmark_input: Tensor {
                data: Vec::with_capacity(idx(LM_SIZE) * idx(LM_SIZE) * 3),
                shape: vec![1, idx(LM_SIZE), idx(LM_SIZE), 3],
            },
            detector_outputs: Vec::new(),
            landmark_outputs: Vec::new(),
            detections: Vec::new(),
        }
    }

    /// Attach the shared lock-free tuning cell.
    pub fn set_live_tuning_source(&mut self, source: Arc<BodyLiveTuning>) {
        self.live_tuning = Some(source);
    }

    /// Diagnostics for the most recent processed frame.
    #[must_use]
    pub fn diagnostics(&self) -> PoseDiagnostics {
        self.last_diagnostics
    }

    /// Run one frame. `now` is the worker-relative capture time driving the
    /// aux-ROI One-Euro filter's timestep (mirrors `BodySmoother::smooth`).
    /// `detector_only` selects the idle presence probe (see module docs).
    /// `payload`, when given, receives the quantized mask and the extracted
    /// edges (full frames only; probes and absent frames decay the mask into it
    /// instead).
    ///
    /// # Errors
    /// Returns [`InferenceError`] if a model stage that was supposed to run
    /// fails. Invalid frames and empty detections are `Ok(absent)`, not
    /// errors.
    pub fn process(
        &mut self,
        frame: &Frame,
        now: Duration,
        detector_only: bool,
        mut payload: Option<&mut BodyFramePayload>,
    ) -> Result<PoseResult, InferenceError> {
        let frame_start = Instant::now();
        let mut diag = PoseDiagnostics::default();
        let blend_ratio = self
            .live_tuning
            .as_ref()
            .map_or(self.config.mask_ema_alpha, |t| t.mask_ema_alpha());

        if !frame.is_consistent() || frame.width == 0 || frame.height == 0 {
            // A bad frame breaks tracking: re-acquire next frame.
            self.tracked = None;
            diag.detector_reason = DetectorRunReason::InvalidFrame;
            self.fade_mask_into(blend_ratio, payload.as_deref_mut());
            diag.total = frame_start.elapsed();
            self.last_diagnostics = diag;
            return Ok(PoseResult::absent());
        }
        let content = ContentRect::for_frame(frame.width, frame.height);

        // Square-pad into the reused buffer (taken out so stage methods can
        // borrow it beside &mut self; restored before every return).
        let stage = Instant::now();
        let square = {
            let mut square = std::mem::take(&mut self.square_buf);
            square_pad_into(frame, &mut square);
            square
        };
        diag.preprocess = stage.elapsed();

        if detector_only {
            // Idle probe: the detector is a presence sensor; a carried crop
            // track is stale after idle, so drop it.
            self.tracked = None;
            diag.detector_reason = DetectorRunReason::IdleProbe;
            let stage = Instant::now();
            let det = self.detect(&square);
            diag.detector = stage.elapsed();
            self.square_buf = square;
            let det = det?;
            let (present, confidence) = det.as_ref().map_or((false, 0.0), |d| (true, d.score));
            self.fade_mask_into(blend_ratio, payload.as_deref_mut());
            diag.present = present;
            diag.confidence = confidence;
            diag.total = frame_start.elapsed();
            self.last_diagnostics = diag;
            return Ok(PoseResult {
                present,
                confidence,
                ..PoseResult::absent()
            });
        }

        // Detect-then-track: run the detector only without a carried track.
        // A fresh track (no carried ROI) means the aux filter must cold-start
        // so a new person does not inherit stale filter state.
        let fresh_track = self.tracked.is_none();
        let roi = if let Some(roi) = self.tracked {
            diag.detector_reason = DetectorRunReason::Tracking;
            Some(roi)
        } else {
            diag.detector_reason = DetectorRunReason::ColdStart;
            let stage = Instant::now();
            let det = self.detect(&square);
            diag.detector = stage.elapsed();
            match det {
                Ok(d) => d.map(|d| roi_from_detection(&d)),
                Err(e) => {
                    self.square_buf = square;
                    return Err(e);
                }
            }
        };
        let Some(roi) = roi else {
            // Nobody in frame: fade the mask, stay quiet.
            self.square_buf = square;
            self.fade_mask_into(blend_ratio, payload.as_deref_mut());
            diag.total = frame_start.elapsed();
            self.last_diagnostics = diag;
            return Ok(PoseResult::absent());
        };

        let stage = Instant::now();
        let outcome = self.landmark_stage(
            &square,
            roi,
            content,
            now,
            fresh_track,
            blend_ratio,
            payload.as_deref_mut(),
        );
        diag.landmark = stage.elapsed();
        self.square_buf = square;
        let outcome = outcome?;

        let result = if let Some(tracked) = outcome {
            // Carry the aux-row ROI only while it stays plausible.
            self.tracked = roi_trackable(&tracked.next_roi, content).then_some(tracked.next_roi);
            tracked.result
        } else {
            // Presence collapsed: drop the track and fade the mask.
            self.tracked = None;
            self.fade_mask_into(blend_ratio, payload);
            PoseResult::absent()
        };
        diag.present = result.present;
        diag.confidence = result.confidence;
        diag.total = frame_start.elapsed();
        self.last_diagnostics = diag;
        Ok(result)
    }

    /// Detector stage: resize → NHWC tensor → run → decode → best person.
    fn detect(&mut self, square: &RgbImage) -> Result<Option<PersonDetection>, InferenceError> {
        resize_into(
            square,
            DETECTOR_INPUT,
            DETECTOR_INPUT,
            &mut self.detector_resize_buf,
        );
        fill_nhwc_unit(&self.detector_resize_buf, &mut self.detector_input);
        self.detector
            .run(&self.detector_input, &mut self.detector_outputs)?;
        let (boxes, scores) = pick_pose_detector_outputs(&self.detector_outputs)?;
        decode_pose_detections_into(
            boxes,
            scores,
            &self.anchors,
            self.config.detector_score_threshold,
            &mut self.detections,
        );
        Ok(best_person(&self.detections, PERSON_BLEND_IOU))
    }

    /// Landmark/mask stage for one ROI. `Ok(None)` = presence below
    /// threshold (person lost). `fresh_track` cold-starts the aux-ROI filter
    /// (new track); `now` supplies its timestep.
    #[allow(
        clippy::too_many_arguments,
        reason = "worker-side stage threads frame time, track-freshness, and blend ratio alongside the ROI/content/payload; splitting into a param struct would obscure the straight-line data flow"
    )]
    fn landmark_stage(
        &mut self,
        square: &RgbImage,
        roi: RoiRect,
        content: ContentRect,
        now: Duration,
        fresh_track: bool,
        blend_ratio: f32,
        payload: Option<&mut BodyFramePayload>,
    ) -> Result<Option<TrackedBody>, InferenceError> {
        warp_roi_into(square, &roi, LM_SIZE, &mut self.warp_buf);
        fill_nhwc_unit(&self.warp_buf, &mut self.landmark_input);
        self.landmark
            .run(&self.landmark_input, &mut self.landmark_outputs)?;
        let picked = pick_pose_landmark_outputs(&self.landmark_outputs)?;
        if picked.confidence < self.config.presence_threshold {
            return Ok(None);
        }

        // Heatmap landmark refinement (upstream `RefineLandmarksFromHeatmap`),
        // in crop space before projection and the aux filter. Copy the raw
        // regression rows into a stack scratch array, refine x/y in place, then
        // project. Skipped when the A/B toggle is set or the model emitted no
        // heatmap. No allocation: `refined` is a fixed 195-float stack array.
        let mut refined = [0.0_f32; LANDMARK_ROWS * LANDMARK_VALUES];
        let copy_len = refined.len().min(picked.landmarks.len());
        refined[..copy_len].copy_from_slice(&picked.landmarks[..copy_len]);
        if !self.config.disable_heatmap_refine {
            if let Some(heatmap) = picked.heatmap {
                refine_landmarks_from_heatmap(&mut refined, heatmap);
            }
        }
        let rows = project_body_landmarks(&refined, &roi);
        // Heavily filter the aux alignment points before deriving next frame's
        // tracking ROI so the crop does not jitter (upstream aux filter). A
        // fresh track resets the filter first (no stale state from a prior
        // person); the raw points seed the object scale.
        if fresh_track {
            self.aux_filter.reset();
        }
        let (aux_center, aux_scale) = self.aux_filter.filter(
            rows[AUX_CENTER_ROW].pos.truncate(),
            rows[AUX_SCALE_ROW].pos.truncate(),
            now,
        );
        let next_roi = roi_from_alignment_points(aux_center, aux_scale);

        // Publish the first 33 rows in content-norm (mask UV space).
        let mut landmarks = [BodyLandmark::default(); BODY_LANDMARK_COUNT];
        for (dst, row) in landmarks.iter_mut().zip(rows.iter()) {
            dst.pos = content.to_content_norm(row.pos);
            dst.visibility = row.visibility;
        }
        // World landmarks are de-rotated by the ROI rotation into an
        // image-aligned frame (upstream WorldLandmarkProjectionCalculator).
        let world_landmarks = decode_world_landmarks(picked.world, &roi);

        // Mask + edges into the pooled payload (worker-side, per spec).
        if let Some(payload) = payload {
            self.mask.ingest(picked.mask, &roi, content, blend_ratio);
            self.mask.write_u8(&mut payload.mask);
            extract_edges(self.mask.smoothed(), &mut payload.edges);
        }

        Ok(Some(TrackedBody {
            result: PoseResult {
                present: true,
                confidence: picked.confidence,
                landmarks,
                world_landmarks,
            },
            next_roi,
        }))
    }

    /// Person-absent path: decay the mask accumulator toward empty and, when a
    /// payload is supplied, publish the faded mask + its (shrinking) edge list
    /// so a stale silhouette never lingers on screen. (The decay is our own
    /// graceful-fade extra, not part of the upstream blend; it keeps its
    /// original EMA-style `acc -= acc·alpha` behavior, driven by the same knob.)
    fn fade_mask_into(&mut self, alpha: f32, payload: Option<&mut BodyFramePayload>) {
        self.mask.decay(alpha);
        if let Some(payload) = payload {
            self.mask.write_u8(&mut payload.mask);
            extract_edges(self.mask.smoothed(), &mut payload.edges);
        }
    }
}

/// One tracked frame's outcome: the published result plus the ROI to track
/// from next frame. Stack-only.
struct TrackedBody {
    result: PoseResult,
    next_roi: RoiRect,
}

// --- model output selection -----------------------------------------------

/// Select the detector outputs by shape: `[1, 2254, 12]` boxes and
/// `[1, 2254, 1]` scores.
fn pick_pose_detector_outputs(out: &[Tensor]) -> Result<(&[f32], &[f32]), InferenceError> {
    let boxes = out
        .iter()
        .find(|t| t.shape == [1, POSE_ANCHOR_COUNT, POSE_REGRESSION_LEN])
        .ok_or_else(|| InferenceError::Run("pose detector: no [1,2254,12] output".into()))?;
    let scores = out
        .iter()
        .find(|t| t.shape == [1, POSE_ANCHOR_COUNT, 1])
        .ok_or_else(|| InferenceError::Run("pose detector: no [1,2254,1] output".into()))?;
    Ok((&boxes.data, &scores.data))
}

/// The landmark model's outputs the pipeline consumes.
struct PoseLandmarkOutputs<'a> {
    /// `[1, 195]`: 39 rows × (x, y, z, visibility, presence), crop pixels.
    landmarks: &'a [f32],
    /// Pose-presence probability (consumed raw — the sigmoid is baked into
    /// the graph; pinned against the vendored model in Task 14).
    confidence: f32,
    /// `[1, 256, 256, 1]` segmentation logits, crop space.
    mask: &'a [f32],
    /// `[1, 117]`: 39 × (x, y, z) metric world landmarks.
    world: &'a [f32],
    /// `[1, HEATMAP_SIZE, HEATMAP_SIZE, LANDMARK_ROWS]` refinement heatmap,
    /// when the model emitted it. `None` degrades gracefully to no refinement
    /// (a model export that stripped the heatmap head), which the vendored
    /// model contract test guards against.
    heatmap: Option<&'a [f32]>,
}

/// Select the landmark model's outputs **by shape** (order-independent). The
/// four required shapes are mutually distinct, so shape matching is
/// unambiguous; a missing required shape reports everything observed. The
/// `[1, 64, 64, 39]` refinement heatmap is optional (see
/// [`PoseLandmarkOutputs::heatmap`]).
fn pick_pose_landmark_outputs(out: &[Tensor]) -> Result<PoseLandmarkOutputs<'_>, InferenceError> {
    let find = |shape: &[usize]| out.iter().find(|t| t.shape == shape);
    let heatmap = find(&[1, HEATMAP_SIZE, HEATMAP_SIZE, LANDMARK_ROWS]).map(|t| t.data.as_slice());
    let (Some(landmarks), Some(conf), Some(mask), Some(world)) = (
        find(&[1, LANDMARK_ROWS * LANDMARK_VALUES]),
        find(&[1, 1]),
        find(&[1, MASK_SIZE, MASK_SIZE, 1]),
        find(&[1, LANDMARK_ROWS * 3]),
    ) else {
        let observed: Vec<&[usize]> = out.iter().map(|t| t.shape.as_slice()).collect();
        return Err(InferenceError::Run(format!(
            "pose landmark: unexpected output shapes {observed:?}; \
             want [1,195], [1,1], [1,{MASK_SIZE},{MASK_SIZE},1], [1,117]"
        )));
    };
    let confidence = conf
        .data
        .first()
        .copied()
        .ok_or_else(|| InferenceError::Run("pose landmark: empty confidence".into()))?;
    Ok(PoseLandmarkOutputs {
        landmarks: &landmarks.data,
        confidence,
        mask: &mask.data,
        world: &world.data,
        heatmap,
    })
}

/// Port of `MediaPipe`'s `RefineLandmarksFromHeatmapCalculator`
/// (`mediapipe/calculators/util/refine_landmarks_from_heatmap_calculator.cc`,
/// `RefineLandmarksFromHeatMap`), specialized to the pose graph's options.
///
/// For each of the [`LANDMARK_ROWS`] landmark rows it locates the landmark's
/// cell in the `[HEATMAP_SIZE, HEATMAP_SIZE, LANDMARK_ROWS]` heatmap (NHWC,
/// batch 1), scans a [`HEATMAP_KERNEL_SIZE`]² window (offset
/// `(kernel_size - 1) / 2` = 3), sigmoids each cell, and — when the window's
/// max confidence clears [`HEATMAP_MIN_CONFIDENCE`] and the weight sum is
/// positive — replaces x/y with the confidence-weighted centroid. z,
/// visibility, and presence are left untouched: the pose graph leaves
/// `refine_presence`/`refine_visibility` at their `false` proto defaults.
///
/// Runs in **crop space** on the raw landmark array (crop pixels in
/// `[0, LANDMARK_INPUT]`), BEFORE projection and the aux One-Euro filter — the
/// same graph order as upstream. Allocation-free: edits the caller's scratch
/// array in place. Landmarks whose centre cell falls outside the heatmap are
/// left unchanged (upstream's `continue`).
fn refine_landmarks_from_heatmap(landmarks: &mut [f32], heatmap: &[f32]) {
    // NHWC strides (batch 1): idx = hm_row_size·row + hm_pixel_size·col + lm.
    let hm_f = hf(HEATMAP_SIZE);
    let hm_pixel_size = LANDMARK_ROWS; // channels per pixel
    let hm_row_size = HEATMAP_SIZE * hm_pixel_size; // floats per heatmap row
    let offset = (HEATMAP_KERNEL_SIZE - 1) / 2;
    for lm in 0..LANDMARK_ROWS {
        let base = lm * LANDMARK_VALUES;
        let (Some(&lx), Some(&ly)) = (landmarks.get(base), landmarks.get(base + 1)) else {
            break;
        };
        // Raw landmarks are crop PIXELS; upstream indexes by normalized
        // [0, 1] × heatmap dimension.
        let center_col_f = lx / LANDMARK_INPUT * hm_f;
        let center_row_f = ly / LANDMARK_INPUT * hm_f;
        if !(center_col_f >= 0.0
            && center_col_f < hm_f
            && center_row_f >= 0.0
            && center_row_f < hm_f)
        {
            continue;
        }
        let center_col = idx(floor_u32(center_col_f));
        let center_row = idx(floor_u32(center_row_f));
        let begin_col = center_col.saturating_sub(offset);
        let end_col = (center_col + offset + 1).min(HEATMAP_SIZE);
        let begin_row = center_row.saturating_sub(offset);
        let end_row = (center_row + offset + 1).min(HEATMAP_SIZE);
        let mut sum = 0.0_f32;
        let mut weighted_col = 0.0_f32;
        let mut weighted_row = 0.0_f32;
        let mut max_confidence = 0.0_f32;
        for row in begin_row..end_row {
            for col in begin_col..end_col {
                let cell = heatmap.get(hm_row_size * row + hm_pixel_size * col + lm);
                let confidence = sigmoid(cell.copied().unwrap_or(0.0));
                sum += confidence;
                weighted_col += hf(col) * confidence;
                weighted_row += hf(row) * confidence;
                max_confidence = max_confidence.max(confidence);
            }
        }
        if max_confidence >= HEATMAP_MIN_CONFIDENCE && sum > 0.0 {
            // Upstream sets normalized x/y = weighted / hm / sum; convert back
            // to crop pixels (× LANDMARK_INPUT) for the rest of the pipeline.
            landmarks[base] = weighted_col / hm_f / sum * LANDMARK_INPUT;
            landmarks[base + 1] = weighted_row / hm_f / sum * LANDMARK_INPUT;
        }
    }
}

/// Lossless small-`usize` → `f32` for heatmap indices/dims (all ≤ 64).
fn hf(v: usize) -> f32 {
    u16::try_from(v).map_or(0.0, f32::from)
}

/// Decode the `[1, 117]` world tensor: 39 × (x, y, z) metric metres,
/// hip-centred; the first [`BODY_LANDMARK_COUNT`] rows are published.
///
/// The raw world coordinates come out in the **crop-aligned** frame (rotated
/// with the ROI). `MediaPipe`'s `WorldLandmarkProjectionCalculator`
/// (`world_landmark_projection_calculator.cc`) rotates x/y by the ROI rotation
/// back into an image/gravity-aligned frame (z unchanged):
///
/// ```text
/// x' = cos·x − sin·y
/// y' = sin·x + cos·y
/// ```
///
/// so a tilted subject's world landmarks are not left rotated with the crop.
fn decode_world_landmarks(raw: &[f32], roi: &RoiRect) -> [Vec3; BODY_LANDMARK_COUNT] {
    let (sin, cos) = roi.rotation.sin_cos();
    let mut out = [Vec3::ZERO; BODY_LANDMARK_COUNT];
    for (i, lm) in out.iter_mut().enumerate() {
        let base = i * 3;
        let x = raw.get(base).copied().unwrap_or(0.0);
        let y = raw.get(base + 1).copied().unwrap_or(0.0);
        let z = raw.get(base + 2).copied().unwrap_or(0.0);
        *lm = Vec3::new(cos * x - sin * y, sin * x + cos * y, z);
    }
    out
}

// --- image helpers (adapted from the validated hand pipeline) --------------

/// Square-pad a frame to its larger side (black bars), origin-centred, into a
/// reused buffer. (Re)allocates only when the side changes.
fn square_pad_into(frame: &Frame, out: &mut RgbImage) {
    let side = frame.width.max(frame.height);
    if out.width() != side || out.height() != side {
        *out = RgbImage::new(side, side);
    }
    let ox = (side - frame.width) / 2;
    let oy = (side - frame.height) / 2;
    let w = idx(frame.width);
    for y in 0..frame.height {
        let row = idx(y) * w * 3;
        for x in 0..frame.width {
            let i = row + idx(x) * 3;
            out.put_pixel(
                ox + x,
                oy + y,
                image::Rgb([frame.rgb[i], frame.rgb[i + 1], frame.rgb[i + 2]]),
            );
        }
    }
}

/// Bilinearly resize `src` into a reused `dst` (same half-pixel-centre
/// convention and downscale-aliasing tradeoff as the hand pipeline's
/// `resize_into` — `MediaPipe`'s own preprocessing point-samples identically).
fn resize_into(src: &RgbImage, w: u32, h: u32, dst: &mut RgbImage) {
    if dst.width() != w || dst.height() != h {
        *dst = RgbImage::new(w, h);
    }
    if src.width() == 0 || src.height() == 0 || w == 0 || h == 0 {
        return;
    }
    let sx = dim(src.width()) / dim(w);
    let sy = dim(src.height()) / dim(h);
    for oy in 0..h {
        let y = (dim(oy) + 0.5) * sy - 0.5;
        for ox in 0..w {
            let x = (dim(ox) + 0.5) * sx - 0.5;
            dst.put_pixel(ox, oy, sample_bilinear_rgb(src, x, y));
        }
    }
}

/// Warp the rotated normalized ROI out of `square` into a reused `out_size`²
/// crop (bilinear, inverse-mapping each output pixel — mirrors
/// `project_body_landmarks`).
fn warp_roi_into(square: &RgbImage, roi: &RoiRect, out_size: u32, dst: &mut RgbImage) {
    if dst.width() != out_size || dst.height() != out_size {
        *dst = RgbImage::new(out_size, out_size);
    }
    let side = dim(square.width());
    let (sin, cos) = roi.rotation.sin_cos();
    let outf = dim(out_size);
    for oy in 0..out_size {
        for ox in 0..out_size {
            let u = (dim(ox) / outf - 0.5) * roi.size;
            let v = (dim(oy) / outf - 0.5) * roi.size;
            let nx = roi.cx + (u * cos - v * sin);
            let ny = roi.cy + (u * sin + v * cos);
            dst.put_pixel(ox, oy, sample_bilinear_rgb(square, nx * side, ny * side));
        }
    }
}

/// Fill `out` with the NHWC `[1, h, w, 3]` `f32` tensor (RGB in `[0, 1]`),
/// reusing its buffers (`clear()` keeps capacity).
fn fill_nhwc_unit(img: &RgbImage, out: &mut Tensor) {
    out.data.clear();
    for p in img.pixels() {
        out.data.push(f32::from(p[0]) / 255.0);
        out.data.push(f32::from(p[1]) / 255.0);
        out.data.push(f32::from(p[2]) / 255.0);
    }
    out.shape.clear();
    out.shape
        .extend_from_slice(&[1, idx(img.height()), idx(img.width()), 3]);
}

/// Clamped bilinear RGB sample (index-space coordinates, edge clamp).
fn sample_bilinear_rgb(img: &RgbImage, x: f32, y: f32) -> image::Rgb<u8> {
    let w = img.width();
    let h = img.height();
    if w == 0 || h == 0 {
        return image::Rgb([0, 0, 0]);
    }
    let xc = x.clamp(0.0, dim(w - 1));
    let yc = y.clamp(0.0, dim(h - 1));
    let fx = xc - xc.floor();
    let fy = yc - yc.floor();
    let x0 = floor_u32(xc);
    let y0 = floor_u32(yc);
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let mut out = [0_u8; 3];
    for (c, slot) in out.iter_mut().enumerate() {
        let p00 = f32::from(img.get_pixel(x0, y0)[c]);
        let p10 = f32::from(img.get_pixel(x1, y0)[c]);
        let p01 = f32::from(img.get_pixel(x0, y1)[c]);
        let p11 = f32::from(img.get_pixel(x1, y1)[c]);
        let top = p00 + (p10 - p00) * fx;
        let bot = p01 + (p11 - p01) * fx;
        *slot = byte(top + (bot - top) * fy);
    }
    image::Rgb(out)
}

/// `u32` → `usize` (image index); infallible on all supported targets.
fn idx(v: u32) -> usize {
    usize::try_from(v).unwrap_or(0)
}

/// `u32` → `f32` for image dimensions (≤ 65535 for realistic frames).
fn dim(v: u32) -> f32 {
    f32::from(u16::try_from(v).unwrap_or(u16::MAX))
}

/// Floor a finite, non-negative, image-bounded float to a pixel index.
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is finite, clamped >= 0, and bounded by the image dimension; \
              float->int has no From/TryFrom"
)]
fn floor_u32(v: f32) -> u32 {
    v.max(0.0).floor() as u32
}

/// Round a `[0, 255]`-clamped float to a colour byte.
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is clamped to [0, 255]; float->int has no From/TryFrom"
)]
fn byte(v: f32) -> u8 {
    v.clamp(0.0, 255.0).round() as u8
}

/// Test fixtures shared with the worker tests (Task 11): plausible mock
/// outputs for the detector and landmark stages.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::super::roi::{LANDMARK_ROWS, LANDMARK_VALUES};
    use super::{Tensor, MASK_SIZE, POSE_ANCHOR_COUNT, POSE_REGRESSION_LEN};

    /// Anchor index of the first anchor at stride-8 grid cell (14, 14): the
    /// image-centre-ish anchor the hot fixture lights up.
    pub(crate) const HOT_ANCHOR: usize = (14 * 28 + 14) * 2;

    /// Detector outputs with ONE confident person at the central anchor:
    /// box 0.3² centred there; keypoint 0 (mid-hip) at the anchor centre,
    /// keypoint 1 (scale point) 0.15 above it → ROI size 0.375, rotation 0.
    pub(crate) fn hot_person_detector_outputs() -> Vec<Tensor> {
        let mut boxes = vec![0.0_f32; POSE_ANCHOR_COUNT * POSE_REGRESSION_LEN];
        let base = HOT_ANCHOR * POSE_REGRESSION_LEN;
        boxes[base + 2] = 224.0 * 0.3; // w
        boxes[base + 3] = 224.0 * 0.3; // h
        boxes[base + 7] = -224.0 * 0.15; // kp1 y offset: 0.15 up
        let mut scores = vec![-100.0_f32; POSE_ANCHOR_COUNT];
        scores[HOT_ANCHOR] = 100.0;
        vec![
            Tensor {
                data: boxes,
                shape: vec![1, POSE_ANCHOR_COUNT, POSE_REGRESSION_LEN],
            },
            Tensor {
                data: scores,
                shape: vec![1, POSE_ANCHOR_COUNT, 1],
            },
        ]
    }

    /// Detector outputs with every score pinned far below threshold.
    pub(crate) fn empty_detector_outputs() -> Vec<Tensor> {
        vec![
            Tensor::zeros(vec![1, POSE_ANCHOR_COUNT, POSE_REGRESSION_LEN]),
            Tensor {
                data: vec![-100.0; POSE_ANCHOR_COUNT],
                shape: vec![1, POSE_ANCHOR_COUNT, 1],
            },
        ]
    }

    /// Landmark outputs for a confident, well-spread pose: 39 rows spread
    /// down the crop (aux rows 33/34 form a valid upright tracking ROI), a
    /// centred mask blob, constant world rows, presence 0.9 — plus an
    /// all-zeros `[1, 64, 64, 39]` heatmap. Sigmoid(0) = 0.5 is a uniform
    /// field, so refinement pulls each landmark to its (centred) kernel-window
    /// centroid: a no-op for a centred landmark (aux rows 33/34 stay put, so
    /// the tracking ROI is unchanged) and a sub-pixel nudge for off-centre
    /// ones. Tests asserting exact landmark positions build their own blob
    /// heatmap; see `heatmap_refinement_*`.
    pub(crate) fn confident_landmark_outputs() -> Vec<Tensor> {
        confident_landmark_outputs_with_conf(0.9)
    }

    /// As [`confident_landmark_outputs`] but with presence 0.1 (track lost).
    pub(crate) fn low_confidence_landmark_outputs() -> Vec<Tensor> {
        confident_landmark_outputs_with_conf(0.1)
    }

    fn confident_landmark_outputs_with_conf(conf: f32) -> Vec<Tensor> {
        let mut rows = vec![0.0_f32; LANDMARK_ROWS * LANDMARK_VALUES];
        for i in 0..LANDMARK_ROWS {
            let base = i * LANDMARK_VALUES;
            // x sweeps a little around centre; y walks down the crop.
            rows[base] = 118.0 + f32_from_usize(i % 5) * 5.0;
            rows[base + 1] = 50.0 + f32_from_usize(i) * 4.0;
            rows[base + 2] = 0.0;
            rows[base + 3] = 2.0; // visibility logit → ≈ 0.88
            rows[base + 4] = 2.0; // presence logit
        }
        // Aux tracking rows: centre (128, 128), scale point straight above at
        // (128, 96) → upright track ROI with size 2·(32/256)·roi_size·1.25.
        rows[33 * LANDMARK_VALUES] = 128.0;
        rows[33 * LANDMARK_VALUES + 1] = 128.0;
        rows[34 * LANDMARK_VALUES] = 128.0;
        rows[34 * LANDMARK_VALUES + 1] = 96.0;

        // Central mask blob: +8 logits in the middle quarter, −8 elsewhere.
        let mut mask = vec![-8.0_f32; MASK_SIZE * MASK_SIZE];
        for y in 96..160 {
            for x in 96..160 {
                mask[y * MASK_SIZE + x] = 8.0;
            }
        }

        // Constant world rows (metric): x 0.1, y −0.2, z 0.05.
        let mut world = vec![0.0_f32; LANDMARK_ROWS * 3];
        for i in 0..LANDMARK_ROWS {
            world[i * 3] = 0.1;
            world[i * 3 + 1] = -0.2;
            world[i * 3 + 2] = 0.05;
        }

        vec![
            // Deliberately shuffled order + an extra heatmap tensor: the
            // pipeline must pick by shape, not position.
            Tensor::zeros(vec![1, 64, 64, LANDMARK_ROWS]),
            Tensor {
                data: world,
                shape: vec![1, LANDMARK_ROWS * 3],
            },
            Tensor {
                data: vec![conf],
                shape: vec![1, 1],
            },
            Tensor {
                data: mask,
                shape: vec![1, MASK_SIZE, MASK_SIZE, 1],
            },
            Tensor {
                data: rows,
                shape: vec![1, LANDMARK_ROWS * LANDMARK_VALUES],
            },
        ]
    }

    /// Lossless small-usize → f32 for fixture math.
    fn f32_from_usize(v: usize) -> f32 {
        u16::try_from(v).map_or(0.0, f32::from)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::fixtures::*;
    use super::*;
    use crate::input::capture::Frame;
    use crate::input::onnx::Tensor;

    /// Inference stub replaying fixed outputs.
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

    /// Inference stub that always fails — proves a stage was NOT invoked when
    /// a call would error the pipeline.
    struct FailingInference;

    impl ModelInference for FailingInference {
        fn run(&mut self, _input: &Tensor, _out: &mut Vec<Tensor>) -> Result<(), InferenceError> {
            Err(InferenceError::Run("must not run".into()))
        }
    }

    fn solid_frame() -> Frame {
        let mut f = Frame::default();
        f.fit_to(64, 48);
        f
    }

    fn person_pipeline() -> PosePipeline {
        PosePipeline::new(
            Box::new(StaticInference {
                outputs: hot_person_detector_outputs(),
            }),
            Box::new(StaticInference {
                outputs: confident_landmark_outputs(),
            }),
            PoseConfig::default(),
        )
    }

    #[test]
    fn cold_start_detects_then_tracks() {
        let mut p = person_pipeline();
        let mut payload = crate::input::body::transport::BodyFramePayload::new();
        let frame = solid_frame();

        let r1 = p
            .process(&frame, Duration::from_millis(0), false, Some(&mut payload))
            .expect("frame 1");
        assert!(r1.present);
        assert!(r1.confidence > 0.8);
        assert_eq!(
            p.diagnostics().detector_reason,
            DetectorRunReason::ColdStart
        );
        // Landmarks land in content-norm [0, 1] with high visibility.
        for lm in &r1.landmarks {
            assert!(lm.pos.x.is_finite() && lm.pos.y.is_finite());
            assert!(lm.visibility > 0.7, "vis={}", lm.visibility);
        }
        // World landmarks decode from the [1, 117] tensor.
        assert!((r1.world_landmarks[0].x - 0.1).abs() < 1e-5);
        assert!((r1.world_landmarks[0].y - (-0.2)).abs() < 1e-5);

        // Frame 2: the carried aux-row track skips the detector entirely.
        let r2 = p
            .process(&frame, Duration::from_millis(16), false, Some(&mut payload))
            .expect("frame 2");
        assert!(r2.present);
        assert_eq!(p.diagnostics().detector_reason, DetectorRunReason::Tracking);
    }

    #[test]
    fn mask_and_edges_land_in_the_payload() {
        let mut p = person_pipeline();
        let mut payload = crate::input::body::transport::BodyFramePayload::new();
        p.process(
            &solid_frame(),
            Duration::from_millis(0),
            false,
            Some(&mut payload),
        )
        .expect("process");
        // The fixture's mask blob covers the crop centre; after warping, the
        // frame-space mask must be lit near the ROI centre and dark far away.
        let max = payload.mask.iter().copied().max().unwrap_or(0);
        assert!(max > 200, "mask never lit: max={max}");
        assert!(!payload.edges.is_empty(), "edges must be extracted");
        assert!(payload.edges.len() <= crate::input::body::MAX_EDGE_POINTS);
    }

    #[test]
    fn low_landmark_confidence_drops_the_track_and_fades_the_mask() {
        let mut p = PosePipeline::new(
            Box::new(StaticInference {
                outputs: hot_person_detector_outputs(),
            }),
            Box::new(StaticInference {
                outputs: low_confidence_landmark_outputs(),
            }),
            PoseConfig::default(),
        );
        let mut payload = crate::input::body::transport::BodyFramePayload::new();
        let r = p
            .process(
                &solid_frame(),
                Duration::from_millis(0),
                false,
                Some(&mut payload),
            )
            .expect("process");
        assert!(!r.present, "conf below threshold must read absent");
        // Next frame must re-detect (track not carried).
        p.process(
            &solid_frame(),
            Duration::from_millis(16),
            false,
            Some(&mut payload),
        )
        .expect("frame 2");
        assert_eq!(
            p.diagnostics().detector_reason,
            DetectorRunReason::ColdStart
        );
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "exact equality against PoseResult::absent()'s zero literal, not a computed value"
    )]
    fn empty_detector_output_reads_absent() {
        let mut p = PosePipeline::new(
            Box::new(StaticInference {
                outputs: empty_detector_outputs(),
            }),
            Box::new(FailingInference), // landmark stage must not run
            PoseConfig::default(),
        );
        let r = p
            .process(&solid_frame(), Duration::from_millis(0), false, None)
            .expect("process");
        assert!(!r.present);
        assert_eq!(r.confidence, 0.0);
    }

    #[test]
    fn detector_only_probe_skips_the_landmark_stage() {
        // Idle probe: hot detector + a landmark stage that would ERROR if
        // invoked. Present must still be reported (the wake path).
        let mut p = PosePipeline::new(
            Box::new(StaticInference {
                outputs: hot_person_detector_outputs(),
            }),
            Box::new(FailingInference),
            PoseConfig::default(),
        );
        let r = p
            .process(&solid_frame(), Duration::from_millis(0), true, None)
            .expect("probe");
        assert!(r.present, "idle probe must still report presence");
        assert!(r.confidence > 0.8);
        assert_eq!(
            p.diagnostics().detector_reason,
            DetectorRunReason::IdleProbe
        );
    }

    #[test]
    fn invalid_frame_clears_the_track() {
        let mut p = person_pipeline();
        let good = solid_frame();
        p.process(&good, Duration::from_millis(0), false, None)
            .expect("acquire");
        let bad = Frame {
            width: 10, // inconsistent: no bytes
            ..Frame::default()
        };
        let r = p
            .process(&bad, Duration::from_millis(16), false, None)
            .expect("invalid frame is not an error");
        assert!(!r.present);
        assert_eq!(
            p.diagnostics().detector_reason,
            DetectorRunReason::InvalidFrame
        );
        p.process(&good, Duration::from_millis(32), false, None)
            .expect("reacquire");
        assert_eq!(
            p.diagnostics().detector_reason,
            DetectorRunReason::ColdStart
        );
    }

    #[test]
    fn live_tuning_updates_the_mask_alpha() {
        let tuning = std::sync::Arc::new(BodyLiveTuning::new(0.35));
        let mut p = person_pipeline();
        p.set_live_tuning_source(std::sync::Arc::clone(&tuning));
        tuning.set_mask_ema_alpha(0.9);
        assert!((tuning.mask_ema_alpha() - 0.9).abs() < 1e-6);
        // Round-trips through the atomic; the pipeline reads it per frame.
        let mut payload = crate::input::body::transport::BodyFramePayload::new();
        p.process(
            &solid_frame(),
            Duration::from_millis(0),
            false,
            Some(&mut payload),
        )
        .expect("process");
    }

    #[test]
    fn aux_filter_reduces_roi_centre_jitter() {
        // A still subject whose aux CENTRE jitters ±~0.006 around (0.5, 0.5),
        // with a steady scale point above it. The heavy aux One-Euro filter
        // must shrink the centre's frame-to-frame variance well below the raw
        // input's, so the derived tracking ROI stops jittering.
        let mut f = AuxRoiFilter::new();
        let scale_point = Vec2::new(0.5, 0.3);
        let jitter = [
            0.006_f32, -0.006, 0.005, -0.005, 0.006, -0.004, 0.005, -0.006,
        ];
        let mut raw_xs = Vec::new();
        let mut filt_xs = Vec::new();
        for i in 0..120_u64 {
            let idx = usize::try_from(i % 8).unwrap_or(0);
            let center = Vec2::new(0.5 + jitter[idx], 0.5);
            let ms = i.saturating_mul(16);
            let (c, _s) = f.filter(center, scale_point, Duration::from_millis(ms));
            if i >= 40 {
                // Skip the filter warm-up.
                raw_xs.push(center.x);
                filt_xs.push(c.x);
            }
        }
        let variance = |xs: &[f32]| {
            let n = f32::from(u16::try_from(xs.len()).unwrap_or(1)).max(1.0);
            let mean = xs.iter().sum::<f32>() / n;
            xs.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n
        };
        let raw_var = variance(&raw_xs);
        let filt_var = variance(&filt_xs);
        assert!(raw_var > 1e-6, "raw input must actually jitter: {raw_var}");
        assert!(
            filt_var < raw_var * 0.25,
            "filtered var {filt_var} not << raw var {raw_var}"
        );
    }

    #[test]
    fn aux_filter_reset_cold_starts() {
        // After building history, reset() must make the next sample pass
        // through (a returning person inherits no stale filter state).
        let mut f = AuxRoiFilter::new();
        let scale_point = Vec2::new(0.5, 0.3);
        for i in 0..10_u64 {
            f.filter(
                Vec2::new(0.5, 0.5),
                scale_point,
                Duration::from_millis(i.saturating_mul(16)),
            );
        }
        f.reset();
        // A far-away first sample after reset is returned verbatim, not eased
        // from the pre-reset (0.5, 0.5) history.
        let (c, s) = f.filter(
            Vec2::new(0.8, 0.2),
            Vec2::new(0.8, 0.05),
            Duration::from_secs(1),
        );
        assert!(
            (c.x - 0.8).abs() < 1e-6 && (c.y - 0.2).abs() < 1e-6,
            "c={c:?}"
        );
        assert!(
            (s.x - 0.8).abs() < 1e-6 && (s.y - 0.05).abs() < 1e-6,
            "s={s:?}"
        );
    }

    #[test]
    fn decode_world_landmarks_derotates_by_roi_rotation() {
        use std::f32::consts::FRAC_PI_2;
        let mut world = vec![0.0_f32; LANDMARK_ROWS * 3];
        world[0] = 0.1;
        world[1] = -0.2;
        world[2] = 0.05;
        // ROI rotated 90° → (sin, cos) = (1, 0): x' = −y, y' = x, z unchanged.
        let roi = RoiRect {
            cx: 0.5,
            cy: 0.5,
            size: 0.4,
            rotation: FRAC_PI_2,
        };
        let out = decode_world_landmarks(&world, &roi);
        assert!((out[0].x - 0.2).abs() < 1e-5, "x={}", out[0].x); // −(−0.2)
        assert!((out[0].y - 0.1).abs() < 1e-5, "y={}", out[0].y);
        assert!((out[0].z - 0.05).abs() < 1e-6, "z={}", out[0].z);
        // Zero rotation is the identity copy.
        let roi0 = RoiRect {
            rotation: 0.0,
            ..roi
        };
        let out0 = decode_world_landmarks(&world, &roi0);
        assert!((out0[0].x - 0.1).abs() < 1e-6, "x0={}", out0[0].x);
        assert!((out0[0].y + 0.2).abs() < 1e-6, "y0={}", out0[0].y);
    }

    /// Write a single heatmap cell `(row, col)` for landmark channel `lm`.
    fn set_heatmap_cell(hm: &mut [f32], row: usize, col: usize, lm: usize, v: f32) {
        hm[(HEATMAP_SIZE * LANDMARK_ROWS) * row + LANDMARK_ROWS * col + lm] = v;
    }

    #[test]
    fn heatmap_refinement_moves_xy_to_the_weighted_centroid() {
        // Landmark 0 at crop centre (128, 128) → heatmap centre cell (32, 32);
        // with kernel 7 the window is rows/cols 29..=35. Two equal-confidence
        // blob cells at (row 30, col 34) and (row 34, col 34), everything else
        // ~0 confidence. Hand computation (blob conf ≈ 1, background ≈ 0):
        //   sum          = 2·c
        //   weighted_col  = (34 + 34)·c = 68·c
        //   weighted_row  = (30 + 34)·c = 64·c
        //   refined x_norm = 68·c / 64 / (2·c) = 0.53125 → 0.53125·256 = 136.0
        //   refined y_norm = 64·c / 64 / (2·c) = 0.5     → 0.5·256     = 128.0
        let mut landmarks = [0.0_f32; LANDMARK_ROWS * LANDMARK_VALUES];
        landmarks[0] = 128.0;
        landmarks[1] = 128.0;
        let mut heatmap = vec![-30.0_f32; HEATMAP_SIZE * HEATMAP_SIZE * LANDMARK_ROWS];
        set_heatmap_cell(&mut heatmap, 30, 34, 0, 20.0);
        set_heatmap_cell(&mut heatmap, 34, 34, 0, 20.0);

        refine_landmarks_from_heatmap(&mut landmarks, &heatmap);
        assert!((landmarks[0] - 136.0).abs() < 1e-2, "x={}", landmarks[0]);
        assert!((landmarks[1] - 128.0).abs() < 1e-2, "y={}", landmarks[1]);
    }

    #[test]
    fn heatmap_refinement_below_threshold_leaves_the_landmark_unchanged() {
        // Same geometry, but the blob's max confidence is sigmoid(-1) ≈ 0.269,
        // under the 0.5 min_confidence_to_refine → x/y are left as-is.
        let mut landmarks = [0.0_f32; LANDMARK_ROWS * LANDMARK_VALUES];
        landmarks[0] = 128.0;
        landmarks[1] = 128.0;
        let mut heatmap = vec![-30.0_f32; HEATMAP_SIZE * HEATMAP_SIZE * LANDMARK_ROWS];
        set_heatmap_cell(&mut heatmap, 30, 34, 0, -1.0);
        set_heatmap_cell(&mut heatmap, 34, 34, 0, -1.0);

        refine_landmarks_from_heatmap(&mut landmarks, &heatmap);
        assert!((landmarks[0] - 128.0).abs() < 1e-6, "x={}", landmarks[0]);
        assert!((landmarks[1] - 128.0).abs() < 1e-6, "y={}", landmarks[1]);
    }

    /// Confident landmark fixture whose heatmap carries a single strong blob
    /// pulling ONLY landmark 0 to a higher crop-x cell; every other channel is
    /// far below threshold, so only landmark 0's published position moves.
    fn confident_outputs_with_lm0_blob() -> Vec<Tensor> {
        let mut outs = confident_landmark_outputs();
        for t in &mut outs {
            if t.shape == vec![1, HEATMAP_SIZE, HEATMAP_SIZE, LANDMARK_ROWS] {
                let mut h = vec![-30.0_f32; HEATMAP_SIZE * HEATMAP_SIZE * LANDMARK_ROWS];
                // Landmark 0 raw crop is (118, 50) → cell (col 29, row 12);
                // pull it to (col 32, row 9), inside the 7×7 window.
                set_heatmap_cell(&mut h, 9, 32, 0, 20.0);
                t.data = h;
            }
        }
        outs
    }

    #[test]
    fn disable_heatmap_refine_toggle_skips_the_pass() {
        // Same frame, same detector, same landmark fixture with an off-centre
        // blob for landmark 0: with refinement ON the published landmark 0 is
        // pulled toward higher x; with the toggle set it stays at the raw
        // regression position.
        let make = |disable: bool| {
            let cfg = PoseConfig {
                disable_heatmap_refine: disable,
                ..PoseConfig::default()
            };
            PosePipeline::new(
                Box::new(StaticInference {
                    outputs: hot_person_detector_outputs(),
                }),
                Box::new(StaticInference {
                    outputs: confident_outputs_with_lm0_blob(),
                }),
                cfg,
            )
        };
        let mut on = make(false);
        let mut off = make(true);
        let r_on = on
            .process(&solid_frame(), Duration::from_millis(0), false, None)
            .expect("refine on");
        let r_off = off
            .process(&solid_frame(), Duration::from_millis(0), false, None)
            .expect("refine off");
        assert!(r_on.present && r_off.present);
        assert!(
            r_on.landmarks[0].pos.x > r_off.landmarks[0].pos.x + 1e-3,
            "refinement must move landmark 0 (on={}, off={})",
            r_on.landmarks[0].pos.x,
            r_off.landmarks[0].pos.x
        );
    }
}
