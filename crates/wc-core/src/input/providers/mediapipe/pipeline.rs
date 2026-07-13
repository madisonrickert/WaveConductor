//! Two-stage hand pipeline: a [`super::capture::Frame`] in, [`Hand`]s out.
//!
//! Wires the validated stages together: square-pad + resize the frame and run
//! palm detection ([`super::inference`]); decode + NMS ([`super::palm`]); for
//! each detection build the rotated ROI ([`super::landmark`]), warp-crop it, run
//! the landmark model, project the landmarks back to image space, derive the
//! per-hand signals ([`super::signals`]), and map into the Leap-device-mm
//! convention ([`super::coords`]) the rest of the app consumes.
//!
//! Preprocessing constants (`/255` RGB, square-pad → 192; decode scales; ROI
//! factors) were validated against the Python oracle on a real hand — see the
//! design spec's *Spike results*.
//!
//! Foundation module: driven by the worker (plan Phase 8.2); exercised by a
//! hermetic mock test plus an env-var-gated end-to-end check.
//!
//! ## Coordinate spaces
//!
//! Four spaces flow through the pipeline (documented here to prevent
//! coordinate-space bugs from regressing silently):
//!
//! 1. **Square-norm** `[0, 1]²` — the padded-square image space. The model
//!    runs here; all gating (`landmarks_trackable`, `roi_trackable`) and ROI
//!    derivation (`roi_from_landmarks`, `roi_from_palm`, association) stay here.
//! 2. **Content-norm** `[0, 1]²` — the camera-content rect with black-padding
//!    bars removed. Produced by `ContentRect::to_content_norm` immediately
//!    before the Leap-mm mapping. Without this step a 1280×720 camera's hands
//!    reach only 56 % of the Leap Y range (`y ∈ [0.219, 0.781]` of the square)
//!    and vertical motion is compressed 1.78×.
//! 3. **Leap mm** — the output convention expected by all downstream consumers:
//!    `x ∈ [−200, +200]`, `y ∈ [40, 350]` (height above device).
//! 4. **World metric** — the landmark model's world output: metres,
//!    wrist/hand-centred, camera-axis-aligned (x right, y down, z toward/away
//!    from the camera — image-space axes, but orthographic and metric).
//!    Gesture signals (`grab_strength`, `pinch_strength`, the palm normal)
//!    derive from these rather than from the perspective-projected image
//!    landmarks, so a hand tilted toward the camera (foreshortened in image
//!    space) cannot read as partially grabbed. Pipeline-internal: world
//!    coordinates never reach the public [`Hand`] and are never mapped to
//!    xy positions — though the metric wrist→middle-MCP segment, paired with
//!    its square-norm image projection, feeds the size-estimated depth that
//!    becomes palm z ([`super::coords::estimate_depth`]).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bevy::math::Vec3;
use image::{imageops::FilterType, RgbImage};
use smallvec::SmallVec;

use super::anchors::{generate_palm_anchors, Anchor, PalmAnchorOptions};
use super::capture::Frame;
use super::coords::{estimate_depth, image_norm_to_leap_mm, DEFAULT_DEPTH_CALIBRATION_K};
use super::inference::{HandInference, InferenceError, Tensor};
use super::landmark::{project_landmarks, roi_from_landmarks, roi_from_palm, RoiRect};
use super::palm::{
    decode_palm_detections_into, weighted_nms_into, PalmDecodeOptions, PalmDetection,
    PalmNmsScratch,
};
use super::signals::{
    grab_strength, palm_center, palm_normal, palm_velocity, pinch_strength, HandTracker,
};
use crate::input::hand::{Chirality, Hand, LANDMARK_COUNT};
use crate::input::state::MAX_HANDS;

const PALM_SIZE: u32 = 192;
const LM_SIZE: u32 = 224;

/// Scale factor applied to `max(size_a, size_b)` to compute the association gate.
///
/// **Why scale-relative, not fixed:** the palm-path ROI (scale 2.6, shift −0.5)
/// and the track-path ROI (scale 2.0, shift −0.1) for the *same* hand can have
/// centres up to ~0.3× the ROI size apart (dominated by the shift mismatch).  A
/// gate must exceed that to keep merging the same-hand pair reliably. At the same
/// time, two *distinct* hands cannot interpenetrate: their ROI centres sit at
/// least ~0.7–0.8× ROI size apart (ROIs are 2–2.6× the palm box), so
/// `0.5×max(size)` cleanly splits the "same hand" and "distinct hands" bands and
/// scales correctly whether hands are near or far. Using `max(size_a, size_b)` is
/// conservative (prefers merging over duplication), which is the safer failure
/// mode: a duplicated hand consumes the second `MAX_HANDS` slot and hides the
/// real second hand.
///
/// Safe tuning range is roughly `[0.35, 0.65]`: below that the gate dips under
/// the ~0.3× same-hand centre offset and risks splitting one hand into two
/// identities; above it the gate approaches the ~0.7× distinct-hand separation
/// floor and risks merging two real hands.
const ASSOCIATION_GATE_FACTOR: f32 = 0.5;

/// Absolute floor for the scale-relative gate (normalized square units).
///
/// For tiny or distant ROIs the geometric gate (`0.5×size`) can underestimate
/// detector jitter and split a single hand into two identities. The floor keeps
/// the gate wide enough to absorb that noise: `max(0.5×size, 0.08)`.
///
/// 0.08 is a design assumption, not a measured bound: it gives a few-× headroom
/// over the centre jitter expected of the palm detector on small/far ROIs
/// (anchor quantization plus decode noise, a fraction of the ROI size). If a
/// hardware session shows small-ROI jitter exceeding it, raise the floor rather
/// than the factor.
const ASSOCIATION_GATE_FLOOR: f32 = 0.08;

/// Smallest landmark-derived ROI that is still plausible as a track.
///
/// When a hand leaves the camera, the landmark model can report high presence
/// on an edge/empty crop with all landmarks collapsed together. The resulting
/// ROI centre may still be in-frame, so size is the signal that the track is no
/// longer usable. Below this, drop the hand and let palm detection reacquire.
const MIN_TRACK_ROI_SIZE: f32 = 0.05;

/// Minimum landmark bounding-box extent (in both axes) that still looks like a hand.
///
/// Checked against `min(bbox_width, bbox_height)` so a line-collapsed set (wide
/// in x, near-zero in y, or vice versa) is caught even when the mean of the two
/// axes looks plausible. A set collapsed to a line in one axis triggers a false
/// fist in Line's grab model (grab divides by hand scale); rejecting it before
/// deriving grab prevents a phantom attractor.
///
/// TODO: re-pick this threshold after a sustained two-hand hardware session
/// (candidate 0.03) — under the min metric, 0.04 may drop a legitimately
/// edge-on hand whose thin axis dips below it. The P6 change is the metric
/// (min instead of mean), not the value; threshold changes are
/// hardware-validated.
const MIN_TRACK_LANDMARK_SPREAD: f32 = 0.04;

/// Normalized margin inside the camera content that landmarks must stay within.
///
/// Once landmarks touch the square-padded frame edge, the ROI warp is already
/// sampling clamped pixels and the hand geometry is no longer trustworthy.
/// Dropping at a small inset is preferable to letting border-pinned points
/// produce a false fist/grab for one more frame.
const TRACK_LANDMARK_EDGE_MARGIN: f32 = 0.015;

/// Tunables for the pipeline.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Mirror x (webcam-as-mirror).
    pub mirror: bool,
    /// Minimum palm-detection score to accept.
    pub palm_score_threshold: f32,
    /// Minimum hand-presence probability to keep a hand. Compared against the
    /// landmark model's presence head, a real probability in `[0, 1]` (the
    /// sigmoid is baked into the model graph). The `0.5` default matches the
    /// `MediaPipe` web demo's `minTrackingConfidence`.
    pub presence_threshold: f32,
    /// Rest deadzone subtracted from the geometric grab so a *relaxed-open* hand
    /// reads exactly `0`. See [`apply_grab_deadzone`]. Live-tunable from the dev
    /// panel (`HandTrackingSettings::grab_rest_deadzone`); on the worker pipeline
    /// it is refreshed each frame from the provider's shared
    /// [`MediaPipeLiveTuning`].
    pub grab_rest_deadzone: f32,
    /// Calibration gain `k` for the size-estimated hand depth (the camera
    /// focal length in square-side units — see
    /// [`super::coords::estimate_depth`]). `<= 0` disables the estimator and
    /// pins depth to [`super::coords::MEDIAPIPE_DEPTH_PROXY_MM`] (the live-set rollback knob).
    /// Live-tunable from the dev panel
    /// (`HandTrackingSettings::depth_calibration_k`); refreshed each frame from
    /// the shared [`MediaPipeLiveTuning`].
    pub depth_calibration_k: f32,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            mirror: true,
            palm_score_threshold: 0.5,
            presence_threshold: 0.5,
            // 0.05: the world-landmark grab's relaxed-hand rest floor is far
            // lower than the image-space grab's was; the old 0.2 (calibrated
            // pre-world-landmarks) mostly just blunted mid-curl response.
            // Must stay in sync with `HandTrackingSettings::grab_rest_deadzone`.
            grab_rest_deadzone: 0.05,
            depth_calibration_k: DEFAULT_DEPTH_CALIBRATION_K,
        }
    }
}

/// Live (lock-free) pipeline tunables shared between the Bevy main thread
/// (dev-panel sliders, via the provider) and the worker thread's [`Pipeline`].
///
/// Each value is an `f32` stored as its bit pattern in an `AtomicU32`
/// (`to_bits`/`from_bits`) — no `Mutex`, no allocation, safe to write every
/// frame from a tuning system and read every frame from the worker loop.
/// The pipeline refreshes its [`PipelineConfig`] copies at the top of each
/// [`Pipeline::process`]. All accesses use `Ordering::Relaxed`: the fields are
/// independent scalars with no cross-field happens-before requirement, and a
/// one-frame-stale read is harmless (the next frame picks the value up).
///
/// Besides the pipeline tunables, the cell also carries the **idle-throttle
/// flag** read by the worker *loop* (not the pipeline): while set, the worker
/// caps inference at [`super::worker::IDLE_INFERENCE_HZ`] instead of the
/// configured full rate. It rides in this cell because the cell is exactly the
/// existing lock-free app→worker channel.
#[derive(Debug)]
pub struct MediaPipeLiveTuning {
    /// [`PipelineConfig::grab_rest_deadzone`] as `f32` bits.
    grab_deadzone: AtomicU32,
    /// [`PipelineConfig::depth_calibration_k`] as `f32` bits.
    depth_k: AtomicU32,
    /// Whether the app is in Idle/Screensaver — worker drops to the idle
    /// inference rate. Starts `false` (full rate): a freshly built provider is
    /// un-throttled until the per-frame mirror system stores the current
    /// activity state (at most one frame later).
    idle_throttle: AtomicBool,
}

impl MediaPipeLiveTuning {
    /// Build a tuning cell seeded with the given values. The idle-throttle
    /// flag starts cleared (full inference rate).
    #[must_use]
    pub fn new(grab_deadzone: f32, depth_k: f32) -> Self {
        Self {
            grab_deadzone: AtomicU32::new(grab_deadzone.to_bits()),
            depth_k: AtomicU32::new(depth_k.to_bits()),
            idle_throttle: AtomicBool::new(false),
        }
    }

    /// Live-set the grab rest-deadzone.
    pub fn set_grab_deadzone(&self, deadzone: f32) {
        self.grab_deadzone
            .store(deadzone.to_bits(), Ordering::Relaxed);
    }

    /// The current grab rest-deadzone.
    #[must_use]
    pub fn grab_deadzone(&self) -> f32 {
        f32::from_bits(self.grab_deadzone.load(Ordering::Relaxed))
    }

    /// Live-set the depth calibration gain `k` (`<= 0` disables the estimator).
    pub fn set_depth_k(&self, k: f32) {
        self.depth_k.store(k.to_bits(), Ordering::Relaxed);
    }

    /// The current depth calibration gain `k`.
    #[must_use]
    pub fn depth_k(&self) -> f32 {
        f32::from_bits(self.depth_k.load(Ordering::Relaxed))
    }

    /// Live-set the idle-throttle flag (`true` = Idle/Screensaver, cap
    /// inference at the idle rate). A Relaxed store, cheap enough to call
    /// unconditionally every frame from the activity-mirror system.
    pub fn set_idle_throttle(&self, idle: bool) {
        self.idle_throttle.store(idle, Ordering::Relaxed);
    }

    /// Whether the idle inference throttle is currently requested.
    #[must_use]
    pub fn idle_throttle(&self) -> bool {
        self.idle_throttle.load(Ordering::Relaxed)
    }
}

/// Why the palm detector ran or skipped for the latest processed frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PalmRunReason {
    /// The pipeline had no carried track and needed initial acquisition.
    #[default]
    ColdStart,
    /// Fewer than [`MAX_HANDS`] tracks were active, so detection searched for a
    /// second/new hand.
    BelowMaxHands,
    /// The pipeline already had [`MAX_HANDS`] active tracks and skipped palm.
    SkippedAtCapacity,
    /// The frame was invalid; no model stage ran.
    InvalidFrame,
}

impl PalmRunReason {
    /// Static label for diagnostics.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ColdStart => "cold_start",
            Self::BelowMaxHands => "below_max_hands",
            Self::SkippedAtCapacity => "skipped_at_capacity",
            Self::InvalidFrame => "invalid_frame",
        }
    }
}

/// Timing and tracking metrics for the latest processed frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PipelineDiagnostics {
    /// Total process time for one frame.
    pub total: Duration,
    /// Time spent on square padding / preprocessing before model stages.
    pub preprocess: Duration,
    /// Time spent in palm acquisition when it ran.
    pub palm: Duration,
    /// Time spent running landmark-path work across all ROIs.
    pub landmark: Duration,
    /// Why palm did or did not run.
    pub palm_reason: PalmRunReason,
    /// Number of carried tracks at frame start.
    pub tracks_before: u64,
    /// Number of tracks kept for the next frame.
    pub tracks_after: u64,
    /// Number of hands emitted for this frame.
    pub hands: u64,
    /// Cumulative track churn (ids created + aged out) since pipeline start.
    /// Flat for a stable hand; climbs under acquire/lose flicker. See
    /// [`super::signals::HandTracker::churn`].
    pub track_churn: u64,
    /// Physical size-estimated camera distance (mm, rounded) of the first
    /// emitted (focal) hand this frame — the "Est. distance (mm)" dev-panel
    /// metric for calibrating `depth_calibration_k` against a tape measure.
    /// This is the raw similar-triangles estimate (`distance_m × 1000`,
    /// unsmoothed, **before** the Leap-z remap), so at a tape-measured 0.5 m it
    /// reads ≈ 500 once `k` is calibrated. It is NOT the Leap z the attractor
    /// sees (that value is clamped to `[40, 350]` and lives in
    /// `Hand::palm_position.z`). `0` when there is no hand this frame or the
    /// estimator is disabled (`k <= 0` — no physical estimate under the pin).
    pub est_distance_mm: u64,
    /// Raw geometric grab of the focal hand, **pre**-deadzone, in permille
    /// (`round(grab_strength(world) × 1000)`, so `[0, 1000]`) — the dev
    /// panel's "Grab raw (‰)". Watching this against [`Self::grab_permille`]
    /// shows the deadzone subtracting and measures the true rest floor (the
    /// raw value a relaxed-open hand actually sits at). `0` when no hand.
    pub grab_raw_permille: u64,
    /// Deadzoned grab of the focal hand in permille — exactly what ships on
    /// `Hand::grab_strength` (the dev panel's "Grab (‰)"). `0` when no hand.
    pub grab_permille: u64,
}

