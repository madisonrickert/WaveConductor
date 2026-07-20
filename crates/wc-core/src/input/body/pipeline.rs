//! Two-stage `BlazePose` pipeline, multi-person: a camera `Frame` in; per-slot
//! landmarks + world landmarks + a per-slot channel of the warped/temporally-
//! blended RGBA mask + slot-partitioned silhouette edges out.
//!
//! Flow per frame: square-pad the frame; run the person detector when no
//! track is carried (cold start) or on the discovery-scan cadence
//! (`DETECT_SCAN_INTERVAL`) while capacity remains — a healthy full house
//! never pays the detector; associate detections to **stable slots**
//! ([`super::selection::assign_slots`]): active/reserved slots re-bind by
//! centre distance (a returning person keeps their slot), new people claim
//! free slots. Per active slot: warp the rotated ROI into a 256² crop; run
//! the landmark model; gate on its pose-presence scalar; project the 39 rows
//! back to square-norm; heavily One-Euro filter the aux alignment rows before
//! deriving next frame's tracking ROI so the crop does not jitter; publish
//! the first 33 in content-norm (mask UV space); de-rotate the metric world
//! landmarks by the ROI rotation; warp + uncertainty-blend the segmentation
//! mask into the slot's channel; extract silhouette edges into the pooled
//! payload (slot-ordered).
//!
//! **Inference budget:** with ≤ `MAX_FULL_INFERENCE_SLOTS` active tracks
//! every track runs landmark/mask inference every frame; with more, the
//! pipeline interleaves round-robin (`MAX_FULL_INFERENCE_SLOTS` tracks per
//! frame), holding the last landmarks/mask for skipped tracks (freshly
//! activated tracks jump the queue so a new person appears immediately).
//!
//! A **lost** track's slot is *reserved* for `SLOT_RESERVE` before a new
//! person may claim it: long enough for the main-side presence-hold + fade
//! release to finish, so a mask channel is never recycled while its previous
//! occupant is still fading out on screen (see `super::envelope`), and long
//! enough that a brief occlusion re-acquires the same slot.
//!
//! The **idle detector-only probe** (`detector_only = true`) runs just the
//! detector as a presence sensor at the idle rate: landmark/mask stages are
//! skipped, all carried tracks are dropped (stale after idle), and the mask
//! channels decay so no stale silhouette lingers.
//!
//! All scratch (pad/resize/warp images, input/output tensors, per-slot mask
//! processors) is owned by the pipeline and refilled in place — the
//! steady-state frame path allocates nothing. Image helpers are adapted from
//! the validated hand pipeline (same conventions: `/255` RGB NHWC, square-pad
//! to the larger side, bilinear warp/resize with clamp-to-edge).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bevy::math::{Vec2, Vec3};
use image::RgbImage;

use super::detector::{
    decode_pose_detections_into, generate_pose_anchors, sigmoid, weighted_nms_into, Anchor,
    PersonDetection, Rect, DETECTOR_INPUT, MAX_PERSON_CANDIDATES, POSE_ANCHOR_COUNT,
    POSE_REGRESSION_LEN,
};
use super::edges::extract_edges_append;
use super::mask::{MaskProcessor, DEFAULT_MASK_EMA_ALPHA};
use super::roi::{
    project_body_landmarks, roi_from_alignment_points, roi_from_detection, roi_trackable,
    ContentRect, RoiRect, AUX_CENTER_ROW, AUX_SCALE_ROW, LANDMARK_INPUT, LANDMARK_ROWS,
    LANDMARK_VALUES,
};
use super::selection::{assign_slots, visible_fraction};
use super::smoothing::OneEuroFilter;
use super::transport::{BodyFramePayload, SlotFrame};
use super::{BodyLandmark, BODY_LANDMARK_COUNT, MASK_CHANNELS, MASK_SIZE, MAX_TRACKED_BODIES};
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

/// Discovery-scan cadence: while at least one track is active and capacity
/// remains (a free or reserved slot exists), the detector re-runs at this
/// interval to spot new people walking in and to re-acquire reserved slots.
/// ~3 Hz keeps the extra detector cost negligible against the per-slot
/// landmark inference while a newcomer still appears within a beat.
const DETECT_SCAN_INTERVAL: Duration = Duration::from_millis(300);

/// How long a lost track's slot stays *reserved* (matchable by a returning
/// person, unclaimable by a new one) before freeing. Must cover the main-side
/// `envelope::PRESENCE_HOLD` (0.3 s) plus the fade release to zero
/// (`FADE_RELEASE_TAU · ln(1/FADE_DONE_EPSILON)` ≈ 3.6 s) so a mask channel
/// is never handed to a newcomer mid-fade; it also subsumes the old
/// single-track 2 s occlusion stickiness. Tracks lost before
/// [`RESERVE_MIN_ACTIVE`] skip the reservation entirely (they can never have
/// ignited a fade to protect).
const SLOT_RESERVE: Duration = Duration::from_secs(4);

/// Minimum time a track must have been active for its loss to earn the
/// [`SLOT_RESERVE`] reservation; a younger track frees its slot immediately.
///
/// Busy-road defence (see the `envelope` module doc): the main thread only
/// begins a slot's fade-in after `envelope::ADMIT_DWELL` (0.7 s) of
/// sustained presence, so a track that lived less than that has **nothing on
/// screen to protect** — reserving it would only convert drive-past traffic
/// into 4 s slot zombies that starve a genuine newcomer of capacity (four
/// walkers in quick succession would otherwise pin all four slots Reserved).
///
/// Invariant: must stay **strictly below** `envelope::ADMIT_DWELL` with
/// margin for the worker→main transport/poll jitter (a frame or three at
/// 30 Hz), so any track whose main-side dwell *could* have completed — and
/// whose fade could therefore be mid-flight — always takes the reserved
/// path. 0.6 s leaves 100 ms of margin; a defaults-agreement test pins the
/// ordering.
const RESERVE_MIN_ACTIVE: Duration = Duration::from_millis(600);

/// Reservation window for a *young* track whose carried ROI collapsed at the
/// frame edge (the untrackable-`next_roi` branch of `run_slot_inference`).
///
/// That branch is not a confirmed loss: the aux-filtered crop can transiently
/// collapse while the person is still there (fresh filter state, frame-edge
/// standing spot), and its contract is "drop the carried track, keep this
/// frame's landmarks, and let the immediate re-scan re-acquire into the same
/// slot". A young track therefore cannot take the busy-road hard-free (that
/// would kill a person who merely wobbled the crop) — but it also must not
/// pin the slot for the full [`SLOT_RESERVE`] 4 s. This short window covers
/// one detector re-scan cycle ([`DETECT_SCAN_INTERVAL`] = 300 ms) with
/// margin: the returning person re-binds within it, a genuine walker frees
/// the slot in half a second instead of four.
const YOUNG_EDGE_RESERVE: Duration = Duration::from_millis(500);