/// Remap a raw geometric grab so a *relaxed-open* hand reads exactly `0`.
///
/// [`grab_strength`] is calibrated to ideal open-hand geometry (fingers fully
/// extended one hand-scale out → `0`); a real relaxed hand sits slightly curled
/// and landmark noise jitters the fingertips, so the raw signal carries a small
/// positive floor at rest. That floor matters because Line's decay gate is
/// `grab > 0`: any positive open-hand floor keeps the attractor faintly — and,
/// via the slow attack EMA, increasingly — alive even with the hand wide open.
/// Subtracting a rest deadzone and rescaling pins `grab <= deadzone → 0` while a full fist still reaches `1`.
///
/// `deadzone` is clamped to `[0, 0.95]`; `0` is a pass-through.
fn apply_grab_deadzone(grab: f32, deadzone: f32) -> f32 {
    let dz = deadzone.clamp(0.0, 0.95);
    if dz <= 0.0 {
        return grab;
    }
    ((grab - dz) / (1.0 - dz)).clamp(0.0, 1.0)
}

/// The two-stage hand pipeline. Holds the model sessions, anchors, tracker, and
/// reused scratch buffers.
pub struct Pipeline {
    palm: Box<dyn HandInference>,
    landmark: Box<dyn HandInference>,
    anchors: Vec<Anchor>,
    decode: PalmDecodeOptions,
    tracker: HandTracker,
    config: PipelineConfig,
    /// Per-hand landmark-derived ROIs carried to the next frame
    /// (`MediaPipe`'s detect-then-track). While this holds [`MAX_HANDS`] tracks the
    /// next [`Self::process`] skips palm entirely (the dominant per-frame cost) and
    /// tracks landmark-only; palm re-runs only when fewer than [`MAX_HANDS`] are
    /// tracked (count-gated re-detection), so a healthy pair of hands is never
    /// re-seeded. A track is dropped when its hand is lost (landmark presence below
    /// threshold), when it leaves the frame ([`roi_trackable`]), or when the
    /// frame is unusable.
    tracked: SmallVec<[RoiRect; MAX_HANDS]>,
    /// Optional live source for the tunable config values (grab rest-deadzone,
    /// depth calibration `k`), shared lock-free with the provider so a tuning
    /// UI can re-tune them while the worker thread runs. Refreshed at the top
    /// of each [`Self::process`]; `None` (tests, no UI) leaves the config
    /// values in force.
    live_tuning: Option<Arc<MediaPipeLiveTuning>>,
    /// Diagnostics for the most recent processed frame.
    last_diagnostics: PipelineDiagnostics,
    /// Reused square-pad scratch image (taken out via `mem::take` while
    /// processing so the per-frame methods can borrow it without aliasing
    /// `&mut self`). Avoids a per-frame `RgbImage` allocation.
    square_buf: RgbImage,
    /// Reused ROI-warp crop (`LM_SIZE`²), refilled per landmark stage.
    warp_buf: RgbImage,
    /// Reused palm-stage resize target (`PALM_SIZE`²), refilled each
    /// acquisition by [`resize_into`].
    palm_resize_buf: RgbImage,
    /// Reused palm-stage input tensor (`192²×3` f32), refilled each acquisition.
    palm_input: Tensor,
    /// Reused landmark-stage input tensor (`224²×3` f32), refilled per ROI.
    landmark_input: Tensor,
    /// Reused decoded-palm-detections buffer, cleared+refilled each acquisition
    /// ([`decode_palm_detections_into`], then NMS'd in place).
    palm_dets: Vec<PalmDetection>,
    /// Reused weighted-NMS scratch (mask, cluster, kept buffers).
    palm_nms: PalmNmsScratch,
    /// Reused palm-stage raw output tensors ([`HandInference::run`] refills them
    /// in place), so decoding never allocates the model outputs per acquisition.
    palm_outputs: Vec<Tensor>,
    /// Reused landmark-stage raw output tensors, refilled in place per ROI.
    landmark_outputs: Vec<Tensor>,
}

impl Pipeline {
    /// Build a pipeline from the two model stages.
    #[must_use]
    pub fn new(
        palm: Box<dyn HandInference>,
        landmark: Box<dyn HandInference>,
        config: PipelineConfig,
    ) -> Self {
        Self {
            palm,
            landmark,
            anchors: generate_palm_anchors(&PalmAnchorOptions::mediapipe_palm_192()),
            decode: PalmDecodeOptions::mediapipe_palm_192(),
            tracker: HandTracker::default(),
            config,
            tracked: SmallVec::new(),
            live_tuning: None,
            last_diagnostics: PipelineDiagnostics::default(),
            // Scratch buffers, pre-sized so steady-state processing allocates
            // nothing: the input tensors fill to capacity on the first frame and
            // are cleared+refilled thereafter; the images (re)allocate only if the
            // camera frame size changes.
            square_buf: RgbImage::default(),
            warp_buf: RgbImage::new(LM_SIZE, LM_SIZE),
            palm_resize_buf: RgbImage::new(PALM_SIZE, PALM_SIZE),
            palm_input: Tensor {
                data: Vec::with_capacity(idx(PALM_SIZE) * idx(PALM_SIZE) * 3),
                shape: vec![1, idx(PALM_SIZE), idx(PALM_SIZE), 3],
            },
            landmark_input: Tensor {
                data: Vec::with_capacity(idx(LM_SIZE) * idx(LM_SIZE) * 3),
                shape: vec![1, idx(LM_SIZE), idx(LM_SIZE), 3],
            },
            palm_dets: Vec::new(),
            palm_nms: PalmNmsScratch::default(),
            palm_outputs: Vec::new(),
            landmark_outputs: Vec::new(),
        }
    }

    /// Attach the shared, lock-free tuning cell so a tuning UI on the main
    /// thread can re-tune the grab rest-deadzone and depth calibration `k`
    /// while this pipeline runs on the worker thread.
    pub fn set_live_tuning_source(&mut self, source: Arc<MediaPipeLiveTuning>) {
        self.live_tuning = Some(source);
    }

    /// Diagnostics for the most recent processed frame.
    #[must_use]
    pub fn diagnostics(&self) -> PipelineDiagnostics {
        self.last_diagnostics
    }

    /// The execution provider each stage is running on *right now*
    /// (palm, landmark), or `None` per stage for a backend with no EP concept (the
    /// test mocks) — see [`HandInference::backend_label`].
    ///
    /// Read live rather than latched at load because a stage can **demote itself to
    /// the CPU EP mid-session** when its accelerator fails inference persistently.
    /// The worker samples this each processed frame and ships it to the provider, so
    /// a degradation that happens at hour three of a soak still reaches the settings
    /// panel's amber backend row. Two `Copy` field reads: no lock, no allocation.
    #[must_use]
    pub fn backend_labels(&self) -> (Option<&'static str>, Option<&'static str>) {
        (self.palm.backend_label(), self.landmark.backend_label())
    }

    /// Run one frame through both stages and return the tracked hands.
    ///
    /// `dt` is the time since the previous processed frame (for palm velocity).
    ///
    /// # Errors
    /// Returns [`InferenceError`] if either model fails to run.
    pub fn process(
        &mut self,
        frame: &Frame,
        dt: Duration,
    ) -> Result<SmallVec<[Hand; MAX_HANDS]>, InferenceError> {
        let frame_start = Instant::now();
        let mut diagnostics = PipelineDiagnostics {
            tracks_before: u64::try_from(self.tracked.len()).unwrap_or(u64::MAX),
            ..PipelineDiagnostics::default()
        };
        // Pick up any live re-tune from the provider/UI before this frame
        // derives grab and depth.
        if let Some(tuning) = &self.live_tuning {
            self.config.grab_rest_deadzone = tuning.grab_deadzone();
            self.config.depth_calibration_k = tuning.depth_k();
        }
        let mut hands: SmallVec<[Hand; MAX_HANDS]> = SmallVec::new();
        if !frame.is_consistent() || frame.width == 0 || frame.height == 0 {
            self.tracker.end_frame();
            self.tracked.clear(); // a bad frame breaks tracking → re-acquire next
            diagnostics.palm_reason = PalmRunReason::InvalidFrame;
            diagnostics.total = frame_start.elapsed();
            self.last_diagnostics = diagnostics;
            return Ok(hands);
        }

        // The camera content occupies only part of the square-padded image (black
        // bars when the frame isn't square). Compute the content rect now so the
        // off-screen drop below treats a hand that has drifted into a bar — centre
        // still in [0, 1] of the square — as gone, not as a lingering phantom.
        let content = ContentRect::for_frame(frame.width, frame.height);

        // Square-pad to the larger side so detection coords are aspect-correct.
        // Take the reused buffer out of `self` so the per-frame methods below can
        // borrow it alongside `&mut self` without aliasing; restored before return.
        let stage_start = Instant::now();
        let mut square = std::mem::take(&mut self.square_buf);
        square_pad_into(frame, &mut square);
        diagnostics.preprocess = stage_start.elapsed();

        // Count-gated re-detection (MediaPipe's detect-then-track): run palm
        // detection ONLY while fewer than MAX_HANDS are tracked — including cold
        // start (empty). Once MAX_HANDS are tracked, palm never runs; the hands
        // are tracked landmark-only and are never re-seeded by a fresh detection
        // (the old fixed-interval re-detect re-seeded healthy tracks, which
        // duplicated/swapped hands). A track drops via presence or leaving the
        // frame, lowering the count, which re-enables detection next frame.
        // [`associate`] merges fresh palm ROIs with the tracked ROIs: tracked win
        // (kept verbatim), only a non-overlapping detection is added as a new hand.
        let to_run: SmallVec<[RoiRect; MAX_HANDS]> = if self.tracked.len() < MAX_HANDS {
            diagnostics.palm_reason = if self.tracked.is_empty() {
                PalmRunReason::ColdStart
            } else {
                PalmRunReason::BelowMaxHands
            };
            let stage_start = Instant::now();
            let palm_rois = self.acquire_rois(&square)?;
            diagnostics.palm = stage_start.elapsed();
            let tracked = std::mem::take(&mut self.tracked);
            associate(tracked, &palm_rois)
        } else {
            diagnostics.palm_reason = PalmRunReason::SkippedAtCapacity;
            std::mem::take(&mut self.tracked)
        };

        // Run the landmark stage on each ROI; keep the hand and carry its
        // next-frame ROI (derived from this frame's landmarks) when presence holds
        // AND the hand is still on screen. A dropped track lowers the count, so the
        // next frame re-detects to re-acquire (or pick up a new hand).
        let mut next: SmallVec<[RoiRect; MAX_HANDS]> = SmallVec::new();
        for roi in to_run {
            let stage_start = Instant::now();
            if let Some(tracked) = self.landmark_for(&square, roi, content, dt)? {
                // `landmark_for` already refreshed this hand's signals-level
                // track (`HandTracker::assign`), so a hand dropped by the ROI
                // check below keeps its track id for one extra frame — id
                // continuity on reacquire is deliberate; `track_churn` counts
                // the drop one frame late.
                if roi_trackable(&tracked.next_roi, content) {
                    if hands.is_empty() {
                        // First (focal) hand: surface its PHYSICAL estimated
                        // distance for the dev panel's "Est. distance (mm)"
                        // calibration metric (0 when the estimator is off).
                        // The value is finite and non-negative; floor(x + 0.5)
                        // rounds, and the f32→u32 cast inside `floor_u32`
                        // saturates on a degenerate (collapsed-segment) huge
                        // estimate rather than wrapping.
                        diagnostics.est_distance_mm =
                            u64::from(floor_u32(tracked.est_distance_mm + 0.5));
                        // Raw vs deadzoned grab, in permille (both in [0, 1],
                        // so ×1000 + round stays well inside u32). Surfacing
                        // the pair lets the operator watch the deadzone
                        // subtract and measure the true rest floor.
                        diagnostics.grab_raw_permille =
                            u64::from(floor_u32(tracked.grab_raw.mul_add(1000.0, 0.5)));
                        diagnostics.grab_permille =
                            u64::from(floor_u32(tracked.hand.grab_strength.mul_add(1000.0, 0.5)));
                    }
                    hands.push(tracked.hand);
                    next.push(tracked.next_roi);
                }
            }
            diagnostics.landmark = diagnostics.landmark.saturating_add(stage_start.elapsed());
        }
        self.tracked = next;
        self.square_buf = square; // return the reused buffer to its home
        self.tracker.end_frame();
        diagnostics.tracks_after = u64::try_from(self.tracked.len()).unwrap_or(u64::MAX);
        diagnostics.hands = u64::try_from(hands.len()).unwrap_or(u64::MAX);
        diagnostics.track_churn = self.tracker.churn();
        diagnostics.total = frame_start.elapsed();
        self.last_diagnostics = diagnostics;
        Ok(hands)
    }

    /// Acquisition path: run palm detection on the square frame and return up to
    /// [`MAX_HANDS`] candidate ROIs (highest-scoring first). Only runs when not
    /// already tracking.
    fn acquire_rois(
        &mut self,
        square: &RgbImage,
    ) -> Result<SmallVec<[RoiRect; MAX_HANDS]>, InferenceError> {
        // Bilinear-resize into the reused 192² buffer (see [`resize_into`] for
        // the equivalence story vs the image crate's Triangle resize), then
        // refill the reused input tensor — no per-acquisition allocation.
        resize_into(square, PALM_SIZE, PALM_SIZE, &mut self.palm_resize_buf);
        fill_nhwc_unit(&self.palm_resize_buf, &mut self.palm_input);
        self.palm.run(&self.palm_input, &mut self.palm_outputs)?;
        let (boxes, scores) = pick_palm_outputs(&self.palm_outputs)?;
        // Decode + NMS through the reused scratch buffers: the `_into` forms
        // clear-and-refill, so capacities persist across frames and the
        // steady-state acquisition path allocates nothing (worker-loop rule).
        decode_palm_detections_into(
            boxes,
            scores,
            &self.anchors,
            &self.decode,
            self.config.palm_score_threshold,
            &mut self.palm_dets,
        );
        weighted_nms_into(&mut self.palm_dets, 0.3, &mut self.palm_nms);
        // No re-sort: weighted_nms_into documents (and palm.rs's
        // `nms_output_is_sorted_by_descending_score` pins) that its output is
        // already non-increasing by score — each blended cluster carries its
        // seed's maximal score and seeds are visited in descending order — so
        // truncating keeps the top MAX_HANDS. (The stable sort_by that stood
        // here also allocated an aux buffer above ~20 elements.)
        self.palm_dets.truncate(MAX_HANDS);
        // NOT an allocation: SmallVec<[RoiRect; MAX_HANDS]> keeps up to
        // MAX_HANDS (= 2) elements inline on the stack, and `dets` was just
        // truncated to that, so this collect never spills to the heap.
        Ok(self.palm_dets.iter().map(roi_from_palm).collect())
    }