/// Active-track count at or below which every track runs landmark/mask
/// inference every frame; above it the pipeline interleaves round-robin,
/// running this many tracks per frame (see the module doc's inference
/// budget).
const MAX_FULL_INFERENCE_SLOTS: usize = 2;

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
/// (landmarks + mask + edges) inherits the jitter. One instance per slot.
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
    /// Minimum pose-presence probability from the landmark model to keep a
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
    /// Maximum concurrently tracked people, clamped to
    /// `1..=`[`MAX_TRACKED_BODIES`] at construction. Slots at or above the
    /// cap are never claimable (see `BodyTrackingConfig::max_tracked_bodies`
    /// for the operator knob this mirrors).
    pub max_tracked_bodies: usize,
}

impl Default for PoseConfig {
    fn default() -> Self {
        Self {
            detector_score_threshold: 0.5,
            presence_threshold: 0.5,
            mask_ema_alpha: DEFAULT_MASK_EMA_ALPHA,
            disable_heatmap_refine: false,
            max_tracked_bodies: MAX_TRACKED_BODIES,
        }
    }
}

/// Live (lock-free) tunables shared between the Bevy main thread and the
/// worker: the idle-throttle flag read by the worker *loop* and the mask
/// combine-with-previous ratio read by this pipeline each frame. Same shape as
/// the hand provider's
/// `MediaPipeLiveTuning` (f32 bit patterns in `AtomicU32`, all `Relaxed` —
/// independent scalars, one-frame-stale reads are harmless).
///
/// (The old person-cycle counter moved main-side: with stable slots the `KeyN`
/// hotkey cycles which slot is *primary* in the publisher and never needs to
/// reach the worker — see `selection::PrimarySelect::cycle`.)
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
    /// No active track: the detector ran to (re)acquire.
    #[default]
    ColdStart,
    /// Carried tracks supplied every ROI; the detector was skipped.
    Tracking,
    /// Idle detector-only presence probe (landmark stage skipped).
    IdleProbe,
    /// The frame was invalid; no model stage ran.
    InvalidFrame,
    /// Tracks were active but capacity remained: the periodic discovery scan
    /// ran the detector to spot new people / re-acquire reserved slots.
    Scan,
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
            Self::Scan => "scan",
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
    /// Landmark/mask-stage time (all slots run this frame; zero when skipped).
    pub landmark: Duration,
    /// Why the detector ran or was skipped.
    pub detector_reason: DetectorRunReason,
    /// Whether any person was tracked this frame.
    pub present: bool,
    /// The frame's best confidence over slots (detector score or landmark
    /// presence).
    pub confidence: f32,
    /// Number of weighted-NMS person candidates from the MOST RECENT detector
    /// pass. Only refreshes when the detector actually runs (cold start,
    /// discovery scan, or idle probe) — it is stale (carried) on the
    /// detector-skipping tracking frames in between.
    pub people_detected: u8,
    /// Number of active tracked slots after this frame.
    pub active_tracks: u8,
}

/// Which lifecycle phase a slot is in (worker-side).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SlotPhase {
    /// Unoccupied; claimable by a new person (below the configured cap).
    #[default]
    Free,
    /// Occupied by a live track.
    Active,
    /// Recently lost: matchable by a returning person, unclaimable by a new
    /// one, until `reserved_until` (see [`SLOT_RESERVE`]).
    Reserved,
}

/// Worker-side per-slot tracking state: the carried ROI, association anchor,
/// aux-ROI filter, mask accumulator, and the last published [`SlotFrame`].
struct SlotTrack {
    /// Lifecycle phase.
    phase: SlotPhase,
    /// The ROI to crop this frame (carried aux track or a fresh detection);
    /// `Some` only while [`SlotPhase::Active`].
    roi: Option<RoiRect>,
    /// Last known person centre (square-norm) — the association anchor while
    /// Active or Reserved.
    anchor: Vec2,
    /// When this occupancy first became [`SlotPhase::Active`] (worker clock).
    /// NOT reset on a Reserved→Active re-acquisition — the occupancy
    /// continues — only on a fresh Free→Active claim. Drives the
    /// [`RESERVE_MIN_ACTIVE`] fast-free decision in [`Self::lose`].
    active_since: Duration,
    /// [`SlotPhase::Reserved`] expiry.
    reserved_until: Duration,
    /// This slot's aux alignment-row filter (reset on every fresh track).
    aux_filter: AuxRoiFilter,
    /// This slot's mask warp/temporal-blend state (owns 3×256 KB f32).
    mask: MaskProcessor,
    /// Latest result, held for round-robin-skipped frames.
    frame: SlotFrame,
}

impl SlotTrack {
    fn new() -> Self {
        Self {
            phase: SlotPhase::Free,
            roi: None,
            anchor: Vec2::ZERO,
            active_since: Duration::ZERO,
            reserved_until: Duration::ZERO,
            aux_filter: AuxRoiFilter::new(),
            mask: MaskProcessor::new(),
            frame: SlotFrame::default(),
        }
    }

    /// Whether a loss at `now` earns the [`SLOT_RESERVE`] reservation: only
    /// tracks old enough ([`RESERVE_MIN_ACTIVE`]) to possibly have ignited a
    /// main-side fade. Younger tracks (drive-past traffic) free immediately.
    fn earns_reserve(&self, now: Duration) -> bool {
        now.saturating_sub(self.active_since) >= RESERVE_MIN_ACTIVE
    }

    /// Track lost: reserve the slot (see [`SLOT_RESERVE`]) so the person can
    /// return to it and the main-side fade can finish before reuse — unless
    /// the track was too young to have ignited ([`Self::earns_reserve`]), in
    /// which case the slot frees on the spot (no zombie reservation for
    /// walk-through traffic).
    fn lose(&mut self, now: Duration) {
        if !self.earns_reserve(now) {
            self.release();
            return;
        }
        self.phase = SlotPhase::Reserved;
        self.reserved_until = now + SLOT_RESERVE;
        self.roi = None;
        self.frame.present = false;
    }

    /// Reservation expired (or idle probe): fully release the slot. The mask
    /// accumulator is reset so the channel is clean for the next claimant.
    fn release(&mut self) {
        self.phase = SlotPhase::Free;
        self.roi = None;
        self.frame = SlotFrame::default();
        self.mask.reset();
    }
}

/// The two-stage multi-person pose pipeline: model sessions, anchors, slot
/// tracks, and reused scratch buffers.
pub struct PosePipeline {
    detector: Box<dyn ModelInference>,
    landmark: Box<dyn ModelInference>,
    anchors: Vec<Anchor>,
    config: PoseConfig,
    /// Effective tracked-body cap (`config.max_tracked_bodies` clamped to
    /// `1..=MAX_TRACKED_BODIES`); slots at or above it are never claimed.
    max_tracked: usize,
    /// Per-slot tracking state, indexed by stable slot.
    slots: [SlotTrack; MAX_TRACKED_BODIES],
    /// Per-slot results of the latest processed frame (what the worker
    /// publishes).
    slot_frames: [SlotFrame; MAX_TRACKED_BODIES],
    /// Next discovery scan is due at this time (worker-relative clock).
    next_scan: Duration,
    /// Round-robin cursor: the slot index the next interleaved inference pass
    /// starts scanning from.
    rr_next: usize,
    /// Optional live tuning shared with the provider systems.
    live_tuning: Option<Arc<BodyLiveTuning>>,
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
    /// Reused weighted-NMS person-cluster candidates (≤
    /// [`MAX_PERSON_CANDIDATES`]), refilled each detector pass; never allocates
    /// after construction.
    person_clusters: Vec<PersonDetection>,
    /// Candidate count from the most recent detector pass, surfaced through
    /// [`PoseDiagnostics::people_detected`]. Stale on tracking frames.
    people_detected: u8,
}

impl PosePipeline {
    /// Build a pipeline from the two model stages.
    #[must_use]
    pub fn new(
        detector: Box<dyn ModelInference>,
        landmark: Box<dyn ModelInference>,
        config: PoseConfig,
    ) -> Self {
        let max_tracked = config.max_tracked_bodies.clamp(1, MAX_TRACKED_BODIES);
        Self {
            detector,
            landmark,
            anchors: generate_pose_anchors(),
            config,
            max_tracked,
            slots: std::array::from_fn(|_| SlotTrack::new()),
            slot_frames: [SlotFrame::default(); MAX_TRACKED_BODIES],
            next_scan: Duration::ZERO,
            rr_next: 0,
            live_tuning: None,
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
            person_clusters: Vec::with_capacity(MAX_PERSON_CANDIDATES),
            people_detected: 0,
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

    /// Per-slot results of the most recent processed frame. The worker copies
    /// this array into the published `BodyFrame`.
    #[must_use]
    pub fn slot_frames(&self) -> &[SlotFrame; MAX_TRACKED_BODIES] {
        &self.slot_frames
    }

    /// Run one frame. `now` is the worker-relative capture time driving the
    /// aux-ROI One-Euro filters, the slot reservations, and the scan cadence.
    /// `detector_only` selects the idle presence probe (see module docs).
    /// `payload`, when given, receives the quantized RGBA mask and the
    /// slot-partitioned edges (full frames only; probes and absent frames
    /// decay the mask into it instead). Results land in
    /// [`Self::slot_frames`].
    ///
    /// # Errors
    /// Returns [`InferenceError`] if a model stage that was supposed to run
    /// fails. Invalid frames and empty detections are `Ok` (absent slots),
    /// not errors.
    pub fn process(
        &mut self,
        frame: &Frame,
        now: Duration,
        detector_only: bool,
        payload: Option<&mut BodyFramePayload>,
    ) -> Result<(), InferenceError> {
        let frame_start = Instant::now();
        let mut diag = PoseDiagnostics::default();
        let blend_ratio = self
            .live_tuning
            .as_ref()
            .map_or(self.config.mask_ema_alpha, |t| t.mask_ema_alpha());

        if !frame.is_consistent() || frame.width == 0 || frame.height == 0 {
            self.process_invalid_frame(now, blend_ratio, payload, diag, frame_start);
            return Ok(());
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
            return self.process_idle_probe(square, blend_ratio, payload, diag, frame_start);
        }

        // Expire reservations whose window has passed, and clear presence on
        // the still-reserved ones: a slot that lost its track keeps its final
        // valid frame `present` (see `run_slot_inference`'s untrackable
        // branch) but must read absent from the NEXT frame unless the
        // detector re-acquires it below.
        for slot in &mut self.slots {
            match slot.phase {
                SlotPhase::Reserved if now >= slot.reserved_until => slot.release(),
                SlotPhase::Reserved => slot.frame.present = false,
                SlotPhase::Free | SlotPhase::Active => {}
            }
        }

        // Detect-or-track: run the detector on cold start (no active track,
        // every frame) or on the discovery-scan cadence while capacity
        // remains; otherwise every ROI comes from a carried track.
        let mut fresh = [false; MAX_TRACKED_BODIES];
        let active_before = self.active_count();
        let capacity_open = self.slots[..self.max_tracked]
            .iter()
            .any(|s| s.phase != SlotPhase::Active);
        let need_detect = active_before == 0 || (capacity_open && now >= self.next_scan);
        if need_detect {
            diag.detector_reason = if active_before == 0 {
                DetectorRunReason::ColdStart
            } else {
                DetectorRunReason::Scan
            };
            let stage = Instant::now();
            let detected = self.detect_clusters(&square);
            diag.detector = stage.elapsed();
            if let Err(e) = detected {
                self.square_buf = square;
                return Err(e);
            }
            self.next_scan = now + DETECT_SCAN_INTERVAL;
            self.associate_detections(now, &mut fresh);
        } else {
            diag.detector_reason = DetectorRunReason::Tracking;
        }
        diag.people_detected = self.people_detected;

        // Landmark/mask inference over the active slots, budgeted (see the
        // module doc): all of them at ≤ MAX_FULL_INFERENCE_SLOTS, round-robin
        // otherwise, with freshly-activated slots jumping the queue.
        let run_set = self.plan_inference(fresh);
        let active_before_infer = self.active_count();
        let stage = Instant::now();
        for slot_idx in run_set.into_iter().flatten() {
            let outcome = self.run_slot_inference(
                slot_idx,
                &square,
                content,
                now,
                fresh[slot_idx],
                blend_ratio,
            );
            if let Err(e) = outcome {
                self.square_buf = square;
                return Err(e);
            }
        }
        diag.landmark = stage.elapsed();
        self.square_buf = square;
        if self.active_count() < active_before_infer {
            // A track was lost this frame: re-scan immediately next frame
            // (re-acquire the person, or confirm the exit) instead of waiting
            // out the discovery interval — matching the old single-track
            // "re-detect right after a loss" behaviour.
            self.next_scan = now;
        }

        // Skipped-but-active slots: refresh crop/size from the carried ROI so
        // primary scoring stays current even between their inference turns.
        for slot in &mut self.slots {
            if slot.phase == SlotPhase::Active {
                if let Some(roi) = slot.roi {
                    let (crop, size) = roi_metrics(&roi, content);
                    slot.frame.crop_fraction = crop;
                    slot.frame.size = size;
                }
            } else {
                // Absent slots: fade their mask channel so no stale
                // silhouette lingers (the graceful-fade extra; same knob).
                slot.mask.decay(blend_ratio);
            }
        }

        // Payload: every slot's accumulator is written every frame (the
        // pooled buffers rotate, so each must carry the full picture), then
        // the slot-partitioned edge list.
        if let Some(payload) = payload {
            self.write_payload(payload);
        }

        self.publish_slot_frames(&mut diag);
        diag.total = frame_start.elapsed();
        self.last_diagnostics = diag;
        Ok(())
    }

    /// Invalid-frame path of [`Self::process`]: a bad frame breaks tracking,
    /// so every active slot is reserved and re-acquired next frame
    /// (association routes returning people back to their slots), the masks
    /// decay, and the frame publishes as all-absent.
    fn process_invalid_frame(
        &mut self,
        now: Duration,
        blend_ratio: f32,
        payload: Option<&mut BodyFramePayload>,
        mut diag: PoseDiagnostics,
        frame_start: Instant,
    ) {
        for slot in &mut self.slots {
            if slot.phase == SlotPhase::Active {
                slot.lose(now);
            }
        }
        diag.detector_reason = DetectorRunReason::InvalidFrame;
        self.decay_masks_into(blend_ratio, payload);
        self.publish_slot_frames(&mut diag);
        // No detector ran; carry the most-recent candidate count.
        diag.people_detected = self.people_detected;
        diag.total = frame_start.elapsed();
        self.last_diagnostics = diag;
    }

    /// Idle-probe path of [`Self::process`]: the detector alone is a presence
    /// sensor. Carried crop tracks are stale after idle, so every slot is
    /// released; presence + confidence report through slot 0 (landmarks stay
    /// defaults — nothing renders during idle; the wake path only needs the
    /// presence bit), and the mask channels decay.
    fn process_idle_probe(
        &mut self,
        square: RgbImage,
        blend_ratio: f32,
        payload: Option<&mut BodyFramePayload>,
        mut diag: PoseDiagnostics,
        frame_start: Instant,
    ) -> Result<(), InferenceError> {
        for slot in &mut self.slots {
            if slot.phase != SlotPhase::Free {
                slot.release();
            }
        }
        diag.detector_reason = DetectorRunReason::IdleProbe;
        let stage = Instant::now();
        let detected = self.detect_clusters(&square);
        diag.detector = stage.elapsed();
        self.square_buf = square;
        detected?;
        let (present, confidence) = self
            .person_clusters
            .first()
            .map_or((false, 0.0), |d| (true, d.score));
        self.slots[0].frame = SlotFrame {
            present,
            confidence,
            ..SlotFrame::default()
        };
        self.decay_masks_into(blend_ratio, payload);
        self.publish_slot_frames(&mut diag);
        diag.people_detected = self.people_detected;
        diag.total = frame_start.elapsed();
        self.last_diagnostics = diag;
        Ok(())
    }

    /// Number of [`SlotPhase::Active`] slots.
    fn active_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|s| s.phase == SlotPhase::Active)
            .count()
    }