    /// Run the landmark stage for one ROI. Returns the tracked hand, the ROI
    /// to use for it next frame (derived from its landmarks), and the per-hand
    /// diagnostic values, or `None` if the model's presence score is below
    /// threshold (no hand in this ROI).
    fn landmark_for(
        &mut self,
        square: &RgbImage,
        roi: RoiRect,
        content: ContentRect,
        dt: Duration,
    ) -> Result<Option<TrackedHand>, InferenceError> {
        // Warp the ROI into the reused crop buffer, then into the reused input
        // tensor — no per-ROI image/tensor allocation.
        warp_roi_into(square, &roi, LM_SIZE, &mut self.warp_buf);
        fill_nhwc_unit(&self.warp_buf, &mut self.landmark_input);
        self.landmark
            .run(&self.landmark_input, &mut self.landmark_outputs)?;
        let LandmarkOutputs {
            image: raw_lms,
            presence,
            handedness: handed,
            world: raw_world,
        } = pick_landmark_outputs(&self.landmark_outputs)?;
        if presence < self.config.presence_threshold {
            return Ok(None);
        }

        let img_landmarks = project_landmarks(raw_lms, &roi);
        if !landmarks_trackable(&img_landmarks, content) {
            return Ok(None);
        }

        // Metric world landmarks (space 4 of the module doc) for the gesture
        // signals: pose-invariant geometry a perspective-projected hand can't
        // give (a hand tilted toward the camera foreshortens its image-space
        // tip-to-palm distances and would previously read as partially grabbed).
        let world = decode_world_landmarks(raw_world);
        // Orientation frame for the palm normal: world axes mapped into the
        // Leap convention (see [`world_to_leap_orientation`]). Stack array —
        // the per-frame path stays allocation-free.
        let mut world_leap = [Vec3::ZERO; LANDMARK_COUNT];
        for (dst, src) in world_leap.iter_mut().zip(world.iter()) {
            *dst = world_to_leap_orientation(*src, self.config.mirror);
        }

        // Map every landmark through content-norm then into Leap-device-mm.
        //
        // `content.to_content_norm` strips the square-padding bars first, so
        // landmark y ∈ [y0, y1] of the square maps to [0, 1] of the content,
        // giving `image_norm_to_leap_mm` a full [0, 1] input in both axes.
        // Without this step, a 1280×720 camera (y0=0.219, y1=0.781) exposes
        // only 56% of the Leap Y range and compresses vertical motion 1.78×.
        //
        // Gating (`landmarks_trackable`) and ROI derivation
        // (`roi_from_landmarks`) stay in square-norm; gesture signals derive
        // from the metric world landmarks decoded above; only the Leap-mm
        // mapping step changes space here.
        let mut landmarks = [Vec3::ZERO; LANDMARK_COUNT];
        for (dst, src) in landmarks.iter_mut().zip(img_landmarks.iter()) {
            // square-norm → content-norm → Leap mm
            *dst = image_norm_to_leap_mm(content.to_content_norm(*src), self.config.mirror);
        }
        let observed_chirality = if handed >= 0.5 {
            Chirality::Right
        } else {
            Chirality::Left
        };
        // palm_center is computed in square-norm; unproject before mm mapping
        // so palm position and per-landmark positions are consistent.
        let mut palm_pos = image_norm_to_leap_mm(
            content.to_content_norm(palm_center(&img_landmarks)),
            self.config.mirror,
        );
        // Live size-estimated depth: a single webcam has no direct hand-Z (the
        // landmark model's z is a near-zero relative value), but apparent size
        // gives one — the wrist→middle-MCP segment is metric in the WORLD
        // landmarks and measured in square-norm xy in the IMAGE landmarks
        // (square-norm because it is isotropic; content-norm rescales the axes
        // unevenly), so similar triangles yield a camera distance, remapped
        // into the Leap z range Line's `5^((−z+350)/160)` power model expects.
        // Closer ⇒ stronger, like Leap. `k <= 0` disables the estimator and
        // restores the fixed [`MEDIAPIPE_DEPTH_PROXY_MM`] pin (the live-set
        // rollback knob). The raw estimate is noisy, so the tracker EMA-smooths
        // it per track inside `assign`; the smoothed value lands in
        // `palm_pos.z` below, AFTER identity assignment.
        let depth = estimate_depth(&world, &img_landmarks, self.config.depth_calibration_k);
        let raw_depth_mm = depth.leap_z_mm;
        // Position-based id with a hysteresis-held chirality, so a spurious
        // per-frame handedness flip neither churns the id nor flickers
        // downstream. The gate inside `assign` compares xy only (palm_pos.z is
        // ignored), so raw depth noise can never break identity association.
        let assigned = self.tracker.assign(
            observed_chirality,
            palm_pos,
            raw_depth_mm,
            depth.distance_mm,
            dt,
        );
        // Emit the track's EMA-smoothed depth as palm z, replacing the
        // meaningless image-z that rode through the mm mapping.
        palm_pos.z = assigned.depth_mm;
        // Finite-difference the palm against its previous-frame position (held by
        // the tracker) over the inter-frame `dt`. A fresh track has no history, so
        // velocity starts at zero on first sighting; `dt == 0` is also zero.
        // palm_velocity.z is now the smoothed-depth derivative (it was always 0
        // under the fixed depth pin); no current consumer reads velocity z.
        let velocity = palm_velocity(assigned.prev_pos.unwrap_or(palm_pos), palm_pos, dt);

        // Next frame tracks from these landmarks, skipping palm detection.
        let next_roi = roi_from_landmarks(&img_landmarks);

        // Raw geometric grab, computed once: deadzoned onto the emitted hand,
        // surfaced raw in the diagnostics so the dev panel can show both.
        let grab_raw = grab_strength(&world);

        Ok(Some(TrackedHand {
            hand: Hand {
                id: assigned.id,
                chirality: assigned.chirality,
                palm_position: palm_pos,
                // Orientation from the Leap-mapped world landmarks; the held
                // (hysteresis-smoothed) chirality picks the cross-product sign.
                // No sketch currently consumes palm_normal from this provider
                // (input::systems stubs it to Vec3::Y when rebuilding hands);
                // the first real consumer should hardware-validate the sign
                // convention first (see [`world_to_leap_orientation`]).
                palm_normal: palm_normal(&world_leap, assigned.chirality),
                palm_velocity: velocity,
                // Pinch/grab from the metric world landmarks: pose-invariant,
                // so a tilted-open hand no longer reads as pinched/grabbed.
                pinch_strength: pinch_strength(&world),
                // Rest-deadzone the grab so a relaxed-open hand reads exactly 0
                // (otherwise its small positive floor keeps Line's attractor on).
                grab_strength: apply_grab_deadzone(grab_raw, self.config.grab_rest_deadzone),
                landmarks,
                // The track's EMA-smoothed physical distance (unclamped, so it
                // keeps tracking past the 1 m Leap-z far rail); 0.0 when the
                // estimator is off (k <= 0) — consumers fall back to Leap-z
                // behaviour for unknown. Smoothed with the same τ as palm z so
                // the audio distance band doesn't flutter with landmark noise.
                camera_distance_mm: assigned.distance_mm,
            },
            next_roi,
            est_distance_mm: depth.distance_mm,
            grab_raw,
        }))
    }
}

/// One ROI's landmark-stage outcome: the hand to emit, the ROI to track it
/// from next frame, and the per-hand values [`Pipeline::process`] surfaces in
/// [`PipelineDiagnostics`] for the focal hand. Stack-only (no heap fields
/// beyond [`Hand`]'s fixed arrays) — fine on the per-frame path.
struct TrackedHand {
    /// The hand as emitted downstream (post-deadzone grab, smoothed depth z).
    hand: Hand,
    /// Landmark-derived ROI to track this hand from next frame.
    next_roi: RoiRect,
    /// Physical size-estimated camera distance (mm, raw/unsmoothed); `0.0`
    /// when the estimator is disabled (`k <= 0`). See
    /// [`super::coords::DepthEstimate::distance_mm`].
    est_distance_mm: f32,
    /// Raw geometric grab (`grab_strength` on the world landmarks), **before**
    /// the rest deadzone; the deadzoned value lives on
    /// [`Self::hand`]`.grab_strength`.
    grab_raw: f32,
}

// --- detect-then-track association ---------------------------------------

/// Merge the previous frame's tracked ROIs with fresh palm detections, the way
/// `MediaPipe`'s `AssociationNormRectCalculator` does: **tracked ROIs win.**
///
/// Each kept ROI is the smooth landmark-derived track, never replaced by a jumpy
/// fresh palm ROI. A fresh detection is added (as a new hand) only if its centre
/// is more than [`association_gate`] from every already-kept ROI, so an existing
/// hand is never duplicated or snapped to a new identity. The result is capped at
/// [`MAX_HANDS`].
fn associate(
    tracked: SmallVec<[RoiRect; MAX_HANDS]>,
    palm_rois: &[RoiRect],
) -> SmallVec<[RoiRect; MAX_HANDS]> {
    let mut out = tracked; // tracked have priority — kept verbatim
    for p in palm_rois {
        if out.len() >= MAX_HANDS {
            break;
        }
        if out
            .iter()
            .all(|kept| roi_center_dist(kept, p) > association_gate(kept, p))
        {
            out.push(*p);
        }
    }
    out
}

/// Scale-relative merge gate for [`associate`].
///
/// Returns `max(`[`ASSOCIATION_GATE_FACTOR`]` × max(a.size, b.size),`
/// [`ASSOCIATION_GATE_FLOOR`]`)`.
///
/// Using `max(a.size, b.size)` is conservative (wider gate → prefers merging
/// over duplication). See [`ASSOCIATION_GATE_FACTOR`] for the geometric rationale.
fn association_gate(a: &RoiRect, b: &RoiRect) -> f32 {
    (ASSOCIATION_GATE_FACTOR * a.size.max(b.size)).max(ASSOCIATION_GATE_FLOOR)
}

/// Centre-to-centre distance between two ROIs in normalized image units.
fn roi_center_dist(a: &RoiRect, b: &RoiRect) -> f32 {
    (a.cx - b.cx).hypot(a.cy - b.cy)
}

/// The camera content rectangle inside the square-padded image, in
/// square-normalized coordinates.
///
/// [`square_pad_into`] pads a non-square camera frame to its larger side with
/// black bars (top/bottom for the usual landscape webcam, left/right for a
/// portrait one). Those bars live *inside* `[0, 1]` of the padded square, so a
/// hand that drifts off the camera into a bar still has a centre within `[0, 1]`.
/// Tracking must treat the bars as off-screen, which a bare `[0, 1]` test does
/// not — hence this explicit content rect. For an already-square frame it is the
/// full `[0, 1]²`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ContentRect {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl ContentRect {
    /// Content rect for a `frame_w × frame_h` camera frame square-padded to its
    /// larger side, matching [`square_pad_into`]'s `ox`/`oy` centring.
    fn for_frame(frame_w: u32, frame_h: u32) -> Self {
        let side = frame_w.max(frame_h).max(1);
        let sidef = dim(side);
        let ox = dim((side - frame_w) / 2);
        let oy = dim((side - frame_h) / 2);
        Self {
            x0: ox / sidef,
            y0: oy / sidef,
            x1: (ox + dim(frame_w)) / sidef,
            y1: (oy + dim(frame_h)) / sidef,
        }
    }

    /// Map a square-normalized point into content-normalized coordinates.
    ///
    /// `x' = (x − x0) / (x1 − x0)`, `y'` analog, `z` passes through.
    ///
    /// Transforms a point expressed in the full `[0, 1]²` of the square-padded
    /// image into `[0, 1]²` of the camera content rect, stripping the black-bar
    /// padding that [`square_pad_into`] adds. Feeding content-normalized
    /// coordinates to `image_norm_to_leap_mm` makes the full Leap Y range
    /// reachable regardless of the camera's aspect ratio.
    ///
    /// # Invariant
    /// The rect is built from a non-zero frame (`for_frame` enforces
    /// `frame_w >= 1`, `frame_h >= 1`, `side >= 1`), so `x1 > x0` and
    /// `y1 > y0` always hold; division by their differences is safe.
    /// `debug_assert!` guards this in debug/test builds.
    fn to_content_norm(self, p: Vec3) -> Vec3 {
        // Width and height of the content window in square-normalized units.
        // For a 1280×720 frame: w = 1.0, h ≈ 0.5625 (720/1280).
        let w = self.x1 - self.x0;
        let h = self.y1 - self.y0;
        // Invariant: for_frame guarantees non-zero frame dims → w > 0, h > 0.
        debug_assert!(w > 0.0, "content rect has zero width: {self:?}");
        debug_assert!(h > 0.0, "content rect has zero height: {self:?}");
        // x' = (x − x0) / w  maps square-norm x into content [0, 1].
        // y' = (y − y0) / h  maps square-norm y into content [0, 1].
        // z passes through untouched: at this stage it is still the landmark
        // model's relative image-z. The pipeline overwrites *palm* z with the
        // smoothed size-estimated depth after track assignment (see
        // `landmark_for`); the 21 per-landmark z values keep the relative
        // image-z into `Hand::landmarks` (mixed z units by design — a future
        // consumer of landmark depth must convert deliberately).
        Vec3::new((p.x - self.x0) / w, (p.y - self.y0) / h, p.z)
    }

    /// Whether the normalized point `(cx, cy)` lies within the content rect.
    fn contains(self, cx: f32, cy: f32) -> bool {
        (self.x0..=self.x1).contains(&cx) && (self.y0..=self.y1).contains(&cy)
    }

    /// Whether `(cx, cy)` lies inside the content rect after insetting all edges.
    fn contains_inset(self, cx: f32, cy: f32, margin: f32) -> bool {
        let x0 = self.x0 + margin;
        let y0 = self.y0 + margin;
        let x1 = self.x1 - margin;
        let y1 = self.y1 - margin;
        x0 <= x1 && y0 <= y1 && (x0..=x1).contains(&cx) && (y0..=y1).contains(&cy)
    }
}

/// True if the ROI's centre (the hand's palm in square-normalized coordinates)
/// is within the camera content rect — i.e. the hand is still in the camera's
/// view, not drifted into a square-padding bar. A track whose centre has left
/// the content is an off-screen hand: drop it rather than let it linger as a
/// drifting phantom (the landmark model can keep reporting high presence on a
/// clamped/black-bar edge crop, and a stale phantom would hold a tracked slot
/// and stay the focal hand, so a returning hand is ignored). See [`ContentRect`].
fn roi_on_screen(roi: &RoiRect, content: ContentRect) -> bool {
    content.contains(roi.cx, roi.cy)
}

/// True if the landmark-derived ROI is worth carrying into the next frame.
fn roi_trackable(roi: &RoiRect, content: ContentRect) -> bool {
    roi_on_screen(roi, content) && roi.size.is_finite() && roi.size >= MIN_TRACK_ROI_SIZE
}

/// True when the projected landmark set is finite and spatially hand-like.
fn landmarks_trackable(landmarks: &[Vec3; LANDMARK_COUNT], content: ContentRect) -> bool {
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for lm in landmarks {
        if !lm.x.is_finite()
            || !lm.y.is_finite()
            || !content.contains_inset(lm.x, lm.y, TRACK_LANDMARK_EDGE_MARGIN)
        {
            return false;
        }
        min_x = min_x.min(lm.x);
        min_y = min_y.min(lm.y);
        max_x = max_x.max(lm.x);
        max_y = max_y.max(lm.y);
    }
    // Use min(width, height) rather than mean so a line-collapsed set (large in
    // one axis, near-zero in the other) is rejected even when the mean looks
    // plausible. A line of landmarks triggers a false fist in the grab model.
    (max_x - min_x).min(max_y - min_y) >= MIN_TRACK_LANDMARK_SPREAD
}

// --- world-landmark gesture geometry --------------------------------------

/// Decode the landmark model's `[1, 63]` WORLD output into per-landmark points.
///
/// Same 21 × `(x, y, z)` row-major layout as the image output, but the values
/// are already metric hand-space — metres, wrist/hand-centred, camera-axis
/// aligned (x right, y down, z toward/away from the camera) — **not**
/// crop-relative, so unlike [`project_landmarks`] there is no ROI unprojection.
/// Returns a stack array: no heap allocation on the per-frame path.
fn decode_world_landmarks(raw: &[f32]) -> [Vec3; LANDMARK_COUNT] {
    let mut out = [Vec3::ZERO; LANDMARK_COUNT];
    for (i, lm) in out.iter_mut().enumerate() {
        let base = i * 3;
        *lm = Vec3::new(
            raw.get(base).copied().unwrap_or(0.0),
            raw.get(base + 1).copied().unwrap_or(0.0),
            raw.get(base + 2).copied().unwrap_or(0.0),
        );
    }
    out
}