    /// Copy per-slot frames into the published array and fold the per-slot
    /// presence/confidence into the diagnostics.
    fn publish_slot_frames(&mut self, diag: &mut PoseDiagnostics) {
        let mut present = false;
        let mut confidence = 0.0_f32;
        for (dst, slot) in self.slot_frames.iter_mut().zip(&self.slots) {
            *dst = slot.frame;
            if slot.frame.present {
                present = true;
                confidence = confidence.max(slot.frame.confidence);
            }
        }
        diag.present = present;
        diag.confidence = confidence;
        diag.active_tracks = u8::try_from(self.active_count()).unwrap_or(u8::MAX);
    }

    /// Detector stage: resize → NHWC tensor → run → decode → weighted NMS,
    /// leaving the bounded person-cluster candidates in `self.person_clusters`
    /// (descending score, `out[0]` = top person). Allocation-free (all buffers
    /// reused). The caller associates candidates to slots.
    fn detect_clusters(&mut self, square: &RgbImage) -> Result<(), InferenceError> {
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
        weighted_nms_into(
            &mut self.detections,
            PERSON_BLEND_IOU,
            MAX_PERSON_CANDIDATES,
            &mut self.person_clusters,
        );
        self.people_detected = u8::try_from(self.person_clusters.len()).unwrap_or(u8::MAX);
        Ok(())
    }

    /// Associate the latest detector candidates to slots
    /// ([`super::selection::assign_slots`]): candidates matched to an
    /// *active* slot are consumed (no duplicate track), matches to a
    /// *reserved* slot re-activate it (the returning person keeps their slot
    /// and mask channel), and unmatched candidates claim free slots below the
    /// configured cap. Newly (re)activated slots are marked `fresh` so their
    /// aux filter cold-starts and their inference jumps the queue.
    fn associate_detections(&mut self, now: Duration, fresh: &mut [bool; MAX_TRACKED_BODIES]) {
        let mut anchors: [Option<Vec2>; MAX_TRACKED_BODIES] = [None; MAX_TRACKED_BODIES];
        let mut claimable = [false; MAX_TRACKED_BODIES];
        for (i, slot) in self.slots.iter().enumerate() {
            match slot.phase {
                SlotPhase::Active | SlotPhase::Reserved => anchors[i] = Some(slot.anchor),
                SlotPhase::Free => claimable[i] = i < self.max_tracked,
            }
        }
        // Candidate centres (ROI centres — keypoint 0, the mid-hip).
        let mut centres = [Vec2::ZERO; MAX_PERSON_CANDIDATES];
        let n = self.person_clusters.len().min(MAX_PERSON_CANDIDATES);
        for (c, det) in self.person_clusters.iter().take(n).enumerate() {
            let roi = roi_from_detection(det);
            centres[c] = Vec2::new(roi.cx, roi.cy);
        }
        let assigned = assign_slots(&centres[..n], &anchors, &claimable);
        for (c, slot_idx) in assigned.iter().take(n).enumerate() {
            let Some(s) = *slot_idx else { continue };
            let slot = &mut self.slots[s];
            match slot.phase {
                // Already tracked: the candidate is the same person; the
                // carried aux ROI stays authoritative (smoother than a raw
                // detection). Consuming the match prevents duplicate claims.
                SlotPhase::Active => {}
                SlotPhase::Reserved | SlotPhase::Free => {
                    if slot.phase == SlotPhase::Free {
                        // A fresh occupancy starts its age clock here; a
                        // Reserved re-acquisition keeps the original
                        // active_since (the same person's visit continues,
                        // and their main-side fade may be mid-flight).
                        slot.active_since = now;
                    }
                    slot.phase = SlotPhase::Active;
                    slot.roi = Some(roi_from_detection(&self.person_clusters[c]));
                    slot.anchor = centres[c];
                    fresh[s] = true;
                }
            }
        }
    }

    /// Choose which active slots run landmark/mask inference this frame.
    /// Returns up to [`MAX_TRACKED_BODIES`] slot indices (`None`-padded).
    /// All actives run when ≤ [`MAX_FULL_INFERENCE_SLOTS`]; otherwise fresh
    /// slots first, then round-robin from `rr_next`, capped at the budget.
    fn plan_inference(
        &mut self,
        fresh: [bool; MAX_TRACKED_BODIES],
    ) -> [Option<usize>; MAX_TRACKED_BODIES] {
        let mut run: [Option<usize>; MAX_TRACKED_BODIES] = [None; MAX_TRACKED_BODIES];
        let mut count = 0usize;
        let active = self.active_count();
        if active <= MAX_FULL_INFERENCE_SLOTS {
            for (i, slot) in self.slots.iter().enumerate() {
                if slot.phase == SlotPhase::Active && slot.roi.is_some() {
                    run[count] = Some(i);
                    count += 1;
                }
            }
            return run;
        }
        // Over budget: fresh slots jump the queue (a new person must appear
        // immediately)…
        for (i, _) in fresh.iter().enumerate().filter(|(_, f)| **f) {
            if count == MAX_FULL_INFERENCE_SLOTS {
                break;
            }
            if self.slots[i].phase == SlotPhase::Active && self.slots[i].roi.is_some() {
                run[count] = Some(i);
                count += 1;
                // Advance the cursor past a fresh pick too, so the next
                // frame's round-robin starts at the slots this frame skipped.
                self.rr_next = (i + 1) % MAX_TRACKED_BODIES;
            }
        }
        // …then round-robin over the remaining actives.
        for step in 0..MAX_TRACKED_BODIES {
            if count == MAX_FULL_INFERENCE_SLOTS {
                break;
            }
            let i = (self.rr_next + step) % MAX_TRACKED_BODIES;
            if self.slots[i].phase == SlotPhase::Active
                && self.slots[i].roi.is_some()
                && !run[..count].contains(&Some(i))
            {
                run[count] = Some(i);
                count += 1;
                self.rr_next = (i + 1) % MAX_TRACKED_BODIES;
            }
        }
        run
    }

    /// Landmark/mask stage for one slot: warp the ROI crop, run the landmark
    /// model, gate on presence, refine + project, derive next frame's ROI
    /// through the slot's aux filter, ingest the slot's mask channel, and
    /// update the slot's [`SlotFrame`]. A lost/untrackable outcome reserves
    /// the slot.
    fn run_slot_inference(
        &mut self,
        slot_idx: usize,
        square: &RgbImage,
        content: ContentRect,
        now: Duration,
        fresh_track: bool,
        blend_ratio: f32,
    ) -> Result<(), InferenceError> {
        // Split borrows: the model/scratch fields and the slot are disjoint.
        let Self {
            ref mut landmark,
            ref mut landmark_input,
            ref mut landmark_outputs,
            ref mut warp_buf,
            ref config,
            ref mut slots,
            ..
        } = *self;
        let slot = &mut slots[slot_idx];
        let Some(roi) = slot.roi else {
            return Ok(());
        };

        warp_roi_into(square, &roi, LM_SIZE, warp_buf);
        fill_nhwc_unit(warp_buf, landmark_input);
        landmark.run(landmark_input, landmark_outputs)?;
        let picked = pick_pose_landmark_outputs(landmark_outputs)?;
        if picked.confidence < config.presence_threshold {
            // Presence collapsed: the person left this crop.
            slot.lose(now);
            return Ok(());
        }

        // Heatmap landmark refinement (upstream `RefineLandmarksFromHeatmap`),
        // in crop space before projection and the aux filter. Copy the raw
        // regression rows into a stack scratch array, refine x/y in place, then
        // project. Skipped when the A/B toggle is set or the model emitted no
        // heatmap. No allocation: `refined` is a fixed 195-float stack array.
        let mut refined = [0.0_f32; LANDMARK_ROWS * LANDMARK_VALUES];
        let copy_len = refined.len().min(picked.landmarks.len());
        refined[..copy_len].copy_from_slice(&picked.landmarks[..copy_len]);
        if !config.disable_heatmap_refine {
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
            slot.aux_filter.reset();
        }
        let (aux_center, aux_scale) = slot.aux_filter.filter(
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

        // Mask into this slot's accumulator (channel write happens in the
        // shared payload pass).
        slot.mask.ingest(picked.mask, &roi, content, blend_ratio);

        let (crop_fraction, size) = roi_metrics(&next_roi, content);
        slot.frame = SlotFrame {
            present: true,
            confidence: picked.confidence,
            landmarks,
            world_landmarks,
            crop_fraction,
            size,
        };
        slot.anchor = Vec2::new(next_roi.cx, next_roi.cy);
        if roi_trackable(&next_roi, content) {
            slot.roi = Some(next_roi);
        } else {
            // The carried crop collapsed (frame edge / transient aux wobble).
            // THIS frame's landmarks are still valid — `present` stays true —
            // but the carried track is dropped and the slot reserved; the
            // caller schedules an immediate re-scan, so next frame either
            // re-acquires the person into this same slot or confirms the
            // exit (the reserved-clear pass then reads it absent). A mature
            // track earns the full window (its main-side fade may be
            // mid-flight); a young one gets only [`YOUNG_EDGE_RESERVE`] so
            // drive-past traffic cannot zombie the slot.
            slot.phase = SlotPhase::Reserved;
            slot.reserved_until = now
                + if slot.earns_reserve(now) {
                    SLOT_RESERVE
                } else {
                    YOUNG_EDGE_RESERVE
                };
            slot.roi = None;
        }
        Ok(())
    }

    /// Person-absent / probe path helper: decay every slot's mask accumulator
    /// toward empty and, when a payload is supplied, publish the faded RGBA
    /// mask + its (shrinking) slot-partitioned edge list so a stale
    /// silhouette never lingers on screen. (The decay is our own
    /// graceful-fade extra, not part of the upstream blend; it keeps its
    /// original EMA-style `acc -= acc·alpha` behavior, driven by the same
    /// knob.)
    fn decay_masks_into(&mut self, alpha: f32, payload: Option<&mut BodyFramePayload>) {
        for slot in &mut self.slots {
            slot.mask.decay(alpha);
        }
        if let Some(payload) = payload {
            self.write_payload(payload);
        }
    }

    /// Write every slot's mask channel + the slot-partitioned edge list into
    /// the pooled payload (in place; no allocation).
    fn write_payload(&mut self, payload: &mut BodyFramePayload) {
        payload.edges.clear();
        for (i, slot) in self.slots.iter().enumerate() {
            slot.mask.write_channel(&mut payload.mask, MASK_CHANNELS, i);
            payload.edge_slot_counts[i] =
                extract_edges_append(slot.mask.smoothed(), &mut payload.edges);
        }
    }
}

/// Axis-aligned bbox of a (possibly rotated) ROI square, rotation ignored —
/// a cheap approximation that is exact for upright bodies and close enough
/// for crop/size scoring.
fn roi_bbox(roi: &RoiRect) -> Rect {
    let half = roi.size * 0.5;
    Rect {
        xmin: roi.cx - half,
        ymin: roi.cy - half,
        xmax: roi.cx + half,
        ymax: roi.cy + half,
    }
}

/// `(crop_fraction, size)` for a slot's ROI against the camera content rect:
/// crop = fraction of the ROI bbox inside the content (1.0 = fully framed),
/// size = normalized ROI area (the closest-person proxy). Both are computed
/// from the same ROI source every frame so cross-body comparisons are fair.
fn roi_metrics(roi: &RoiRect, content: ContentRect) -> (f32, f32) {
    let bounds = Rect {
        xmin: content.x0,
        ymin: content.y0,
        xmax: content.x1,
        ymax: content.y1,
    };
    (visible_fraction(roi_bbox(roi), bounds), roi.size * roi.size)
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
        // Deliberate departure from upstream: a marginally-negative coord is
        // skipped here, where upstream's int cast truncates it to cell 0 and
        // refines anyway. Off-crop landmarks are already unreliable; leaving
        // them unrefined is the conservative reading.
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
/// `resize_into`; upstream `MediaPipe`'s CPU `ImageToTensor` is likewise
/// bilinear — `warpAffine` with `LINEAR` sampling).
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

    /// Anchor for a second person at stride-8 grid cell (4, 4): image
    /// position ≈ (4.5/28, 4.5/28) ≈ (0.161, 0.161), well clear of
    /// [`HOT_ANCHOR`]'s ≈ (0.518, 0.518) so the two never blend into one
    /// weighted-NMS cluster.
    pub(crate) const PERSON_B_ANCHOR: usize = (4 * 28 + 4) * 2;

    /// Anchor for a third person at stride-8 grid cell (24, 24): image
    /// position ≈ (24.5/28, 24.5/28) ≈ (0.875, 0.875) — far from A and B.
    pub(crate) const PERSON_C_ANCHOR: usize = (24 * 28 + 24) * 2;

    /// Image-space centre (both axes) of the person at [`HOT_ANCHOR`] / the
    /// second person at [`PERSON_B_ANCHOR`]. Keypoint 0 sits at the anchor
    /// centre, so these are the ROI centres selection compares.
    pub(crate) const PERSON_A_CENTER: f32 = 14.5 / 28.0;
    /// See [`PERSON_A_CENTER`].
    pub(crate) const PERSON_B_CENTER: f32 = 4.5 / 28.0;
    /// See [`PERSON_A_CENTER`].
    pub(crate) const PERSON_C_CENTER: f32 = 24.5 / 28.0;

    /// Fill one person's box/score at `anchor` (0.3² box, scale point 0.15
    /// above — upright ROI) with raw logit `raw`.
    fn set_person(boxes: &mut [f32], scores: &mut [f32], anchor: usize, raw: f32) {
        let base = anchor * POSE_REGRESSION_LEN;
        boxes[base + 2] = 224.0 * 0.3; // w
        boxes[base + 3] = 224.0 * 0.3; // h
        boxes[base + 7] = -224.0 * 0.15; // kp1 y offset: 0.15 up
        scores[anchor] = raw;
    }

    /// Detector outputs with TWO confident people: person A at [`HOT_ANCHOR`]
    /// and person B at [`PERSON_B_ANCHOR`]. `raw_score_a`/`raw_score_b` are
    /// raw logits (pre-sigmoid) so a caller can make either the higher scorer.
    pub(crate) fn two_person_detector_outputs(raw_score_a: f32, raw_score_b: f32) -> Vec<Tensor> {
        let mut boxes = vec![0.0_f32; POSE_ANCHOR_COUNT * POSE_REGRESSION_LEN];
        let mut scores = vec![-100.0_f32; POSE_ANCHOR_COUNT];
        set_person(&mut boxes, &mut scores, HOT_ANCHOR, raw_score_a);
        set_person(&mut boxes, &mut scores, PERSON_B_ANCHOR, raw_score_b);
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

    /// Detector outputs with THREE confident, well-separated people (A, B, C
    /// anchors; descending scores A > B > C).
    pub(crate) fn three_person_detector_outputs() -> Vec<Tensor> {
        let mut boxes = vec![0.0_f32; POSE_ANCHOR_COUNT * POSE_REGRESSION_LEN];
        let mut scores = vec![-100.0_f32; POSE_ANCHOR_COUNT];
        set_person(&mut boxes, &mut scores, HOT_ANCHOR, 6.0);
        set_person(&mut boxes, &mut scores, PERSON_B_ANCHOR, 4.0);
        set_person(&mut boxes, &mut scores, PERSON_C_ANCHOR, 2.0);
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
    /// the tracking ROI is unchanged) and a cell-quantization shift (up to a
    /// few crop pixels — one 64-grid cell spans 4) for off-centre ones. Tests
    /// asserting exact landmark positions build their own blob heatmap; see
    /// `heatmap_refinement_*`.
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

    fn payload() -> BodyFramePayload {
        BodyFramePayload::new()
    }

    #[test]
    fn cold_start_detects_then_tracks() {
        let mut p = person_pipeline();
        let mut pl = payload();
        let frame = solid_frame();

        p.process(&frame, Duration::from_millis(0), false, Some(&mut pl))
            .expect("frame 1");
        let s0 = &p.slot_frames()[0];
        assert!(s0.present);
        assert!(s0.confidence > 0.8);
        assert_eq!(
            p.diagnostics().detector_reason,
            DetectorRunReason::ColdStart
        );
        assert_eq!(p.diagnostics().active_tracks, 1);
        // Landmarks land in content-norm [0, 1] with high visibility.
        for lm in &s0.landmarks {
            assert!(lm.pos.x.is_finite() && lm.pos.y.is_finite());
            assert!(lm.visibility > 0.7, "vis={}", lm.visibility);
        }
        // World landmarks decode from the [1, 117] tensor.
        assert!((s0.world_landmarks[0].x - 0.1).abs() < 1e-5);
        assert!((s0.world_landmarks[0].y - (-0.2)).abs() < 1e-5);
        // crop/size metrics are published (fully-framed synthetic person).
        assert!(s0.crop_fraction > 0.9, "crop={}", s0.crop_fraction);
        assert!(s0.size > 0.0);

        // Frame 2: the carried aux-row track skips the detector entirely
        // (one active track, no free capacity scan due yet).
        p.process(&frame, Duration::from_millis(16), false, Some(&mut pl))
            .expect("frame 2");
        assert!(p.slot_frames()[0].present);
        assert_eq!(p.diagnostics().detector_reason, DetectorRunReason::Tracking);
    }

    #[test]
    fn mask_and_edges_land_in_the_slot0_channel() {
        let mut p = person_pipeline();
        let mut pl = payload();
        p.process(
            &solid_frame(),
            Duration::from_millis(0),
            false,
            Some(&mut pl),
        )
        .expect("process");
        // Slot 0 = channel R of the RGBA-interleaved payload.
        let max_r = pl.mask.chunks_exact(4).map(|t| t[0]).max().unwrap_or(0);
        assert!(max_r > 200, "slot-0 channel never lit: max={max_r}");
        let max_g = pl.mask.chunks_exact(4).map(|t| t[1]).max().unwrap_or(0);
        assert_eq!(max_g, 0, "unoccupied slot 1 channel must stay dark");
        assert!(!pl.edges.is_empty(), "edges must be extracted");
        assert!(pl.edges.len() <= crate::input::body::MAX_EDGE_POINTS);
        assert_eq!(pl.edge_slot_counts[0], pl.edges.len());
        assert_eq!(pl.edge_slot_counts[1], 0);
    }

    /// (The loss here happens at track age 0 — under `RESERVE_MIN_ACTIVE` — so
    /// the slot frees outright rather than reserving; either way the next
    /// frame must cold-start re-detect.)
    #[test]
    fn low_landmark_confidence_drops_the_slot_and_recovers() {
        let mut p = PosePipeline::new(
            Box::new(StaticInference {
                outputs: hot_person_detector_outputs(),
            }),
            Box::new(StaticInference {
                outputs: low_confidence_landmark_outputs(),
            }),
            PoseConfig::default(),
        );
        let mut pl = payload();
        p.process(
            &solid_frame(),
            Duration::from_millis(0),
            false,
            Some(&mut pl),
        )
        .expect("process");
        assert!(
            !p.slot_frames()[0].present,
            "conf below threshold must read absent"
        );
        // Next frame must re-detect (no active track).
        p.process(
            &solid_frame(),
            Duration::from_millis(16),
            false,
            Some(&mut pl),
        )
        .expect("frame 2");
        assert_eq!(
            p.diagnostics().detector_reason,
            DetectorRunReason::ColdStart
        );
    }

    #[test]
    fn empty_detector_output_reads_absent() {
        let mut p = PosePipeline::new(
            Box::new(StaticInference {
                outputs: empty_detector_outputs(),
            }),
            Box::new(FailingInference), // landmark stage must not run
            PoseConfig::default(),
        );
        p.process(&solid_frame(), Duration::from_millis(0), false, None)
            .expect("process");
        assert!(p.slot_frames().iter().all(|s| !s.present));
        assert!(!p.diagnostics().present);
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
        p.process(&solid_frame(), Duration::from_millis(0), true, None)
            .expect("probe");
        let s0 = &p.slot_frames()[0];
        assert!(s0.present, "idle probe must still report presence");
        assert!(s0.confidence > 0.8);
        assert_eq!(
            p.diagnostics().detector_reason,
            DetectorRunReason::IdleProbe
        );
    }

    #[test]
    fn invalid_frame_reserves_tracks_then_reacquires() {
        let mut p = person_pipeline();
        let good = solid_frame();
        p.process(&good, Duration::from_millis(0), false, None)
            .expect("acquire");
        let bad = Frame {
            width: 10, // inconsistent: no bytes
            ..Frame::default()
        };
        p.process(&bad, Duration::from_millis(16), false, None)
            .expect("invalid frame is not an error");
        assert!(!p.slot_frames()[0].present);
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
        // The person lands back in slot 0 (a young track freed by the
        // fast-free rule re-claims the lowest free slot; a mature one would
        // re-bind to its reservation — same outcome either way here).
        assert!(p.slot_frames()[0].present, "same slot re-acquired");
    }

    /// Busy-road fast-free: a confirmed loss ([`SlotTrack::lose`] — presence
    /// collapse or an invalid frame) before `RESERVE_MIN_ACTIVE` must skip
    /// the Reserved phase entirely — the slot is Free (claimable by the next
    /// person) the moment the track drops, instead of a 4 s zombie
    /// reservation. At the age boundary, the loss reserves.
    #[test]
    fn short_lived_track_frees_immediately_instead_of_reserving() {
        let mut young = SlotTrack::new();
        young.phase = SlotPhase::Active;
        young.active_since = Duration::ZERO;
        young.frame.present = true;
        young.lose(Duration::from_millis(200));
        assert_eq!(
            young.phase,
            SlotPhase::Free,
            "young track must free, not reserve"
        );
        assert!(!young.frame.present, "released slot reads absent");

        let mut mature = SlotTrack::new();
        mature.phase = SlotPhase::Active;
        mature.active_since = Duration::ZERO;
        mature.frame.present = true;
        mature.lose(RESERVE_MIN_ACTIVE);
        assert_eq!(
            mature.phase,
            SlotPhase::Reserved,
            "at the age boundary the loss takes the reserved path"
        );
    }

    /// The untrackable-crop branch is NOT a confirmed loss: a young track
    /// whose carried ROI collapses reserves for [`YOUNG_EDGE_RESERVE`] (long
    /// enough for the immediate re-scan to re-bind the same person) rather
    /// than hard-freeing — and that window must actually cover a detector
    /// re-scan cycle, else the contract is vacuous.
    #[test]
    fn young_edge_reserve_covers_a_rescan_cycle() {
        assert!(
            YOUNG_EDGE_RESERVE > DETECT_SCAN_INTERVAL,
            "young edge reservation ({YOUNG_EDGE_RESERVE:?}) must outlast a \
             discovery-scan interval ({DETECT_SCAN_INTERVAL:?}) so the re-scan \
             can re-acquire into the reserved slot"
        );
        assert!(
            YOUNG_EDGE_RESERVE < SLOT_RESERVE,
            "the whole point is a shorter window than the mature reservation"
        );
    }

    /// The complement: a track older than `RESERVE_MIN_ACTIVE` still takes the
    /// full Reserved path on loss (the mask channel may be mid-fade on the
    /// main thread), and a Reserved→Active re-acquisition keeps the original
    /// age — the same person's later loss still reserves.
    #[test]
    fn mature_track_reserves_and_reacquisition_keeps_its_age() {
        struct SharedInference(std::sync::Arc<std::sync::Mutex<Vec<Tensor>>>);
        impl ModelInference for SharedInference {
            fn run(
                &mut self,
                _input: &Tensor,
                out: &mut Vec<Tensor>,
            ) -> Result<(), InferenceError> {
                out.clone_from(&self.0.lock().expect("outputs lock"));
                Ok(())
            }
        }
        let landmark = std::sync::Arc::new(std::sync::Mutex::new(confident_landmark_outputs()));
        let mut p = PosePipeline::new(
            Box::new(StaticInference {
                outputs: hot_person_detector_outputs(),
            }),
            Box::new(SharedInference(std::sync::Arc::clone(&landmark))),
            PoseConfig::default(),
        );
        let good = solid_frame();
        p.process(&good, Duration::from_millis(0), false, None)
            .expect("acquire");
        // An invalid frame at 700 ms (age ≥ RESERVE_MIN_ACTIVE): reserves.
        let bad = Frame {
            width: 10,
            ..Frame::default()
        };
        p.process(&bad, Duration::from_millis(700), false, None)
            .expect("invalid");
        assert_eq!(
            p.slots[0].phase,
            SlotPhase::Reserved,
            "mature track must reserve on loss"
        );
        // Re-acquire the same person into the reservation…
        p.process(&good, Duration::from_millis(750), false, None)
            .expect("reacquire");
        assert_eq!(p.slots[0].phase, SlotPhase::Active);
        // …and a loss 50 ms later must STILL reserve: the occupancy's age
        // carries across the re-acquisition (750 ms > 600 ms), it does not
        // restart at the re-acquisition instant.
        *landmark.lock().expect("outputs lock") = low_confidence_landmark_outputs();
        p.process(&good, Duration::from_millis(800), false, None)
            .expect("second loss");
        assert_eq!(
            p.slots[0].phase,
            SlotPhase::Reserved,
            "re-acquired occupancy keeps its original age"
        );
    }

    /// The cross-thread invariant behind the fast-free: the worker's
    /// `RESERVE_MIN_ACTIVE` must sit strictly below the main thread's
    /// `ADMIT_DWELL` (with jitter margin), so a track that could have ignited
    /// a fade always earns the reservation that protects that fade.
    #[test]
    fn reserve_min_active_sits_under_the_admission_dwell() {
        let margin = crate::input::body::envelope::ADMIT_DWELL.saturating_sub(RESERVE_MIN_ACTIVE);
        assert!(
            margin >= Duration::from_millis(66),
            "need ≥ two 30 Hz frames of transport-jitter margin, got {margin:?}"
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
        let mut pl = payload();
        p.process(
            &solid_frame(),
            Duration::from_millis(0),
            false,
            Some(&mut pl),
        )
        .expect("process");
    }

    #[test]
    fn two_people_occupy_two_stable_slots() {
        let mut p = PosePipeline::new(
            Box::new(StaticInference {
                outputs: two_person_detector_outputs(4.0, 2.0),
            }),
            Box::new(StaticInference {
                outputs: confident_landmark_outputs(),
            }),
            PoseConfig::default(),
        );
        let mut pl = payload();
        let frame = solid_frame();
        p.process(&frame, Duration::from_millis(0), false, Some(&mut pl))
            .expect("frame 1");
        assert_eq!(p.diagnostics().people_detected, 2);
        assert_eq!(p.diagnostics().active_tracks, 2);
        assert!(p.slot_frames()[0].present && p.slot_frames()[1].present);
        // Slot 0 got the higher scorer (person A); each slot's landmarks are
        // projected through its own ROI, so the two poses land near their own
        // person's centre.
        let x0 = p.slot_frames()[0].landmarks[0].pos.x;
        let x1 = p.slot_frames()[1].landmarks[0].pos.x;
        assert!(
            (x0 - PERSON_A_CENTER).abs() < 0.05,
            "slot 0 tracks person A: {x0}"
        );
        assert!(
            (x1 - PERSON_B_CENTER).abs() < 0.05,
            "slot 1 tracks person B: {x1}"
        );
        // Both mask channels lit; edge partition covers both slots.
        let max_r = pl.mask.chunks_exact(4).map(|t| t[0]).max().unwrap_or(0);
        let max_g = pl.mask.chunks_exact(4).map(|t| t[1]).max().unwrap_or(0);
        assert!(
            max_r > 200 && max_g > 200,
            "both channels lit: R={max_r} G={max_g}"
        );
        assert!(pl.edge_slot_counts[0] > 0 && pl.edge_slot_counts[1] > 0);
        assert_eq!(
            pl.edge_slot_counts.iter().sum::<usize>(),
            pl.edges.len(),
            "counts partition the edge list"
        );

        // Tracking frames keep both slots without re-detecting.
        p.process(&frame, Duration::from_millis(16), false, Some(&mut pl))
            .expect("frame 2");
        assert_eq!(p.diagnostics().detector_reason, DetectorRunReason::Tracking);
        assert!(p.slot_frames()[0].present && p.slot_frames()[1].present);
    }

    #[test]
    fn max_tracked_bodies_caps_claimed_slots() {
        let mut p = PosePipeline::new(
            Box::new(StaticInference {
                outputs: two_person_detector_outputs(4.0, 2.0),
            }),
            Box::new(StaticInference {
                outputs: confident_landmark_outputs(),
            }),
            PoseConfig {
                max_tracked_bodies: 1,
                ..PoseConfig::default()
            },
        );
        p.process(&solid_frame(), Duration::from_millis(0), false, None)
            .expect("frame");
        assert_eq!(p.diagnostics().people_detected, 2, "both detected");
        assert_eq!(p.diagnostics().active_tracks, 1, "but only one tracked");
        assert!(p.slot_frames()[0].present);
        assert!(!p.slot_frames()[1].present);
    }

    #[test]
    fn three_people_interleave_round_robin() {
        let mut p = PosePipeline::new(
            Box::new(StaticInference {
                outputs: three_person_detector_outputs(),
            }),
            Box::new(StaticInference {
                outputs: confident_landmark_outputs(),
            }),
            PoseConfig::default(),
        );
        let frame = solid_frame();
        // Frame 1: three fresh tracks, budget 2 → two run now, one is active
        // but not yet inferred.
        p.process(&frame, Duration::from_millis(0), false, None)
            .expect("frame 1");
        assert_eq!(p.diagnostics().active_tracks, 3);
        let present_1 = p.slot_frames().iter().filter(|s| s.present).count();
        assert_eq!(present_1, 2, "budget: two inferences on frame 1");
        // Frame 2: the round-robin reaches the remaining slot; all present.
        p.process(&frame, Duration::from_millis(16), false, None)
            .expect("frame 2");
        let present_2 = p.slot_frames().iter().filter(|s| s.present).count();
        assert_eq!(present_2, 3, "round-robin covers every active slot");
        // The late-inferred slot 2 tracks person C (its own ROI, not a copy).
        let x2 = p.slot_frames()[2].landmarks[0].pos.x;
        assert!(
            (x2 - PERSON_C_CENTER).abs() < 0.05,
            "slot 2 tracks person C: {x2}"
        );
        // Steady state stays all-present (held frames for skipped slots).
        p.process(&frame, Duration::from_millis(32), false, None)
            .expect("frame 3");
        assert_eq!(p.slot_frames().iter().filter(|s| s.present).count(), 3);
    }

    #[test]
    fn discovery_scan_admits_a_second_person_mid_track() {
        // Start with one person, then the detector begins reporting two: the
        // periodic scan must claim a slot for the newcomer within the scan
        // interval, without disturbing slot 0.
        struct SharedInference(std::sync::Arc<std::sync::Mutex<Vec<Tensor>>>);
        impl ModelInference for SharedInference {
            fn run(
                &mut self,
                _input: &Tensor,
                out: &mut Vec<Tensor>,
            ) -> Result<(), InferenceError> {
                out.clone_from(&self.0.lock().expect("outputs lock"));
                Ok(())
            }
        }

        let outputs = std::sync::Arc::new(std::sync::Mutex::new(two_person_detector_outputs(
            4.0, -100.0, // person B far below threshold at first
        )));
        let mut p = PosePipeline::new(
            Box::new(SharedInference(std::sync::Arc::clone(&outputs))),
            Box::new(StaticInference {
                outputs: confident_landmark_outputs(),
            }),
            PoseConfig::default(),
        );
        let frame = solid_frame();
        p.process(&frame, Duration::from_millis(0), false, None)
            .expect("frame 1");
        assert_eq!(p.diagnostics().active_tracks, 1);

        // Person B walks in.
        *outputs.lock().expect("outputs lock") = two_person_detector_outputs(4.0, 4.0);
        // Just after the scan interval elapses, the detector re-runs.
        p.process(&frame, Duration::from_millis(350), false, None)
            .expect("scan frame");
        assert_eq!(p.diagnostics().detector_reason, DetectorRunReason::Scan);
        assert!(
            p.slot_frames()[0].present && p.slot_frames()[1].present,
            "newcomer admitted beside the incumbent"
        );
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

    #[test]
    fn roi_metrics_report_crop_and_size() {
        let content = ContentRect::for_frame(256, 256);
        // Fully-inside ROI: crop 1, size = 0.4².
        let inside = RoiRect {
            cx: 0.5,
            cy: 0.5,
            size: 0.4,
            rotation: 0.0,
        };
        let (crop, size) = roi_metrics(&inside, content);
        assert!((crop - 1.0).abs() < 1e-6);
        assert!((size - 0.16).abs() < 1e-6);
        // Half off the left edge: crop ≈ 0.5.
        let edge = RoiRect {
            cx: 0.0,
            cy: 0.5,
            size: 0.4,
            rotation: 0.0,
        };
        let (crop_edge, _) = roi_metrics(&edge, content);
        assert!((crop_edge - 0.5).abs() < 1e-3, "crop={crop_edge}");
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
        on.process(&solid_frame(), Duration::from_millis(0), false, None)
            .expect("refine on");
        off.process(&solid_frame(), Duration::from_millis(0), false, None)
            .expect("refine off");
        let r_on = &on.slot_frames()[0];
        let r_off = &off.slot_frames()[0];
        assert!(r_on.present && r_off.present);
        assert!(
            r_on.landmarks[0].pos.x > r_off.landmarks[0].pos.x + 1e-3,
            "refinement must move landmark 0 (on={}, off={})",
            r_on.landmarks[0].pos.x,
            r_off.landmarks[0].pos.x
        );
    }
}