/// Map a metric world-landmark point into the Leap **orientation** convention
/// consumed by [`palm_normal`] (orientation only — world coords never feed the
/// positional path).
///
/// - `x` — negated when the webcam is mirrored: the positional path mirrors x
///   (`1 − x` in [`image_norm_to_leap_mm`]), which reverses cross-product
///   handedness, so the orientation frame must mirror with it or the normal
///   flips relative to the rendered hand.
/// - `y` — negated: image/world y points down, Leap y points up (height above
///   the device).
/// - `z` — passed through: both conventions keep z on the camera axis.
///
/// The resulting normal's *internal* consistency (perpendicularity, mirror
/// flip) is pinned by tests; whether its absolute sign matches the Leap
/// provider's out-of-the-palm convention still needs hardware validation.
fn world_to_leap_orientation(v: Vec3, mirror: bool) -> Vec3 {
    Vec3::new(if mirror { -v.x } else { v.x }, -v.y, v.z)
}

// --- numeric conversion helpers (kept tiny + justified) ------------------

/// `u32` → `usize` (image index); infallible on all supported targets.
fn idx(v: u32) -> usize {
    usize::try_from(v).unwrap_or(0)
}

/// `u32` → `f32` for image dimensions/indices (all ≤ 65535 for realistic
/// frames; clamps above, which never happens for camera resolutions).
fn dim(v: u32) -> f32 {
    f32::from(u16::try_from(v).unwrap_or(u16::MAX))
}

/// Floor a finite, non-negative, image-bounded float to a pixel index.
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is finite, clamped >= 0, and bounded by the image dimension; \
              float→int has no From/TryFrom"
)]
fn floor_u32(v: f32) -> u32 {
    v.max(0.0).floor() as u32
}

/// Round a `[0, 255]`-clamped float to a colour byte.
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is clamped to [0, 255]; float→int has no From/TryFrom"
)]
fn byte(v: f32) -> u8 {
    v.clamp(0.0, 255.0).round() as u8
}

// --- image helpers -------------------------------------------------------

/// Square-pad a frame to its larger side (black bars), origin-centered, into a
/// reused `out` buffer. (Re)allocates `out` only when the side changes — i.e.
/// once for a fixed camera resolution — so steady-state padding allocates
/// nothing. The black bars stay zero across frames (they are never written).
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

/// Allocating convenience wrapper over [`square_pad_into`] (tests/benchmarks).
#[allow(
    dead_code,
    reason = "allocating convenience wrapper exercised only by #[cfg(test)]; square_pad_into is the hot path"
)]
fn square_pad(frame: &Frame) -> RgbImage {
    let mut img = RgbImage::default();
    square_pad_into(frame, &mut img);
    img
}

/// Bilinearly resize `src` into a reused `dst` buffer. (Re)allocates `dst`
/// only when the target size changes — i.e. never in steady state — replacing
/// the per-acquisition output allocation of `image::imageops::resize`.
///
/// Sampling is plain bilinear at each output-pixel centre, with the same
/// half-pixel-centre mapping and clamp-to-edge as [`sample_bilinear`] /
/// [`warp_roi_into`]. For **upscales** (ratio < 1) this is exactly what the
/// `image` crate's `FilterType::Triangle` computes. For **downscales** Triangle
/// widens its kernel to average over the scale ratio while bilinear
/// point-samples a 2×2 neighbourhood. The accepted tradeoff: point sampling
/// **aliases on high-frequency content** (fine texture/edges can shimmer
/// frame-to-frame) where Triangle would low-pass it away. That is also exactly
/// what `MediaPipe`'s own `ImageToTensor` preprocessing does (`OpenCV`
/// `INTER_LINEAR` on CPU, GPU bilinear sampling), so the palm model sees the
/// resize conditions — aliasing included — it was built for. Agreement with
/// Triangle on smooth content is pinned by
/// `resize_into_matches_triangle_resize_on_gradients`.
fn resize_into(src: &RgbImage, w: u32, h: u32, dst: &mut RgbImage) {
    if dst.width() != w || dst.height() != h {
        *dst = RgbImage::new(w, h);
    }
    // Degenerate sizes leave dst's previous pixels in place; unreachable from
    // the pipeline (process() rejects zero-dimension frames before acquisition,
    // and w/h are the nonzero PALM_SIZE) — guarded here only against div-by-zero.
    if src.width() == 0 || src.height() == 0 || w == 0 || h == 0 {
        return;
    }
    // Output-pixel centre → source coordinates (half-pixel-centre convention):
    // src_x = (dst_x + 0.5) · (src_w / dst_w) − 0.5, src_y analog.
    let sx = dim(src.width()) / dim(w);
    let sy = dim(src.height()) / dim(h);
    for oy in 0..h {
        let y = (dim(oy) + 0.5) * sy - 0.5;
        for ox in 0..w {
            let x = (dim(ox) + 0.5) * sx - 0.5;
            dst.put_pixel(ox, oy, sample_bilinear(src, x, y));
        }
    }
}

/// `image`-crate Triangle resize (allocates its output). Tests/benchmarks
/// only — the equivalence-test oracle for [`resize_into`], which is what the
/// pipeline's detect path uses.
#[allow(
    dead_code,
    reason = "test-only equivalence oracle for resize_into (the pipeline's hot path)"
)]
fn resize(img: &RgbImage, w: u32, h: u32) -> RgbImage {
    image::imageops::resize(img, w, h, FilterType::Triangle)
}

/// Fill `out` with the NHWC `[1, h, w, 3]` `f32` tensor (RGB in `[0,1]`) of
/// `img`, reusing `out`'s buffers. `data.clear()` keeps capacity, so after the
/// first frame this refills without allocating.
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

/// Warp the rotated normalized ROI out of `square` into a reused `out_size`²
/// RGB crop `dst` (bilinear). Inverse-maps each output pixel through the ROI,
/// mirroring [`project_landmarks`]. (Re)allocates `dst` only when the side
/// changes, so per-frame warping allocates nothing.
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
            let px = sample_bilinear(square, nx * side, ny * side);
            dst.put_pixel(ox, oy, px);
        }
    }
}

/// Allocating convenience wrapper over [`warp_roi_into`] (tests/benchmarks).
#[allow(
    dead_code,
    reason = "allocating convenience wrapper exercised only by #[cfg(test)]; warp_roi_into is the hot path"
)]
fn warp_roi(square: &RgbImage, roi: &RoiRect, out: u32) -> RgbImage {
    let mut dst = RgbImage::default();
    warp_roi_into(square, roi, out, &mut dst);
    dst
}

fn sample_bilinear(img: &RgbImage, x: f32, y: f32) -> image::Rgb<u8> {
    let w = img.width();
    let h = img.height();
    if w == 0 || h == 0 {
        return image::Rgb([0, 0, 0]);
    }
    let xc = x.clamp(0.0, dim(w - 1));
    let yc = y.clamp(0.0, dim(h - 1));
    let fx = xc - xc.floor();
    let fy = yc - yc.floor();
    let x0u = floor_u32(xc);
    let y0u = floor_u32(yc);
    let x1u = (x0u + 1).min(w - 1);
    let y1u = (y0u + 1).min(h - 1);
    let mut out = [0u8; 3];
    for (c, slot) in out.iter_mut().enumerate() {
        let p00 = f32::from(img.get_pixel(x0u, y0u)[c]);
        let p10 = f32::from(img.get_pixel(x1u, y0u)[c]);
        let p01 = f32::from(img.get_pixel(x0u, y1u)[c]);
        let p11 = f32::from(img.get_pixel(x1u, y1u)[c]);
        let top = p00 + (p10 - p00) * fx;
        let bot = p01 + (p11 - p01) * fx;
        *slot = byte(top + (bot - top) * fy);
    }
    image::Rgb(out)
}

// --- model output selection ---------------------------------------------

fn pick_palm_outputs(out: &[Tensor]) -> Result<(&[f32], &[f32]), InferenceError> {
    let boxes = out
        .iter()
        .find(|t| t.shape == [1, 2016, 18])
        .ok_or_else(|| InferenceError::Run("palm: no [1,2016,18] output".into()))?;
    let scores = out
        .iter()
        .find(|t| t.shape == [1, 2016, 1])
        .ok_or_else(|| InferenceError::Run("palm: no [1,2016,1] output".into()))?;
    Ok((&boxes.data, &scores.data))
}

/// The landmark model's four outputs, selected by declared index order.
///
/// The scalar heads carry **Sigmoid ops inside the graph**, so [`Self::presence`]
/// and [`Self::handedness`] are already probabilities in `[0, 1]` — they must be
/// consumed raw. (Applying sigmoid again, as the old shape-matching selection
/// did, squashes any `[0, 1]` input into `[0.5, 0.731]`, which disabled both the
/// presence gate and the Left half of the handedness test.) The premise is
/// pinned against the vendored model by the `inference_ort` test
/// `ort_landmark_presence_is_a_probability_from_the_graph`.
#[derive(Debug)]
struct LandmarkOutputs<'a> {
    /// Crop-space landmarks, 21 × (x, y, z) in landmark-crop pixels (`[1, 63]`).
    image: &'a [f32],
    /// Hand-presence probability in `[0, 1]`.
    presence: f32,
    /// Handedness probability in `[0, 1]`; `>= 0.5` reads as a right hand.
    handedness: f32,
    /// World-space landmarks, 21 × (x, y, z) in hand-centred metres (`[1, 63]`).
    /// Decoded by [`decode_world_landmarks`]; the pose-invariant gesture
    /// signals (grab, pinch, palm normal) derive from these.
    world: &'a [f32],
}

/// Select the landmark model's outputs by declared index order — `0` image
/// landmarks `[1, 63]`, `1` presence `[1, 1]`, `2` handedness `[1, 1]`,
/// `3` world landmarks `[1, 63]` (the inference backend preserves the
/// session's declared output order). Each index is shape-sanity-checked; a
/// mismatch reports the observed shapes. No per-call allocation on the
/// success path (the error strings allocate only when returned).
fn pick_landmark_outputs(out: &[Tensor]) -> Result<LandmarkOutputs<'_>, InferenceError> {
    const WANT: [&[usize]; 4] = [&[1, 63], &[1, 1], &[1, 1], &[1, 63]];
    let shapes_ok = out.len() == WANT.len()
        && out
            .iter()
            .zip(WANT)
            .all(|(tensor, want)| tensor.shape == want);
    if !shapes_ok {
        let observed: Vec<&[usize]> = out.iter().map(|t| t.shape.as_slice()).collect();
        return Err(InferenceError::Run(format!(
            "landmark: unexpected output shapes {observed:?}; want {WANT:?} in declared order"
        )));
    }
    let scalar = |index: usize| {
        out[index]
            .data
            .first()
            .copied()
            .ok_or_else(|| InferenceError::Run(format!("landmark: output {index} has no data")))
    };
    Ok(LandmarkOutputs {
        image: &out[0].data,
        presence: scalar(1)?,
        handedness: scalar(2)?,
        world: &out[3].data,
    })
}

/// Test fixtures shared between this module's tests and the worker's
/// ([`super::worker`]): plausible mock data for the palm and landmark stages.
///
/// Gesture signals derive from the landmark model's world output, and
/// [`super::signals::hand_scale`] divides by the wrist→middle-MCP distance — a
/// degenerate all-zeros world tensor collapses that to `f32::EPSILON` and turns
/// grab/pinch into divide-by-epsilon garbage. Every landmark-model mock
/// therefore emits this anatomically plausible open hand instead of zeros.
#[cfg(test)]
pub(crate) mod fixtures {
    use bevy::math::Vec3;

    use super::super::inference::Tensor;
    use crate::input::hand::{LandmarkIndex, LANDMARK_COUNT};

    /// An open right hand in the landmark model's WORLD space: metric metres,
    /// wrist at the origin, camera-axis-aligned (x right, y **down** — fingers
    /// extend toward −y, i.e. upward in the image), flat in the XY plane
    /// (z = 0, palm plane facing the camera). Wrist → middle MCP is 0.09 m
    /// (the `hand_scale` reference); fingertips sit roughly a full hand-scale
    /// beyond the palm centre; the thumb splays toward −x.
    pub(crate) fn open_world_hand() -> [Vec3; LANDMARK_COUNT] {
        use LandmarkIndex as L;
        let mut lm = [Vec3::ZERO; LANDMARK_COUNT];
        let mut set = |i: L, x: f32, y: f32| lm[i.as_index()] = Vec3::new(x, y, 0.0);
        set(L::Wrist, 0.0, 0.0);
        // Thumb column, splayed toward −x.
        set(L::ThumbCmc, -0.025, -0.020);
        set(L::ThumbMcp, -0.045, -0.040);
        set(L::ThumbIp, -0.060, -0.055);
        set(L::ThumbTip, -0.075, -0.065);
        // Index finger.
        set(L::IndexMcp, -0.025, -0.090);
        set(L::IndexPip, -0.027, -0.125);
        set(L::IndexDip, -0.028, -0.150);
        set(L::IndexTip, -0.029, -0.170);
        // Middle finger.
        set(L::MiddleMcp, 0.0, -0.090);
        set(L::MiddlePip, 0.0, -0.130);
        set(L::MiddleDip, 0.0, -0.160);
        set(L::MiddleTip, 0.0, -0.185);
        // Ring finger.
        set(L::RingMcp, 0.025, -0.088);
        set(L::RingPip, 0.026, -0.125);
        set(L::RingDip, 0.027, -0.150);
        set(L::RingTip, 0.028, -0.170);
        // Pinky.
        set(L::PinkyMcp, 0.050, -0.080);
        set(L::PinkyPip, 0.052, -0.110);
        set(L::PinkyDip, 0.054, -0.130);
        set(L::PinkyTip, 0.056, -0.150);
        lm
    }

    /// Flatten 21 world points into the model's `[1, 63]` row-major
    /// `(x, y, z)` tensor data layout.
    pub(crate) fn world_tensor(points: &[Vec3; LANDMARK_COUNT]) -> Vec<f32> {
        let mut data = Vec::with_capacity(LANDMARK_COUNT * 3);
        for p in points {
            data.extend_from_slice(&[p.x, p.y, p.z]);
        }
        data
    }

    /// [`open_world_hand`] as ready-to-mock `[1, 63]` tensor data.
    pub(crate) fn open_world_tensor() -> Vec<f32> {
        world_tensor(&open_world_hand())
    }

    /// A plausibly spread mock hand in landmark-crop pixels: wrist + key
    /// MCPs + middle tip separated so all trackability gates pass. Centre-of-
    /// crop placement keeps the mock away from frame edges, so tests that
    /// expect a healthy hand are not inadvertently exercising the
    /// edge-invalidation path.
    ///
    /// Shared between [`super::tests::counting_pipeline_with_config`]
    /// (default `lms` argument) and the worker's
    /// [`super::super::worker`] test fixture.
    pub(crate) fn spread_image_landmarks() -> Vec<f32> {
        let mut lms = vec![112.0f32; 63];
        let mut set = |i: usize, x: f32, y: f32| {
            lms[i * 3] = x;
            lms[i * 3 + 1] = y;
        };
        set(0, 112.0, 160.0); // wrist
        set(9, 112.0, 90.0); // middle MCP
        set(5, 85.0, 110.0); // index MCP
        set(17, 140.0, 110.0); // pinky MCP
        set(12, 112.0, 50.0); // middle tip
        lms
    }

    /// Palm mock outputs: one hot central stride-8 anchor producing a 0.2×0.2
    /// detection at the frame centre; all other 2015 anchors score −100 and
    /// drop. Returns the two tensors `[boxes [1,2016,18], scores [1,2016,1]]`
    /// in the order the pipeline's `pick_palm_outputs` selects them by shape.
    ///
    /// Shared between [`super::tests::counting_pipeline_with_config`] and the
    /// worker's [`super::super::worker`] test fixture.
    pub(crate) fn hot_anchor_palm_outputs() -> Vec<Tensor> {
        let mut scores = vec![-100.0f32; 2016];
        let hot_anchor = (12 * 24 + 12) * 2; // central stride-8 cell, first anchor
        scores[hot_anchor] = 100.0;
        let mut boxes = vec![0.0f32; 2016 * 18];
        let hot_box = hot_anchor * 18;
        boxes[hot_box + 2] = 192.0 * 0.2;
        boxes[hot_box + 3] = 192.0 * 0.2;
        boxes[hot_box + 5] = 192.0 * 0.1;
        boxes[hot_box + 9] = -192.0 * 0.1;
        vec![
            Tensor {
                data: boxes,
                shape: vec![1, 2016, 18],
            },
            Tensor {
                data: scores,
                shape: vec![1, 2016, 1],
            },
        ]
    }

    /// Landmark mock outputs for one confident spread right hand:
    /// [`spread_image_landmarks`] image landmarks, presence = 0.98,
    /// handedness = 0.9 (Right), and [`open_world_tensor`] world landmarks.
    /// Returns four tensors in declared model output order (image, presence,
    /// handedness, world), matching what `pick_landmark_outputs` expects.
    ///
    /// Shared between [`super::tests::counting_pipeline`] and the worker's
    /// [`super::super::worker`] test fixture.
    pub(crate) fn confident_spread_landmark_outputs() -> Vec<Tensor> {
        vec![
            Tensor {
                data: spread_image_landmarks(),
                shape: vec![1, 63],
            },
            Tensor {
                data: vec![0.98], // presence: confidently a hand
                shape: vec![1, 1],
            },
            Tensor {
                data: vec![0.9], // handedness: Right
                shape: vec![1, 1],
            },
            Tensor {
                data: open_world_tensor(),
                shape: vec![1, 63],
            },
        ]
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::super::coords::{
        distance_m_to_leap_z_mm, size_estimated_distance_m, MEDIAPIPE_DEPTH_PROXY_MM,
    };
    use super::super::signals;
    use super::*;
    use crate::input::hand::LandmarkIndex;
    use crate::input::providers::mediapipe::capture::{FrameSource, MockFrameSource};

    fn model(name: &str) -> Box<dyn HandInference> {
        use super::super::inference_ort::OrtInference;
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/models/hand")
            .join(name);
        let bytes = std::fs::read(path).expect("read model");
        Box::new(
            OrtInference::load(&bytes, crate::settings::HandTrackingBackend::Auto, name)
                .expect("load model"),
        )
    }

    fn real_pipeline() -> Pipeline {
        Pipeline::new(
            model("palm_detection.onnx"),
            model("hand_landmark.onnx"),
            PipelineConfig::default(),
        )
    }

    #[test]
    fn solid_frame_yields_no_hands() {
        // A blank frame has no palm → the pipeline returns no hands without error,
        // exercising the full wiring (preprocess → palm → decode → NMS).
        let mut pipe = real_pipeline();
        let mut src = MockFrameSource::solid(640, 480, [0, 0, 0]);
        let mut frame = Frame::default();
        src.next_frame(&mut frame).expect("frame");
        let hands = pipe
            .process(&frame, Duration::from_millis(33))
            .expect("process");
        assert!(hands.is_empty());
    }

    /// End-to-end check on a real hand image. Skipped unless
    /// `WC_HANDTRACK_TEST_IMAGE` points at a readable image (so CI stays
    /// hermetic and there is no hardcoded path). Run locally with:
    ///   `WC_HANDTRACK_TEST_IMAGE=/path/to/hand.jpg cargo test -p wc-core \
    ///    --features hand-tracking-mediapipe -- --ignored e2e_real`
    #[test]
    #[ignore = "needs WC_HANDTRACK_TEST_IMAGE pointing at a hand photo"]
    fn e2e_real_hand_image_produces_a_hand() {
        let Ok(path) = std::env::var("WC_HANDTRACK_TEST_IMAGE") else {
            return;
        };
        let img = image::open(&path).expect("open test image").to_rgb8();
        let frame = Frame {
            width: img.width(),
            height: img.height(),
            rgb: img.into_raw(),
        };
        let mut pipe = real_pipeline();
        let hands = pipe
            .process(&frame, Duration::from_millis(33))
            .expect("process");
        assert!(!hands.is_empty(), "expected at least one hand");
        let h = &hands[0];
        // Palm should land within the Leap-mm working volume, and landmarks
        // should not be degenerate.
        assert!(
            h.palm_position.x.abs() <= 220.0,
            "palm x={}",
            h.palm_position.x
        );
        assert!(
            h.palm_position.y >= 40.0 && h.palm_position.y <= 350.0,
            "palm y={} out of Leap range [40, 350]; letterbox unprojection missing?",
            h.palm_position.y
        );
        let spread = h
            .landmarks
            .iter()
            .map(|l| l.distance(h.landmarks[0]))
            .fold(0.0_f32, f32::max);
        assert!(spread > 1.0, "landmarks too clustered: {spread}");
        println!(
            "e2e: {} hand(s); hand0 chirality={:?} pinch={:.2} grab={:.2} palm={:?}",
            hands.len(),
            h.chirality,
            h.pinch_strength,
            h.grab_strength,
            h.palm_position,
        );
    }

    /// Per-stage latency breakdown for the two-stage pipeline, in the profile
    /// `cargo rund` uses (our code at opt-level 1, ort/image at opt-level 3).
    /// Not a correctness test — a measurement harness for the framerate work.
    /// Run with:
    ///   `cargo test -p wc-core --features hand-tracking-mediapipe \
    ///    -- --ignored --nocapture profile_pipeline_stages`
    #[test]
    #[ignore = "measurement harness, not a correctness assertion; run with --nocapture"]
    fn profile_pipeline_stages() {
        use std::time::Instant;

        let mut palm = model("palm_detection.onnx");
        let mut landmark = model("hand_landmark.onnx");

        // Time `body` N times after one warm-up; return mean milliseconds.
        let bench = |iters: u32, body: &mut dyn FnMut()| -> f64 {
            body();
            let t = Instant::now();
            for _ in 0..iters {
                body();
            }
            (t.elapsed().as_secs_f64() * 1000.0) / f64::from(iters)
        };

        // A non-trivial synthetic frame (gradient) at each candidate capture res.
        let make_frame = |w: u32, h: u32| -> Frame {
            let mut rgb = vec![0u8; idx(w) * idx(h) * 3];
            for (i, px) in rgb.chunks_exact_mut(3).enumerate() {
                px[0] = u8::try_from(i % 256).unwrap_or(0);
                px[1] = u8::try_from((i / 7) % 256).unwrap_or(0);
                px[2] = u8::try_from((i / 13) % 256).unwrap_or(0);
            }
            Frame {
                width: w,
                height: h,
                rgb,
            }
        };

        eprintln!("\n=== mediapipe pipeline per-stage latency (mean ms) ===");

        // Preprocessing scales with capture resolution — measure the realistic set.
        for &(w, h) in &[(640u32, 480u32), (1280, 720), (1920, 1080)] {
            let frame = make_frame(w, h);
            let mut sq = square_pad(&frame);
            let t_pad = bench(20, &mut || {
                sq = square_pad(&frame);
            });
            let mut small = RgbImage::default();
            let t_resize = bench(20, &mut || {
                resize_into(&sq, PALM_SIZE, PALM_SIZE, &mut small);
            });
            eprintln!(
                "  preprocess @ {w}x{h}: square_pad {t_pad:.2}  resize->192 {t_resize:.2}  (sum {:.2})",
                t_pad + t_resize
            );
        }

        // Inference latency is data-independent (fixed conv/matmul FLOPs), so a
        // zeros tensor measures it faithfully.
        let palm_in = Tensor {
            data: vec![0.0; idx(PALM_SIZE) * idx(PALM_SIZE) * 3],
            shape: vec![1, idx(PALM_SIZE), idx(PALM_SIZE), 3],
        };
        let mut palm_out = Vec::new();
        let t_palm = bench(20, &mut || {
            palm.run(&palm_in, &mut palm_out).expect("palm run");
        });

        // ROI warp (one per detected hand) + landmark inference.
        let sq = square_pad(&make_frame(1280, 720));
        let roi = RoiRect {
            cx: 0.5,
            cy: 0.5,
            size: 0.5,
            rotation: 0.0,
        };
        let mut crop = warp_roi(&sq, &roi, LM_SIZE);
        let t_warp = bench(20, &mut || {
            crop = warp_roi(&sq, &roi, LM_SIZE);
        });
        let lm_in = Tensor {
            data: vec![0.0; idx(LM_SIZE) * idx(LM_SIZE) * 3],
            shape: vec![1, idx(LM_SIZE), idx(LM_SIZE), 3],
        };
        let mut lm_out = Vec::new();
        let t_lm = bench(20, &mut || {
            landmark.run(&lm_in, &mut lm_out).expect("landmark run");
        });

        eprintln!("  palm.run (192):       {t_palm:.2}");
        eprintln!("  warp_roi->224:        {t_warp:.2}");
        eprintln!("  landmark.run (224):   {t_lm:.2}");

        // Per-frame budgets at 1280x720 with one hand in view.
        let f720 = make_frame(1280, 720);
        let s720 = square_pad(&f720);
        let t_pad_720 = bench(20, &mut || {
            let _ = square_pad(&f720);
        });
        let mut small_720 = RgbImage::default();
        let t_resize_720 = bench(20, &mut || {
            resize_into(&s720, PALM_SIZE, PALM_SIZE, &mut small_720);
        });
        // Acquisition frame: square_pad + resize->192 + palm + warp + landmark.
        let acquire = t_pad_720 + t_resize_720 + t_palm + t_warp + t_lm;
        // Tracking frame (detect-then-track): no palm, no resize->192 — just
        // square_pad (warp samples it) + warp + landmark.
        let tracking = t_pad_720 + t_warp + t_lm;
        eprintln!(
            "\n  acquisition frame (palm path): {acquire:.2} ms  (~{:.1} fps)",
            1000.0 / acquire
        );
        eprintln!(
            "  tracking frame (palm skipped): {tracking:.2} ms  (~{:.1} fps)",
            1000.0 / tracking
        );
        eprintln!("=======================================================\n");
    }

    /// Inference stub that counts `run` calls and returns canned outputs, so a
    /// test can observe which model stages the pipeline invokes per frame.
    struct CountingInference {
        calls: std::sync::Arc<std::sync::atomic::AtomicU32>,
        outputs: Vec<Tensor>,
    }

    impl HandInference for CountingInference {
        fn run(&mut self, _input: &Tensor, out: &mut Vec<Tensor>) -> Result<(), InferenceError> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            out.clone_from(&self.outputs);
            Ok(())
        }
    }

    /// A plausibly spread mock hand (wrist + MCPs + middle tip placed apart) in
    /// landmark-crop pixels, for tests that need the geometry gates to pass.
    /// Delegates to [`fixtures::spread_image_landmarks`] so the worker tests
    /// share the same construction.
    fn spread_landmarks() -> Vec<f32> {
        fixtures::spread_image_landmarks()
    }

    /// Image landmarks for an OPEN hand tilted toward the camera: perspective
    /// foreshortening collapses the projected fingertips onto the palm centroid
    /// (~`(112, 127)` crop px for [`spread_landmarks`]' wrist/MCPs) while the
    /// hand is actually open. The wrist/MCP/PIP geometry the trackability gates
    /// and tracking ROI use stays hand-like; only the tip-to-palm distances —
    /// the grab/pinch geometry — are corrupted by the projection.
    fn foreshortened_image_landmarks() -> Vec<f32> {
        let mut lms = spread_landmarks();
        let mut set = |i: usize, x: f32, y: f32| {
            lms[i * 3] = x;
            lms[i * 3 + 1] = y;
        };
        set(LandmarkIndex::IndexTip.as_index(), 108.0, 122.0);
        set(LandmarkIndex::MiddleTip.as_index(), 112.0, 120.0);
        set(LandmarkIndex::RingTip.as_index(), 116.0, 122.0);
        set(LandmarkIndex::PinkyTip.as_index(), 120.0, 124.0);
        lms
    }

    /// Pull the four non-thumb fingertips of a world hand to `frac` hand-scales
    /// from the palm centre (`0` = touching the palm; the open fixture sits at
    /// roughly `1.3`). Leaves every other landmark — including `hand_scale`'s
    /// wrist/middle-MCP reference — untouched.
    fn curl_world_fingertips(
        mut hand: [Vec3; LANDMARK_COUNT],
        frac: f32,
    ) -> [Vec3; LANDMARK_COUNT] {
        let palm = palm_center(&hand);
        let scale = signals::hand_scale(&hand);
        for tip in [
            LandmarkIndex::IndexTip,
            LandmarkIndex::MiddleTip,
            LandmarkIndex::RingTip,
            LandmarkIndex::PinkyTip,
        ] {
            let dir = (hand[tip.as_index()] - palm).normalize_or_zero();
            hand[tip.as_index()] = palm + dir * (frac * scale);
        }
        hand
    }

    /// Rotate world points about the camera X axis — the tilt-toward-the-camera
    /// motion that foreshortens a hand in image space.
    fn rotate_world_about_x(hand: &[Vec3; LANDMARK_COUNT], radians: f32) -> [Vec3; LANDMARK_COUNT] {
        let (sin, cos) = radians.sin_cos();
        let mut out = *hand;
        for p in &mut out {
            // Standard rotation about X: y' = y·cosθ − z·sinθ, z' = y·sinθ + z·cosθ.
            *p = Vec3::new(p.x, p.y * cos - p.z * sin, p.y * sin + p.z * cos);
        }
        out
    }

    /// Build a pipeline wired with call-counting mocks: palm yields exactly one
    /// detection, landmark yields one high-presence hand (probability-realistic
    /// scalars for a confidently present right hand). Returns the pipeline plus
    /// the palm and landmark call counters.
    fn counting_pipeline() -> (
        Pipeline,
        std::sync::Arc<std::sync::atomic::AtomicU32>,
        std::sync::Arc<std::sync::atomic::AtomicU32>,
    ) {
        counting_pipeline_with_outputs(spread_landmarks(), 0.98, 0.9)
    }

    /// [`counting_pipeline_with_image_and_world`] with the plausible open-hand
    /// world fixture: image-landmark-focused tests get world-derived gesture
    /// signals reading "open" by default (an all-zeros world hand would make
    /// `hand_scale` ≈ epsilon and the signals garbage).
    fn counting_pipeline_with_outputs(
        lms: Vec<f32>,
        presence: f32,
        handedness: f32,
    ) -> (
        Pipeline,
        std::sync::Arc<std::sync::atomic::AtomicU32>,
        std::sync::Arc<std::sync::atomic::AtomicU32>,
    ) {
        counting_pipeline_with_image_and_world(
            lms,
            fixtures::open_world_tensor(),
            presence,
            handedness,
        )
    }

    /// [`counting_pipeline_with_config`] with the default [`PipelineConfig`].
    fn counting_pipeline_with_image_and_world(
        lms: Vec<f32>,
        world: Vec<f32>,
        presence: f32,
        handedness: f32,
    ) -> (
        Pipeline,
        std::sync::Arc<std::sync::atomic::AtomicU32>,
        std::sync::Arc<std::sync::atomic::AtomicU32>,
    ) {
        counting_pipeline_with_config(lms, world, presence, handedness, PipelineConfig::default())
    }

    /// Counting-mock pipeline whose landmark stage emits the given image and
    /// world landmarks, presence, and handedness. The mock mirrors the vendored
    /// model's declared output order — image landmarks, presence, handedness,
    /// world landmarks — with the scalars as real probabilities (the model's
    /// graph applies the sigmoid itself).
    fn counting_pipeline_with_config(
        lms: Vec<f32>,
        world: Vec<f32>,
        presence: f32,
        handedness: f32,
        config: PipelineConfig,
    ) -> (
        Pipeline,
        std::sync::Arc<std::sync::atomic::AtomicU32>,
        std::sync::Arc<std::sync::atomic::AtomicU32>,
    ) {
        use std::sync::atomic::AtomicU32;
        use std::sync::Arc;

        // Palm: shared hot-anchor fixture — one central stride-8 anchor yields
        // a 0.2×0.2 detection; all other anchors drop, keeping the mock away
        // from frame edges so tests that expect a healthy hand do not
        // inadvertently exercise the edge-invalidation path.
        let palm_out = fixtures::hot_anchor_palm_outputs();

        // Declared landmark-model output order: image landmarks, presence,
        // handedness, world landmarks. Selection is index-based, so the mock
        // must emit all four in this order.
        let lm_out = vec![
            Tensor {
                data: lms,
                shape: vec![1, 63],
            },
            Tensor {
                data: vec![presence],
                shape: vec![1, 1],
            },
            Tensor {
                data: vec![handedness],
                shape: vec![1, 1],
            },
            Tensor {
                data: world,
                shape: vec![1, 63],
            },
        ];

        let palm_calls = Arc::new(AtomicU32::new(0));
        let lm_calls = Arc::new(AtomicU32::new(0));
        let pipe = Pipeline::new(
            Box::new(CountingInference {
                calls: Arc::clone(&palm_calls),
                outputs: palm_out,
            }),
            Box::new(CountingInference {
                calls: Arc::clone(&lm_calls),
                outputs: lm_out,
            }),
            config,
        );
        (pipe, palm_calls, lm_calls)
    }

    /// A consistent, non-empty frame for driving the pipeline. Square, so its
    /// content rect is the full `[0, 1]²` — these mock tests exercise the
    /// palm/landmark/association path, not square-padding geometry (that is
    /// covered separately by the `ContentRect` tests).
    fn consistent_frame() -> Frame {
        Frame {
            width: 64,
            height: 64,
            rgb: vec![128u8; 64 * 64 * 3],
        }
    }

    #[test]
    fn palm_reruns_every_frame_while_under_max_hands() {
        use std::sync::atomic::Ordering;
        // Count-gated re-detection: with one hand tracked and MAX_HANDS == 2, the
        // pipeline stays under the cap, so palm runs EVERY frame looking for a
        // second hand (no fixed timer, so a steadily-tracked pair is never
        // re-seeded). The existing hand must NOT be duplicated: its fresh palm
        // detection associates with its track (centres close) and is dropped.
        assert_eq!(MAX_HANDS, 2, "this test assumes the two-hand cap");
        let (mut pipe, palm_calls, lm_calls) = counting_pipeline();
        let frame = consistent_frame();
        let dt = Duration::from_millis(16);

        for n in 1..=3u32 {
            let hands = pipe.process(&frame, dt).expect("frame");
            assert_eq!(
                hands.len(),
                1,
                "frame {n}: exactly one hand, never duplicated"
            );
            assert_eq!(
                palm_calls.load(Ordering::Relaxed),
                n,
                "frame {n}: palm re-runs while under MAX_HANDS"
            );
            assert_eq!(
                lm_calls.load(Ordering::Relaxed),
                n,
                "frame {n}: landmark runs once (the tracked hand)"
            );
        }
    }

    /// A square ROI centred at `(cx, cy)` with a typical mid-range size (0.3).
    /// Size feeds the scale-relative association gate
    /// ([`association_gate`] = `max(0.5×max(size), 0.08)` → 0.15 here);
    /// rotation is irrelevant to association.
    fn roi_at(cx: f32, cy: f32) -> RoiRect {
        RoiRect {
            cx,
            cy,
            size: 0.3,
            rotation: 0.0,
        }
    }

    fn roi_with_size(cx: f32, cy: f32, size: f32) -> RoiRect {
        RoiRect {
            size,
            ..roi_at(cx, cy)
        }
    }

    fn landmark_set_with_spread(spread: f32) -> [Vec3; LANDMARK_COUNT] {
        let mut landmarks = [Vec3::splat(0.5); LANDMARK_COUNT];
        landmarks[0].x -= spread * 0.5;
        landmarks[1].x += spread * 0.5;
        landmarks[2].y -= spread * 0.5;
        landmarks[3].y += spread * 0.5;
        landmarks
    }

    /// Build a track set from `(cx, cy)` centres.
    fn tracks(items: &[(f32, f32)]) -> SmallVec<[RoiRect; MAX_HANDS]> {
        items.iter().map(|&(cx, cy)| roi_at(cx, cy)).collect()
    }

    #[test]
    fn associate_keeps_track_and_drops_overlapping_palm() {
        // A fresh palm detection near an existing track (same hand) is discarded;
        // the smooth tracked ROI is kept verbatim — no duplicate, no identity reset.
        let out = associate(tracks(&[(0.5, 0.5)]), &[roi_at(0.55, 0.52)]);
        assert_eq!(out.len(), 1);
        assert!(
            (out[0].cx - 0.5).abs() < 1e-6 && (out[0].cy - 0.5).abs() < 1e-6,
            "kept the track, not the palm ROI",
        );
    }

    // --- Phase P6: scale-relative association gate + min-based spread check ---

    // regression: these two ran red against the old code (fixed 0.25 gate /
    // mean-based spread metric) — each pins a defect the old constants had.

    /// Two small far-away ROIs (size 0.1) whose centres are 0.12 apart.
    ///
    /// Old fixed gate 0.25: 0.12 <= 0.25 → merged into one hand (swallowed the
    /// second). New scale-relative gate: max(0.5×0.1, 0.08) = 0.08, and
    /// 0.12 > 0.08 → treated as distinct hands.
    #[test]
    fn associate_small_rois_close_but_distinct_are_kept_separately() {
        let a = roi_with_size(0.3, 0.5, 0.1);
        let b = roi_with_size(0.42, 0.5, 0.1); // distance = 0.12
        let out = associate(SmallVec::from_slice(&[a]), &[b]);
        assert_eq!(
            out.len(),
            2,
            "two small hands at distance 0.12 must not be merged; old 0.25 gate swallowed the second"
        );
    }

    /// Line-collapsed landmark set (wide bbox but nearly zero height) must fail
    /// the spread check.
    ///
    /// Old metric: mean((w+h)/2) = (0.2+0.01)/2 = 0.105 >= 0.04 → passed (BUG).
    /// New metric: min(w, h) = 0.01 < 0.04 → rejected. A line-shaped cluster
    /// (all landmarks collapsed onto a horizontal line) triggers a false fist in
    /// Line's grab model and must be dropped before grab is derived.
    #[test]
    fn line_collapsed_landmarks_fail_spread_check() {
        // Place wrist at left, middle-MCP directly right (wide x), zero y spread.
        let mut landmarks = [Vec3::splat(0.5); LANDMARK_COUNT];
        // Create a bbox that is 0.2 wide but 0.01 tall: x spans [0.4, 0.6], y
        // spans [0.495, 0.505].
        landmarks[0] = Vec3::new(0.40, 0.5, 0.0); // leftmost
        landmarks[1] = Vec3::new(0.60, 0.5, 0.0); // rightmost
        landmarks[2] = Vec3::new(0.5, 0.495, 0.0); // topmost
        landmarks[3] = Vec3::new(0.5, 0.505, 0.0); // bottommost
                                                   // All other landmarks at (0.5, 0.5) — inside both bbox extents.
        let full = ContentRect::for_frame(64, 64);
        assert!(
            !landmarks_trackable(&landmarks, full),
            "line-collapsed landmarks (w=0.2, h=0.01) must fail: min(w,h)=0.01 < 0.04"
        );
    }

    // design-range: these two pin the new gate's intended band behaviour
    // (scale-widened merge near a large ROI; the jitter floor on tiny ROIs).

    /// Large tracked ROI (size 0.6) with a detection 0.27 away must merge.
    ///
    /// The distance is chosen inside the (0.25, 0.30) discrimination band: the
    /// old fixed 0.25 gate would have **added** this detection as a phantom
    /// second hand (0.27 > 0.25), while the scale-relative gate
    /// max(0.5×0.6, 0.08) = 0.30 merges it (0.27 ≤ 0.30). This pins that the
    /// gate *widens* with ROI size — not merely that the old behaviour is
    /// preserved.
    #[test]
    fn associate_large_roi_merges_detection_within_scaled_gate() {
        let tracked = roi_with_size(0.5, 0.5, 0.6);
        let detection = roi_with_size(0.77, 0.5, 0.3); // distance = 0.27
        let out = associate(SmallVec::from_slice(&[tracked]), &[detection]);
        assert_eq!(
            out.len(),
            1,
            "detection 0.27 away from a size-0.6 track must merge; \
             the old fixed 0.25 gate added it as a phantom second hand"
        );
        assert!(
            (out[0].cx - 0.5).abs() < 1e-6,
            "tracked ROI wins (kept verbatim)"
        );
    }

    /// Floor case: two tiny ROIs (size 0.05) at centre distance 0.05.
    ///
    /// Geometric gate 0.5×0.05 = 0.025 is below the floor 0.08; the floor
    /// applies, and 0.05 <= 0.08 → merged. Prevents detector jitter on
    /// small/far ROIs from duplicating a single hand.
    #[test]
    fn associate_floor_merges_tiny_rois_with_detector_jitter() {
        let a = roi_with_size(0.5, 0.5, 0.05);
        let b = roi_with_size(0.55, 0.5, 0.05); // distance = 0.05
        let out = associate(SmallVec::from_slice(&[a]), &[b]);
        assert_eq!(
            out.len(),
            1,
            "tiny ROIs within the 0.08 floor must be treated as the same hand"
        );
    }

    #[test]
    fn associate_adds_well_separated_palm_as_new_hand() {
        // Existing hand near (0.3,0.3); palm sees it (dropped) plus a far second
        // hand (added).
        let out = associate(
            tracks(&[(0.3, 0.3)]),
            &[roi_at(0.3, 0.31), roi_at(0.8, 0.8)],
        );
        assert_eq!(out.len(), 2, "near detection dropped; far one added");
        assert!(out.iter().any(|r| (r.cx - 0.8).abs() < 1e-6));
    }

    #[test]
    fn associate_from_empty_acquires_all_detections() {
        let out = associate(SmallVec::new(), &[roi_at(0.2, 0.2), roi_at(0.7, 0.7)]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn associate_caps_at_max_hands() {
        // Two tracks already fill the cap; a further (far) detection cannot exceed
        // MAX_HANDS — the source of "extra confused with a second hand" when the
        // old timer kept re-seeding.
        let out = associate(tracks(&[(0.2, 0.2), (0.8, 0.8)]), &[roi_at(0.5, 0.1)]);
        assert_eq!(out.len(), MAX_HANDS);
    }

    #[test]
    fn off_screen_roi_is_dropped() {
        // A square frame's content rect is the full [0, 1]² — the original
        // frame-edge behaviour.
        let full = ContentRect::for_frame(64, 64);
        assert!(roi_on_screen(&roi_at(0.5, 0.5), full));
        assert!(
            roi_on_screen(&roi_at(0.0, 1.0), full),
            "edge is still on screen"
        );
        assert!(
            !roi_on_screen(&roi_at(-0.01, 0.5), full),
            "palm left the frame on the left"
        );
        assert!(
            !roi_on_screen(&roi_at(0.5, 1.2), full),
            "palm left the frame at the bottom"
        );
    }

    #[test]
    fn collapsed_roi_is_not_trackable() {
        let full = ContentRect::for_frame(64, 64);
        assert!(
            roi_trackable(&roi_with_size(0.5, 0.5, MIN_TRACK_ROI_SIZE), full),
            "minimum plausible ROI is still tracked"
        );
        assert!(
            !roi_trackable(&roi_with_size(0.5, 0.5, MIN_TRACK_ROI_SIZE * 0.5), full),
            "collapsed ROI is dropped even when its centre is on screen"
        );
    }

    #[test]
    fn collapsed_landmarks_are_not_trackable() {
        let full = ContentRect::for_frame(64, 64);
        assert!(
            landmarks_trackable(
                &landmark_set_with_spread(MIN_TRACK_LANDMARK_SPREAD * 1.1),
                full
            ),
            "minimum plausible landmark spread is still tracked"
        );
        assert!(
            !landmarks_trackable(
                &landmark_set_with_spread(MIN_TRACK_LANDMARK_SPREAD * 0.5),
                full
            ),
            "tiny high-presence landmark clusters are dropped"
        );
    }

    #[test]
    fn edge_pinned_landmarks_are_not_trackable() {
        let full = ContentRect::for_frame(64, 64);
        let mut landmarks = landmark_set_with_spread(MIN_TRACK_LANDMARK_SPREAD * 1.1);
        landmarks[0].x = TRACK_LANDMARK_EDGE_MARGIN * 0.5;

        assert!(
            !landmarks_trackable(&landmarks, full),
            "landmarks touching the camera edge are dropped before grab is derived"
        );
    }

    #[test]
    fn landmarks_in_padding_bars_are_not_trackable() {
        let landscape = ContentRect::for_frame(1280, 720);
        let mut landmarks = landmark_set_with_spread(MIN_TRACK_LANDMARK_SPREAD * 1.1);
        landmarks[0].y = landscape.y0 - TRACK_LANDMARK_EDGE_MARGIN;

        assert!(
            !landmarks_trackable(&landmarks, landscape),
            "square-padding bars are outside the usable camera content"
        );
    }

    #[test]
    fn high_presence_collapsed_landmarks_do_not_keep_a_track() {
        let (mut pipe, _palm, _lm) = counting_pipeline_with_outputs(vec![112.0f32; 63], 0.98, 0.9);
        let hands = pipe
            .process(&consistent_frame(), Duration::from_millis(33))
            .expect("process");

        assert!(
            hands.is_empty(),
            "collapsed high-presence landmarks should emit no hand"
        );
        assert!(
            pipe.tracked.is_empty(),
            "collapsed high-presence landmarks should not occupy a tracking slot"
        );
    }

    #[test]
    fn content_rect_excludes_landscape_padding_bars() {
        // A 1280x720 landscape frame square-pads to 1280x1280 with black bars
        // top and bottom: content y ∈ [280/1280, 1000/1280] = [0.219, 0.781],
        // full width. This is the Bug-2/Bug-3 case — a hand leaving via the top
        // or bottom enters a bar while its centre is still within [0, 1].
        let c = ContentRect::for_frame(1280, 720);
        assert!(
            (c.x0 - 0.0).abs() < 1e-6 && (c.x1 - 1.0).abs() < 1e-6,
            "{c:?}"
        );
        assert!(
            (c.y0 - 0.218_75).abs() < 1e-4 && (c.y1 - 0.781_25).abs() < 1e-4,
            "{c:?}"
        );

        // Centre frame: on screen. Mid-band edges: on screen.
        assert!(roi_on_screen(&roi_at(0.5, 0.5), c));
        assert!(
            roi_on_screen(&roi_at(0.5, 0.22), c),
            "just inside the top band"
        );
        // In a padding bar (top/bottom) with centre still in [0, 1]: OFF screen.
        // Before the fix these counted as on-screen and lingered as phantoms.
        assert!(
            !roi_on_screen(&roi_at(0.5, 0.10), c),
            "drifted into the top black bar"
        );
        assert!(
            !roi_on_screen(&roi_at(0.5, 0.95), c),
            "drifted into the bottom black bar"
        );
        // Horizontal exits still leave [0, 1] and are caught as before.
        assert!(!roi_on_screen(&roi_at(-0.01, 0.5), c));
    }

    #[test]
    fn content_rect_for_portrait_excludes_side_bars() {
        // A portrait frame (taller than wide) pads left/right instead.
        let c = ContentRect::for_frame(480, 640);
        assert!(
            (c.y0 - 0.0).abs() < 1e-6 && (c.y1 - 1.0).abs() < 1e-6,
            "{c:?}"
        );
        assert!(
            (c.x0 - 0.125).abs() < 1e-4 && (c.x1 - 0.875).abs() < 1e-4,
            "{c:?}"
        );
        assert!(
            !roi_on_screen(&roi_at(0.05, 0.5), c),
            "drifted into the left bar"
        );
    }

    #[test]
    fn content_rect_for_square_is_full_unit_square() {
        let c = ContentRect::for_frame(720, 720);
        assert_eq!(
            c,
            ContentRect {
                x0: 0.0,
                y0: 0.0,
                x1: 1.0,
                y1: 1.0
            }
        );
    }

    /// Invert [`image_norm_to_leap_mm`] (mirror on, square frame → content-norm
    /// is identity) to recover a landmark's square-norm xy from the emitted
    /// Leap-mm value. Used to compute the expected size-estimated depth from
    /// the same image geometry the pipeline saw.
    fn mm_to_square_norm_xy(p: Vec3) -> bevy::math::Vec2 {
        use crate::input::projection::{LEAP_X_HALFRANGE_MM, LEAP_Y_MAX_MM, LEAP_Y_MIN_MM};
        // x_mm = (1 − x − 0.5) · 2·HALF  ⇒  x = 0.5 − x_mm / (2·HALF).
        let x = 0.5 - p.x / (2.0 * LEAP_X_HALFRANGE_MM);
        // y_mm = MAX − y · (MAX − MIN)  ⇒  y = (MAX − y_mm) / (MAX − MIN).
        let y = (LEAP_Y_MAX_MM - p.y) / (LEAP_Y_MAX_MM - LEAP_Y_MIN_MM);
        bevy::math::Vec2::new(x, y)
    }

    #[test]
    fn palm_z_is_the_size_estimated_depth_when_k_positive() {
        // Phase P5: with the estimator on (default k = 0.8) the emitted palm z
        // is the size-estimated depth — k · |world wrist→middleMCP| /
        // |image wrist→middleMCP| remapped into Leap z — NOT the old 120 pin.
        // One frame in, so the per-track EMA is freshly seeded with the raw
        // estimate and the emitted z equals it exactly.
        let (mut pipe, _palm, _lm) = counting_pipeline();
        let hands = pipe
            .process(&consistent_frame(), Duration::from_millis(33))
            .expect("process");
        assert_eq!(hands.len(), 1);
        let h = &hands[0];

        // Reconstruct the segment the estimator measured: world side from the
        // mock's world fixture, image side by inverting the mm mapping on the
        // emitted wrist/middle-MCP landmarks (square 64×64 frame → content-norm
        // is identity, so this recovers square-norm exactly).
        let world = fixtures::open_world_hand();
        let world_size = world[LandmarkIndex::Wrist.as_index()]
            .distance(world[LandmarkIndex::MiddleMcp.as_index()]);
        let image_size = mm_to_square_norm_xy(h.landmarks[LandmarkIndex::Wrist.as_index()])
            .distance(mm_to_square_norm_xy(
                h.landmarks[LandmarkIndex::MiddleMcp.as_index()],
            ));
        let want = distance_m_to_leap_z_mm(size_estimated_distance_m(
            world_size,
            image_size,
            PipelineConfig::default().depth_calibration_k,
        ));
        // Guard against a vacuous fixture: the expected depth must be a live
        // mid-range value, away from both rails and from the old pin.
        assert!(
            want > 41.0 && want < 349.0,
            "fixture depth {want} should be off both rails"
        );
        assert!(
            (want - MEDIAPIPE_DEPTH_PROXY_MM).abs() > 1.0,
            "fixture depth {want} must differ from the 120 pin to be probative"
        );
        assert!(
            (h.palm_position.z - want).abs() < 0.1,
            "palm z = {} (want size-estimated {want})",
            h.palm_position.z
        );
    }

    #[test]
    fn palm_z_falls_back_to_the_pin_when_k_disabled() {
        // The escape hatch: k = 0 disables the estimator and restores exactly
        // today's fixed 120 mm pin (instant rollback knob during a live set).
        let config = PipelineConfig {
            depth_calibration_k: 0.0,
            ..PipelineConfig::default()
        };
        let (mut pipe, _palm, _lm) = counting_pipeline_with_config(
            spread_landmarks(),
            fixtures::open_world_tensor(),
            0.98,
            0.9,
            config,
        );
        let hands = pipe
            .process(&consistent_frame(), Duration::from_millis(33))
            .expect("process");
        assert_eq!(hands.len(), 1);
        assert!(
            (hands[0].palm_position.z - MEDIAPIPE_DEPTH_PROXY_MM).abs() < 1e-3,
            "palm z = {} (want the {MEDIAPIPE_DEPTH_PROXY_MM} fallback pin)",
            hands[0].palm_position.z
        );
    }

    // --- "Est. distance (mm)" diagnostic (hardware-session calibration fix) --

    /// The dev-panel calibration metric must report the PHYSICAL estimated
    /// camera distance (mm) — `distance_m × 1000`, the pre-remap output of the
    /// size estimator — not the Leap-remapped z. The remapped z is clamped to
    /// `[40, 350]`, so under the old field the documented procedure ("tune k
    /// until the readout ≈ a tape-measured 500 mm") was unsatisfiable and k
    /// drifted to the slider max chasing it.
    #[test]
    fn est_distance_diagnostic_reports_physical_distance_not_leap_z() {
        let (mut pipe, _palm, _lm) = counting_pipeline();
        let hands = pipe
            .process(&consistent_frame(), Duration::from_millis(33))
            .expect("process");
        assert_eq!(hands.len(), 1);
        let h = &hands[0];

        // Reconstruct the segment the estimator measured, exactly as
        // `palm_z_is_the_size_estimated_depth_when_k_positive` does: world side
        // from the mock's world fixture, image side by inverting the mm mapping
        // on the emitted wrist/middle-MCP landmarks.
        let world = fixtures::open_world_hand();
        let world_size = world[LandmarkIndex::Wrist.as_index()]
            .distance(world[LandmarkIndex::MiddleMcp.as_index()]);
        let image_size = mm_to_square_norm_xy(h.landmarks[LandmarkIndex::Wrist.as_index()])
            .distance(mm_to_square_norm_xy(
                h.landmarks[LandmarkIndex::MiddleMcp.as_index()],
            ));
        let distance_m = size_estimated_distance_m(
            world_size,
            image_size,
            PipelineConfig::default().depth_calibration_k,
        );
        let want_mm = distance_m * 1000.0;
        let leap_z = distance_m_to_leap_z_mm(distance_m);
        // Non-vacuous: for this fixture the physical distance and the remapped
        // Leap z must differ, or the assertion below could not tell them apart.
        assert!(
            (want_mm - leap_z).abs() > 1.0,
            "fixture is vacuous: physical {want_mm} mm ≈ leap z {leap_z} mm"
        );
        let want = u64::from(floor_u32(want_mm + 0.5));
        let got = pipe.diagnostics().est_distance_mm;
        assert!(
            got.abs_diff(want) <= 1,
            "diagnostic {got} mm (want physical ≈ {want} mm, NOT leap z {leap_z:.0} mm)"
        );
    }

    /// `k <= 0` disables the estimator: there is no physical estimate under the
    /// fixed pin, so the metric reads `0` (the label semantics for off/no-hand).
    #[test]
    fn est_distance_diagnostic_is_zero_when_estimator_off() {
        let config = PipelineConfig {
            depth_calibration_k: 0.0,
            ..PipelineConfig::default()
        };
        let (mut pipe, _palm, _lm) = counting_pipeline_with_config(
            spread_landmarks(),
            fixtures::open_world_tensor(),
            0.98,
            0.9,
            config,
        );
        let hands = pipe
            .process(&consistent_frame(), Duration::from_millis(33))
            .expect("process");
        assert_eq!(hands.len(), 1, "the hand is still tracked under the pin");
        assert_eq!(
            pipe.diagnostics().est_distance_mm,
            0,
            "estimator off → no physical estimate → 0"
        );
    }

    // --- raw vs deadzoned grab diagnostics (hardware-session visibility) ----

    /// The "Grab raw (‰)" diagnostic must report the PRE-deadzone geometric
    /// grab while the emitted hand (and "Grab (‰)") carry the post-deadzone
    /// value. The pair lets the operator SEE the deadzone subtracting — the
    /// slider felt unrespected because, with grab on world landmarks, a
    /// relaxed hand's raw grab is already near 0 and the deadzone only shapes
    /// mid-curl response.
    #[test]
    fn grab_diagnostics_report_raw_and_deadzoned_values() {
        // Mid-curl world fixture: raw grab is mid-range, so raw and deadzoned
        // genuinely differ (an open hand clamps both to 0 — vacuous).
        let world = curl_world_fingertips(fixtures::open_world_hand(), 0.6);
        let (mut pipe, _palm, _lm) = counting_pipeline_with_image_and_world(
            spread_landmarks(),
            fixtures::world_tensor(&world),
            0.98,
            0.9,
        );
        let hands = pipe
            .process(&consistent_frame(), Duration::from_millis(33))
            .expect("process");
        assert_eq!(hands.len(), 1);

        let raw = grab_strength(&world);
        let deadzoned = apply_grab_deadzone(raw, PipelineConfig::default().grab_rest_deadzone);
        // Non-vacuous: the fixture must be mid-curl with a real deadzone delta.
        assert!(raw > 0.1 && raw < 0.9, "raw grab {raw} should be mid-range");
        assert!(
            (raw - deadzoned) > 1e-3,
            "fixture is vacuous: raw {raw} ≈ deadzoned {deadzoned}"
        );

        assert!(
            (hands[0].grab_strength - deadzoned).abs() < 1e-6,
            "emitted hand carries the POST-deadzone grab: {} (want {deadzoned})",
            hands[0].grab_strength
        );
        let d = pipe.diagnostics();
        assert_eq!(
            d.grab_raw_permille,
            u64::from(floor_u32(raw.mul_add(1000.0, 0.5))),
            "raw diagnostic must be the PRE-deadzone grab"
        );
        assert_eq!(
            d.grab_permille,
            u64::from(floor_u32(deadzoned.mul_add(1000.0, 0.5))),
            "deadzoned diagnostic must match the emitted hand"
        );
        assert_ne!(
            d.grab_raw_permille, d.grab_permille,
            "the two metrics must visibly differ on a mid-curl hand"
        );
    }

    /// No hand this frame → no focal-hand distance → `0`.
    #[test]
    fn est_distance_diagnostic_is_zero_when_no_hand() {
        // Presence below threshold: the ROI is rejected, no hand is emitted.
        let (mut pipe, _palm, _lm) = counting_pipeline_with_outputs(spread_landmarks(), 0.3, 0.9);
        let hands = pipe
            .process(&consistent_frame(), Duration::from_millis(33))
            .expect("process");
        assert!(hands.is_empty());
        assert_eq!(pipe.diagnostics().est_distance_mm, 0);
    }

    #[test]
    fn grab_deadzone_zeroes_the_rest_floor_and_keeps_full_fist() {
        // A relaxed-open floor at/under the deadzone collapses to exactly 0 so
        // Line's `grab > 0` decay gate releases; a full fist still reaches 1.
        assert!(apply_grab_deadzone(0.10, 0.12) < 1e-6, "below deadzone → 0");
        assert!(apply_grab_deadzone(0.12, 0.12) < 1e-6, "at deadzone → 0");
        assert!(
            (apply_grab_deadzone(1.0, 0.12) - 1.0).abs() < 1e-6,
            "full fist stays 1",
        );
        // Mid-grab is rescaled, not clipped: 0.56 → (0.56-0.12)/0.88 = 0.5.
        assert!((apply_grab_deadzone(0.56, 0.12) - 0.5).abs() < 1e-6);
        // A zero deadzone is a pass-through; a degenerate >0.95 deadzone clamps.
        assert!((apply_grab_deadzone(0.3, 0.0) - 0.3).abs() < 1e-6);
        assert!(
            apply_grab_deadzone(0.5, 1.5) < 1e-6,
            "degenerate deadzone clamps"
        );
    }

    // --- Phase P4: pose-invariant gesture signals from world landmarks -----

    #[test]
    fn tilted_open_hand_does_not_read_grabbed() {
        // Review finding #5 / "tilted hands read as grabbed": the WORLD tensor
        // says the hand is open; the IMAGE tensor shows it foreshortened by a
        // tilt toward the camera (projected tips collapsed onto the palm).
        // Signals must come from the world landmarks — on image landmarks this
        // hand reads as a full grab (and a strong pinch).
        let (mut pipe, _p, _l) = counting_pipeline_with_image_and_world(
            foreshortened_image_landmarks(),
            fixtures::open_world_tensor(),
            0.98,
            0.9,
        );
        let hands = pipe
            .process(&consistent_frame(), Duration::from_millis(33))
            .expect("process");
        assert_eq!(hands.len(), 1);
        assert!(
            hands[0].grab_strength < 0.3,
            "open-but-tilted hand reads grab={} (image-landmark foreshortening)",
            hands[0].grab_strength
        );
        assert!(
            hands[0].pinch_strength < 0.3,
            "open-but-tilted hand reads pinch={} (image-landmark foreshortening)",
            hands[0].pinch_strength
        );
    }

    #[test]
    fn flat_world_hand_palm_normal_maps_axes_for_both_mirrors() {
        // The fixture hand is flat in the world XY plane (all z = 0): after the
        // world→Leap orientation map the palm normal must be a pure ±z, and
        // mirroring must flip it (the x flip reverses cross-product handedness,
        // matching the positional mirror). For the right-hand fixture with the
        // held chirality Right, mirror OFF:
        //   a = index_mcp − wrist = (−0.025, +0.09, 0) after the y flip,
        //   b = pinky_mcp − wrist = (+0.05, +0.08, 0)
        //   a × b = (0, 0, −0.0065) → normal −z; mirror ON negates x → +z.
        // The physical sign (toward vs away from the camera, vs the Leap
        // provider's out-of-the-palm convention) still needs hardware
        // validation; this pins internal consistency of the axis map.
        for (mirror, want_z) in [(false, -1.0_f32), (true, 1.0_f32)] {
            let config = PipelineConfig {
                mirror,
                ..PipelineConfig::default()
            };
            let (mut pipe, _p, _l) = counting_pipeline_with_config(
                spread_landmarks(),
                fixtures::open_world_tensor(),
                0.98,
                0.9,
                config,
            );
            let hands = pipe
                .process(&consistent_frame(), Duration::from_millis(33))
                .expect("process");
            assert_eq!(hands.len(), 1);
            let n = hands[0].palm_normal;
            assert!(
                n.x.abs() < 1e-4 && n.y.abs() < 1e-4,
                "mirror={mirror}: normal {n:?} is not perpendicular to the flat world hand"
            );
            assert!(
                (n.z - want_z).abs() < 1e-3,
                "mirror={mirror}: normal {n:?}, want z = {want_z}"
            );
        }
    }

    #[test]
    fn world_signals_are_invariant_under_hand_tilt() {
        // Rotating the world hand ~60° about the camera X axis (the tilt that
        // foreshortens image landmarks) must not move world-derived grab/pinch.
        // Use a half-curled, mid-pinch hand: the open fixture clamps both
        // signals to 0 and would make the invariance assertion vacuous.
        let mut base = curl_world_fingertips(fixtures::open_world_hand(), 0.6);
        // Bring the thumb tip near (not onto) the index tip for a mid-range
        // pinch; the non-axis-aligned offset keeps the rotation non-trivial.
        base[LandmarkIndex::ThumbTip.as_index()] =
            base[LandmarkIndex::IndexTip.as_index()] + Vec3::splat(0.0125);
        let tilted = rotate_world_about_x(&base, 60.0_f32.to_radians());

        let run = |world: Vec<f32>| {
            let (mut pipe, _p, _l) =
                counting_pipeline_with_image_and_world(spread_landmarks(), world, 0.98, 0.9);
            let hands = pipe
                .process(&consistent_frame(), Duration::from_millis(33))
                .expect("process");
            assert_eq!(hands.len(), 1);
            (hands[0].grab_strength, hands[0].pinch_strength)
        };
        let (g0, p0) = run(fixtures::world_tensor(&base));
        let (g1, p1) = run(fixtures::world_tensor(&tilted));

        // Guard against vacuity: both signals must be mid-range, i.e. actually
        // derived from this world hand rather than clamped or image-derived.
        assert!(g0 > 0.1 && g0 < 0.9, "grab {g0} should be mid-range");
        assert!(p0 > 0.1 && p0 < 0.9, "pinch {p0} should be mid-range");
        assert!(
            (g0 - g1).abs() < 0.05,
            "grab changed under tilt: {g0} → {g1}"
        );
        assert!(
            (p0 - p1).abs() < 0.05,
            "pinch changed under tilt: {p0} → {p1}"
        );
    }

    #[test]
    fn live_deadzone_source_overrides_config_grab() {
        // The tuning UI shares an atomic f32-bits cell with the worker pipeline;
        // process() must pick up a re-tune before deriving this frame's grab.
        // The world hand is half-curled so the raw (deadzone-0) grab is real.
        let (mut pipe, _p, _l) = counting_pipeline_with_image_and_world(
            spread_landmarks(),
            fixtures::world_tensor(&curl_world_fingertips(fixtures::open_world_hand(), 0.6)),
            0.98,
            0.9,
        );
        let cell = Arc::new(MediaPipeLiveTuning::new(
            0.0,
            PipelineConfig::default().depth_calibration_k,
        ));
        pipe.set_live_tuning_source(Arc::clone(&cell));
        let frame = consistent_frame();
        let dt = Duration::from_millis(33);

        // Deadzone 0 → the mock's curled-ish hand reports a real grab.
        let h0 = pipe.process(&frame, dt).expect("frame 0");
        assert!(
            h0[0].grab_strength > 0.1,
            "raw grab {}",
            h0[0].grab_strength
        );

        // Crank the live deadzone high → grab collapses to 0 on the next frame,
        // with no restart.
        cell.set_grab_deadzone(0.99);
        let h1 = pipe.process(&frame, dt).expect("frame 1");
        assert!(
            h1[0].grab_strength < 1e-6,
            "deadzoned grab {}",
            h1[0].grab_strength
        );
    }

    #[test]
    fn live_tuning_idle_throttle_defaults_off_and_toggles() {
        // Untouched-behavior guard for the idle inference throttle: a freshly
        // built cell must read un-throttled (full rate) — a provider rebuilt
        // mid-Idle is corrected by the per-frame mirror system, not by the
        // constructor — and the flag must round-trip through the atomics.
        let cell = MediaPipeLiveTuning::new(0.05, PipelineConfig::default().depth_calibration_k);
        assert!(!cell.idle_throttle(), "new tuning cells start un-throttled");
        cell.set_idle_throttle(true);
        assert!(cell.idle_throttle());
        cell.set_idle_throttle(false);
        assert!(!cell.idle_throttle());
    }

    #[test]
    fn pick_landmark_outputs_passes_probabilities_through_raw() {
        // The vendored hand_landmark.onnx applies Sigmoid to the presence and
        // handedness heads INSIDE the graph, so the outputs are already
        // probabilities. Selection must pass them through untouched — the old
        // shape-matching code sigmoided them again, squashing every value into
        // [0.5, 0.731] (sigmoid(0.02) ≈ 0.505) so the presence gate could never
        // reject and handedness always read Right.
        let out = vec![
            Tensor {
                data: vec![0.25; 63],
                shape: vec![1, 63],
            },
            Tensor {
                data: vec![0.02],
                shape: vec![1, 1],
            },
            Tensor {
                data: vec![0.85],
                shape: vec![1, 1],
            },
            Tensor {
                data: vec![0.5; 63],
                shape: vec![1, 63],
            },
        ];
        let picked = pick_landmark_outputs(&out).expect("pick");
        assert!(
            (picked.presence - 0.02).abs() < 1e-6,
            "presence {} must be the raw graph output, not re-sigmoided",
            picked.presence
        );
        assert!(
            (picked.handedness - 0.85).abs() < 1e-6,
            "handedness {} must be the raw graph output",
            picked.handedness
        );
        assert!(
            (picked.image[0] - 0.25).abs() < 1e-6,
            "image landmarks come from declared output 0"
        );
        assert!(
            (picked.world[0] - 0.5).abs() < 1e-6,
            "world landmarks come from declared output 3"
        );
    }

    #[test]
    fn pick_landmark_outputs_rejects_unexpected_shapes() {
        // Index-based selection is only safe with the declared layout; anything
        // else (wrong count, transposed order) must error with the shapes seen.
        let missing_world = vec![
            Tensor {
                data: vec![0.0; 63],
                shape: vec![1, 63],
            },
            Tensor {
                data: vec![0.9],
                shape: vec![1, 1],
            },
            Tensor {
                data: vec![0.9],
                shape: vec![1, 1],
            },
        ];
        let err = pick_landmark_outputs(&missing_world).expect_err("3 outputs must be rejected");
        assert!(matches!(err, InferenceError::Run(_)), "{err:?}");

        // Right COUNT, wrong shapes at each index (the scalars and landmark
        // tensors transposed): per-index shape checking must reject this too,
        // not just a wrong tensor count.
        let transposed = vec![
            Tensor {
                data: vec![0.9],
                shape: vec![1, 1],
            },
            Tensor {
                data: vec![0.0; 63],
                shape: vec![1, 63],
            },
            Tensor {
                data: vec![0.0; 63],
                shape: vec![1, 63],
            },
            Tensor {
                data: vec![0.9],
                shape: vec![1, 1],
            },
        ];
        let err = pick_landmark_outputs(&transposed)
            .expect_err("4 outputs with wrong per-index shapes must be rejected");
        assert!(matches!(err, InferenceError::Run(_)), "{err:?}");
    }

    #[test]
    fn low_presence_emits_no_hand_and_frees_the_track_slot() {
        // Phantom-track regression: presence 0.3 is the model saying "no hand
        // in this ROI". Pre-fix, the double sigmoid mapped it to ≈0.574, the
        // 0.5 gate could never reject, and the empty ROI persisted as a
        // phantom track holding a slot (so palm re-detection never ran).
        let (mut pipe, _palm, _lm) = counting_pipeline_with_outputs(spread_landmarks(), 0.3, 0.9);
        let hands = pipe
            .process(&consistent_frame(), Duration::from_millis(33))
            .expect("process");
        assert!(
            hands.is_empty(),
            "presence 0.3 < threshold 0.5 must emit no hand"
        );
        assert!(
            pipe.tracked.is_empty(),
            "low-presence ROI must free its slot so palm re-detection runs next frame"
        );
    }

    #[test]
    fn handedness_probability_below_half_reads_left() {
        // Handedness 0.2 is a confident Left. Pre-fix, sigmoid(0.2) ≈ 0.55 met
        // the `>= 0.5` Right test — every hand read Right.
        let (mut pipe, _palm, _lm) = counting_pipeline_with_outputs(spread_landmarks(), 0.98, 0.2);
        let hands = pipe
            .process(&consistent_frame(), Duration::from_millis(33))
            .expect("process");
        assert_eq!(hands.len(), 1);
        assert_eq!(hands[0].chirality, Chirality::Left);
    }

    #[test]
    fn warp_center_samples_roi_center() {
        // A 4x4 image with a single bright pixel at the centre; an identity-ish
        // ROI centred there should sample bright near the crop centre.
        let mut img = RgbImage::new(4, 4);
        img.put_pixel(2, 2, image::Rgb([255, 255, 255]));
        let roi = RoiRect {
            cx: 2.5 / 4.0,
            cy: 2.5 / 4.0,
            size: 1.0 / 4.0,
            rotation: 0.0,
        };
        let crop = warp_roi(&img, &roi, 8);
        // Centre of the crop maps to ~(2.5,2.5) in the source — near the bright px.
        let c = crop.get_pixel(4, 4);
        assert!(c[0] > 0, "expected non-black centre, got {c:?}");
    }

    /// A `side`² image with smooth per-channel linear gradients (x ramp, y
    /// ramp, diagonal ramp) — content on which a triangle kernel and bilinear
    /// point-sampling agree up to `u8` rounding and edge clamping.
    fn gradient_image(side: u32) -> RgbImage {
        let mut img = RgbImage::new(side, side);
        let denom = dim(side - 1);
        for y in 0..side {
            for x in 0..side {
                let r = byte(dim(x) / denom * 255.0);
                let g = byte(dim(y) / denom * 255.0);
                let b = byte((dim(x) + dim(y)) * 0.5 / denom * 255.0);
                img.put_pixel(x, y, image::Rgb([r, g, b]));
            }
        }
        img
    }

    #[test]
    fn resize_into_matches_triangle_resize_on_gradients() {
        // Pixel-equivalence pin for the reused-buffer resize: on smooth
        // (linear) content the reused bilinear resize and the image crate's
        // FilterType::Triangle oracle must agree to within u8 rounding.
        // Covers the real camera downscale (square side → 192) and the mock
        // tests' upscale (64 → 192, where Triangle IS bilinear). The
        // threshold is mean abs diff < 1.0 across all channels — i.e. the
        // total error budget is below one intensity level per sample.
        for (side, label) in [(640u32, "downscale"), (64u32, "upscale")] {
            let src = gradient_image(side);
            let oracle = resize(&src, PALM_SIZE, PALM_SIZE);
            let mut got = RgbImage::default();
            resize_into(&src, PALM_SIZE, PALM_SIZE, &mut got);
            let mut total_abs_diff = 0u64;
            for (a, b) in oracle.pixels().zip(got.pixels()) {
                for c in 0..3 {
                    total_abs_diff += u64::from(a[c].abs_diff(b[c]));
                }
            }
            let samples = u64::from(PALM_SIZE) * u64::from(PALM_SIZE) * 3;
            // mean < 1.0 ⇔ total < samples (integer form, no float casts).
            assert!(
                total_abs_diff < samples,
                "{label} {side}→{PALM_SIZE}: mean abs diff {total_abs_diff}/{samples} >= 1.0"
            );
        }
    }

    #[test]
    fn resize_into_reused_buffer_matches_fresh_buffer() {
        // A dirty reused buffer (previous frame's pixels, same dimensions)
        // must produce exactly what a fresh buffer does — every pixel is
        // rewritten, no stale content survives.
        let src = gradient_image(640);
        let mut fresh = RgbImage::default();
        resize_into(&src, PALM_SIZE, PALM_SIZE, &mut fresh);
        let mut reused = RgbImage::from_pixel(PALM_SIZE, PALM_SIZE, image::Rgb([7, 77, 177]));
        resize_into(&src, PALM_SIZE, PALM_SIZE, &mut reused);
        assert_eq!(fresh, reused);
    }

    // --- ContentRect::to_content_norm (Phase P3: letterbox unprojection) -----

    #[test]
    fn content_norm_landscape_top_edge_maps_to_zero_y() {
        // 1280×720: content y0 = 280/1280 = 0.21875.
        // Square-norm y = y0 is the content top edge; content-norm y must be 0.0.
        let c = ContentRect::for_frame(1280, 720);
        let p = c.to_content_norm(Vec3::new(0.5, c.y0, 0.0));
        assert!(
            p.y.abs() < 1e-6,
            "content top: sq y={:.5} → content y={:.7} (want 0.0)",
            c.y0,
            p.y
        );
    }

    #[test]
    fn content_norm_landscape_bottom_edge_maps_to_one_y() {
        // 1280×720: content y1 = 1000/1280 = 0.78125.
        // Square-norm y = y1 is the content bottom edge; content-norm y must be 1.0.
        let c = ContentRect::for_frame(1280, 720);
        let p = c.to_content_norm(Vec3::new(0.5, c.y1, 0.0));
        assert!(
            (p.y - 1.0).abs() < 1e-6,
            "content bottom: sq y={:.5} → content y={:.7} (want 1.0)",
            c.y1,
            p.y
        );
    }

    #[test]
    fn content_norm_landscape_x_is_identity() {
        // 1280×720: x0=0, x1=1 (camera fills the full width), so content-norm x
        // equals square-norm x — no horizontal compression to undo.
        let c = ContentRect::for_frame(1280, 720);
        let p = c.to_content_norm(Vec3::new(0.7, 0.5, 0.0));
        assert!(
            (p.x - 0.7).abs() < 1e-6,
            "landscape full-width: x={:.7} (want 0.7)",
            p.x
        );
    }

    #[test]
    fn content_norm_portrait_left_edge_maps_to_zero_x() {
        // 480×640 portrait: bars left/right, content x0 = 80/640 = 0.125.
        // Square-norm x = x0 is the content left edge; content-norm x must be 0.0.
        let c = ContentRect::for_frame(480, 640);
        let p = c.to_content_norm(Vec3::new(c.x0, 0.5, 0.0));
        assert!(
            p.x.abs() < 1e-6,
            "content left: sq x={:.5} → content x={:.7} (want 0.0)",
            c.x0,
            p.x
        );
    }

    #[test]
    fn content_norm_portrait_right_edge_maps_to_one_x() {
        // 480×640 portrait: content x1 = 560/640 = 0.875.
        // Square-norm x = x1 is the content right edge; content-norm x must be 1.0.
        let c = ContentRect::for_frame(480, 640);
        let p = c.to_content_norm(Vec3::new(c.x1, 0.5, 0.0));
        assert!(
            (p.x - 1.0).abs() < 1e-6,
            "content right: sq x={:.5} → content x={:.7} (want 1.0)",
            c.x1,
            p.x
        );
    }

    #[test]
    fn content_norm_square_frame_is_identity() {
        // Square frame → content rect is full [0,1]² → to_content_norm is identity.
        let c = ContentRect::for_frame(720, 720);
        let p = c.to_content_norm(Vec3::new(0.3, 0.7, 99.0));
        assert!((p.x - 0.3).abs() < 1e-6, "x={}", p.x);
        assert!((p.y - 0.7).abs() < 1e-6, "y={}", p.y);
        assert!((p.z - 99.0).abs() < 1e-6, "z passes through: {}", p.z);
    }

    /// Content top edge, unprojected through `to_content_norm`, must map to
    /// `LEAP_Y_MAX_MM` (350 mm).
    ///
    /// Pre-fix (direct square-norm → mm): 1280×720 content top at sq y=0.21875
    /// → `350 − 0.21875 × 310 ≈ 282 mm` (only 56% of the Leap Y range).
    /// Post-fix: `to_content_norm(y=0.21875)` → `y'=0.0` → `350 mm`.
    #[test]
    fn unproject_content_top_reaches_leap_y_max() {
        use crate::input::projection::LEAP_Y_MAX_MM;
        let c = ContentRect::for_frame(1280, 720);
        let p = c.to_content_norm(Vec3::new(0.5, c.y0, 0.0));
        let mm = image_norm_to_leap_mm(p, false);
        assert!(
            (mm.y - LEAP_Y_MAX_MM).abs() < 0.5,
            "content top → {:.1} mm (want {LEAP_Y_MAX_MM:.0}, pre-fix was ~282)",
            mm.y
        );
    }

    /// Content bottom edge, unprojected through `to_content_norm`, must map to
    /// `LEAP_Y_MIN_MM` (40 mm).
    ///
    /// Pre-fix (direct square-norm → mm): 1280×720 content bottom at sq y=0.78125
    /// → `350 − 0.78125 × 310 ≈ 108 mm` (only 56% of the Leap Y range).
    /// Post-fix: `to_content_norm(y=0.78125)` → `y'=1.0` → `40 mm`.
    #[test]
    fn unproject_content_bottom_reaches_leap_y_min() {
        use crate::input::projection::LEAP_Y_MIN_MM;
        let c = ContentRect::for_frame(1280, 720);
        let p = c.to_content_norm(Vec3::new(0.5, c.y1, 0.0));
        let mm = image_norm_to_leap_mm(p, false);
        assert!(
            (mm.y - LEAP_Y_MIN_MM).abs() < 0.5,
            "content bottom → {:.1} mm (want {LEAP_Y_MIN_MM:.0}, pre-fix was ~108)",
            mm.y
        );
    }
}
