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
#![allow(dead_code)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bevy::math::Vec3;
use image::{imageops::FilterType, RgbImage};
use smallvec::SmallVec;

use super::anchors::{generate_palm_anchors, Anchor, PalmAnchorOptions};
use super::capture::Frame;
use super::coords::{image_norm_to_leap_mm, MEDIAPIPE_DEPTH_PROXY_MM};
use super::inference::{HandInference, InferenceError, Tensor};
use super::landmark::{project_landmarks, roi_from_landmarks, roi_from_palm, RoiRect};
use super::palm::{decode_palm_detections, weighted_nms, PalmDecodeOptions};
use super::signals::{
    grab_strength, palm_center, palm_normal, palm_velocity, pinch_strength, HandTracker,
};
use crate::input::hand::{Chirality, Hand, LANDMARK_COUNT};
use crate::input::state::MAX_HANDS;

const PALM_SIZE: u32 = 192;
const LM_SIZE: u32 = 224;

/// Normalized centre distance below which a fresh palm detection is taken to be
/// the *same* hand as an existing track (the merge gate of `MediaPipe`'s
/// `AssociationNormRectCalculator`, expressed as centre distance rather than
/// `IoU`: our palm ROI (scale 2.6, shift −0.5) and landmark-track ROI (scale 2.0,
/// shift −0.1) for one hand can have `IoU` below 0.5, so `IoU` would *duplicate*
/// the hand;
/// their centres are always close, so centre distance matches reliably). On a
/// re-detect the smooth tracked ROI wins; a detection whose centre is within this
/// of a kept ROI is discarded (no duplicate, no identity reset), and only a
/// well-separated detection is added as a new hand. See [`associate`].
const ASSOCIATION_GATE: f32 = 0.25;

/// Smallest landmark-derived ROI that is still plausible as a track.
///
/// When a hand leaves the camera, the landmark model can report high presence
/// on an edge/empty crop with all landmarks collapsed together. The resulting
/// ROI centre may still be in-frame, so size is the signal that the track is no
/// longer usable. Below this, drop the hand and let palm detection reacquire.
const MIN_TRACK_ROI_SIZE: f32 = 0.05;

/// Smallest mean x/y landmark spread that still looks like a hand.
///
/// Line's grab model divides by hand scale; when the landmark model collapses
/// points onto a tiny high-presence cluster, that geometry reads like a full
/// fist and fires an attractor. Reject the pose before deriving grab.
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
    /// it is refreshed each frame from the provider's shared cell.
    pub grab_rest_deadzone: f32,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            mirror: true,
            palm_score_threshold: 0.5,
            presence_threshold: 0.5,
            grab_rest_deadzone: 0.2,
        }
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
}

/// Remap a raw geometric grab so a *relaxed-open* hand reads exactly `0`.
///
/// [`grab_strength`] is calibrated to ideal open-hand geometry (fingers fully
/// extended one hand-scale out → `0`); a real relaxed hand sits slightly curled
/// and landmark noise jitters the fingertips, so the raw signal carries a small
/// positive floor at rest. That floor matters now that the depth proxy makes
/// grab the *sole* attractor driver: Line's decay gate is `grab > 0`, so any
/// positive floor keeps the attractor faintly — and, via the slow attack EMA,
/// increasingly — on even with the hand wide open. Subtracting a rest deadzone
/// and rescaling pins `grab <= deadzone → 0` while a full fist still reaches `1`.
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
    /// Optional live source for [`PipelineConfig::grab_rest_deadzone`], shared
    /// (lock-free `f32` bits) with the provider so a tuning UI can re-tune the
    /// deadzone while the worker thread runs. Refreshed at the top of each
    /// [`Self::process`]; `None` (tests, no UI) leaves the config value in force.
    live_grab_deadzone: Option<Arc<AtomicU32>>,
    /// Diagnostics for the most recent processed frame.
    last_diagnostics: PipelineDiagnostics,
    /// Reused square-pad scratch image (taken out via `mem::take` while
    /// processing so the per-frame methods can borrow it without aliasing
    /// `&mut self`). Avoids a per-frame `RgbImage` allocation.
    square_buf: RgbImage,
    /// Reused ROI-warp crop (`LM_SIZE`²), refilled per landmark stage.
    warp_buf: RgbImage,
    /// Reused palm-stage input tensor (`192²×3` f32), refilled each acquisition.
    palm_input: Tensor,
    /// Reused landmark-stage input tensor (`224²×3` f32), refilled per ROI.
    landmark_input: Tensor,
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
            live_grab_deadzone: None,
            last_diagnostics: PipelineDiagnostics::default(),
            // Scratch buffers, pre-sized so steady-state processing allocates
            // nothing: the input tensors fill to capacity on the first frame and
            // are cleared+refilled thereafter; the images (re)allocate only if the
            // camera frame size changes.
            square_buf: RgbImage::default(),
            warp_buf: RgbImage::new(LM_SIZE, LM_SIZE),
            palm_input: Tensor {
                data: Vec::with_capacity(idx(PALM_SIZE) * idx(PALM_SIZE) * 3),
                shape: vec![1, idx(PALM_SIZE), idx(PALM_SIZE), 3],
            },
            landmark_input: Tensor {
                data: Vec::with_capacity(idx(LM_SIZE) * idx(LM_SIZE) * 3),
                shape: vec![1, idx(LM_SIZE), idx(LM_SIZE), 3],
            },
        }
    }

    /// Attach a shared, lock-free source for the grab rest-deadzone so a tuning
    /// UI on the main thread can re-tune it while this pipeline runs on the
    /// worker thread. The bits are an `f32` (`to_bits`/`from_bits`).
    pub fn set_live_deadzone_source(&mut self, source: Arc<AtomicU32>) {
        self.live_grab_deadzone = Some(source);
    }

    /// Diagnostics for the most recent processed frame.
    #[must_use]
    pub fn diagnostics(&self) -> PipelineDiagnostics {
        self.last_diagnostics
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
        // Pick up any live deadzone re-tune from the provider/UI before this
        // frame derives grab.
        if let Some(src) = &self.live_grab_deadzone {
            self.config.grab_rest_deadzone = f32::from_bits(src.load(Ordering::Relaxed));
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
            if let Some((hand, next_roi)) = self.landmark_for(&square, roi, content, dt)? {
                if roi_trackable(&next_roi, content) {
                    hands.push(hand);
                    next.push(next_roi);
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
        // `resize` allocates its own output (an `image` crate limitation); the
        // input tensor it feeds is the reused scratch (`palm_input`).
        let resized = resize(square, PALM_SIZE, PALM_SIZE);
        fill_nchw_unit(&resized, &mut self.palm_input);
        let out = self.palm.run(&self.palm_input)?;
        let (boxes, scores) = pick_palm_outputs(&out)?;
        let mut dets = weighted_nms(
            decode_palm_detections(
                boxes,
                scores,
                &self.anchors,
                &self.decode,
                self.config.palm_score_threshold,
            ),
            0.3,
        );
        dets.sort_by(|a, b| b.score.total_cmp(&a.score));
        dets.truncate(MAX_HANDS);
        Ok(dets.iter().map(roi_from_palm).collect())
    }

    /// Run the landmark stage for one ROI. Returns the tracked hand and the ROI
    /// to use for it next frame (derived from its landmarks), or `None` if the
    /// model's presence score is below threshold (no hand in this ROI).
    fn landmark_for(
        &mut self,
        square: &RgbImage,
        roi: RoiRect,
        content: ContentRect,
        dt: Duration,
    ) -> Result<Option<(Hand, RoiRect)>, InferenceError> {
        // Warp the ROI into the reused crop buffer, then into the reused input
        // tensor — no per-ROI image/tensor allocation.
        warp_roi_into(square, &roi, LM_SIZE, &mut self.warp_buf);
        fill_nchw_unit(&self.warp_buf, &mut self.landmark_input);
        let out = self.landmark.run(&self.landmark_input)?;
        // World landmarks are selected/shape-checked but not consumed yet — a
        // later phase derives metric hand geometry from them.
        let LandmarkOutputs {
            image: raw_lms,
            presence,
            handedness: handed,
            world: _,
        } = pick_landmark_outputs(&out)?;
        if presence < self.config.presence_threshold {
            return Ok(None);
        }

        let img_landmarks = project_landmarks(raw_lms, &roi);
        if !landmarks_trackable(&img_landmarks, content) {
            return Ok(None);
        }

        // Map every landmark into the Leap-device-mm convention.
        let mut landmarks = [Vec3::ZERO; LANDMARK_COUNT];
        for (dst, src) in landmarks.iter_mut().zip(img_landmarks.iter()) {
            *dst = image_norm_to_leap_mm(*src, self.config.mirror);
        }
        let observed_chirality = if handed >= 0.5 {
            Chirality::Right
        } else {
            Chirality::Left
        };
        let mut palm_pos = image_norm_to_leap_mm(palm_center(&img_landmarks), self.config.mirror);
        // A single webcam has no reliable hand depth, and the landmark model's z
        // is a near-zero relative value rather than a Leap-range depth. Pin z to
        // a fixed mid-range proxy so the Line power model's `5^((−z+350)/160)`
        // term is a constant (~10×) and grab alone drives attractor strength —
        // otherwise z≈0 makes that term ~34× and the attractor sticks on. See
        // [`MEDIAPIPE_DEPTH_PROXY_MM`].
        palm_pos.z = MEDIAPIPE_DEPTH_PROXY_MM;
        // Position-based id with a hysteresis-held chirality, so a spurious
        // per-frame handedness flip neither churns the id nor flickers downstream.
        let assigned = self.tracker.assign(observed_chirality, palm_pos);
        // Finite-difference the palm against its previous-frame position (held by
        // the tracker) over the inter-frame `dt`. A fresh track has no history, so
        // velocity starts at zero on first sighting; `dt == 0` is also zero.
        let velocity = palm_velocity(assigned.prev_pos.unwrap_or(palm_pos), palm_pos, dt);

        // Next frame tracks from these landmarks, skipping palm detection.
        let next_roi = roi_from_landmarks(&img_landmarks);

        Ok(Some((
            Hand {
                id: assigned.id,
                chirality: assigned.chirality,
                palm_position: palm_pos,
                palm_normal: palm_normal(&landmarks, assigned.chirality),
                palm_velocity: velocity,
                pinch_strength: pinch_strength(&img_landmarks),
                // Rest-deadzone the grab so a relaxed-open hand reads exactly 0
                // (otherwise its small positive floor keeps Line's attractor on).
                grab_strength: apply_grab_deadzone(
                    grab_strength(&img_landmarks),
                    self.config.grab_rest_deadzone,
                ),
                landmarks,
            },
            next_roi,
        )))
    }
}

// --- detect-then-track association ---------------------------------------

/// Merge the previous frame's tracked ROIs with fresh palm detections, the way
/// `MediaPipe`'s `AssociationNormRectCalculator` does: **tracked ROIs win.**
///
/// Each kept ROI is the smooth landmark-derived track, never replaced by a jumpy
/// fresh palm ROI. A fresh detection is added (as a new hand) only if its centre
/// is more than [`ASSOCIATION_GATE`] from every already-kept ROI, so an existing
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
            .all(|kept| roi_center_dist(kept, p) > ASSOCIATION_GATE)
        {
            out.push(*p);
        }
    }
    out
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
    (((max_x - min_x) + (max_y - min_y)) * 0.5) >= MIN_TRACK_LANDMARK_SPREAD
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
fn square_pad(frame: &Frame) -> RgbImage {
    let mut img = RgbImage::default();
    square_pad_into(frame, &mut img);
    img
}

fn resize(img: &RgbImage, w: u32, h: u32) -> RgbImage {
    image::imageops::resize(img, w, h, FilterType::Triangle)
}

/// Fill `out` with the NHWC `[1, h, w, 3]` `f32` tensor (RGB in `[0,1]`) of
/// `img`, reusing `out`'s buffers. `data.clear()` keeps capacity, so after the
/// first frame this refills without allocating.
fn fill_nchw_unit(img: &RgbImage, out: &mut Tensor) {
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

/// Allocating convenience wrapper over [`fill_nchw_unit`] (tests/benchmarks).
/// `size` is unused except to document the expected square dimension.
fn to_nchw_unit(img: &RgbImage, size: u32) -> Tensor {
    let n = idx(size);
    let mut out = Tensor {
        data: Vec::with_capacity(n * n * 3),
        shape: Vec::with_capacity(4),
    };
    fill_nchw_unit(img, &mut out);
    out
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
    /// Selected and shape-checked now; consumed by a later phase.
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

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;
    use crate::input::providers::mediapipe::capture::{FrameSource, MockFrameSource};

    fn model(name: &str) -> Box<dyn HandInference> {
        use super::super::inference_ort::OrtInference;
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/models/hand")
            .join(name);
        let bytes = std::fs::read(path).expect("read model");
        Box::new(OrtInference::load(&bytes).expect("load model"))
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
            let mut small = resize(&sq, PALM_SIZE, PALM_SIZE);
            let t_resize = bench(20, &mut || {
                small = resize(&sq, PALM_SIZE, PALM_SIZE);
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
        let t_palm = bench(20, &mut || {
            let _ = palm.run(&palm_in).expect("palm run");
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
        let t_lm = bench(20, &mut || {
            let _ = landmark.run(&lm_in).expect("landmark run");
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
        let t_resize_720 = bench(20, &mut || {
            let _ = resize(&s720, PALM_SIZE, PALM_SIZE);
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
        fn run(&mut self, _input: &Tensor) -> Result<Vec<Tensor>, InferenceError> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(self.outputs.clone())
        }
    }

    /// A plausibly spread mock hand (wrist + MCPs + middle tip placed apart) in
    /// landmark-crop pixels, for tests that need the geometry gates to pass.
    fn spread_landmarks() -> Vec<f32> {
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

    /// Build a pipeline wired with call-counting mocks: palm yields exactly one
    /// detection, landmark yields one high-presence hand. Returns the pipeline
    /// plus the palm and landmark call counters.
    fn counting_pipeline() -> (
        Pipeline,
        std::sync::Arc<std::sync::atomic::AtomicU32>,
        std::sync::Arc<std::sync::atomic::AtomicU32>,
    ) {
        counting_pipeline_with_landmarks(spread_landmarks())
    }

    /// [`counting_pipeline_with_outputs`] with probability-realistic scalars for
    /// a confidently present right hand.
    fn counting_pipeline_with_landmarks(
        lms: Vec<f32>,
    ) -> (
        Pipeline,
        std::sync::Arc<std::sync::atomic::AtomicU32>,
        std::sync::Arc<std::sync::atomic::AtomicU32>,
    ) {
        counting_pipeline_with_outputs(lms, 0.98, 0.9)
    }

    /// Counting-mock pipeline whose landmark stage emits the given landmarks,
    /// presence, and handedness. The mock mirrors the vendored model's declared
    /// output order — image landmarks, presence, handedness, world landmarks —
    /// with the scalars as real probabilities (the model's graph applies the
    /// sigmoid itself).
    fn counting_pipeline_with_outputs(
        lms: Vec<f32>,
        presence: f32,
        handedness: f32,
    ) -> (
        Pipeline,
        std::sync::Arc<std::sync::atomic::AtomicU32>,
        std::sync::Arc<std::sync::atomic::AtomicU32>,
    ) {
        use std::sync::atomic::AtomicU32;
        use std::sync::Arc;

        // Palm: one central stride-8 anchor hot → one 0.2×0.2 detection; all
        // other anchors drop. Keep the mock away from frame edges so tests that
        // expect a healthy hand are not exercising the edge-invalidation path.
        let mut scores = vec![-100.0f32; 2016];
        let hot_anchor = (12 * 24 + 12) * 2;
        scores[hot_anchor] = 100.0;
        let mut boxes = vec![0.0f32; 2016 * 18];
        let hot_box = hot_anchor * 18;
        boxes[hot_box + 2] = 192.0 * 0.2;
        boxes[hot_box + 3] = 192.0 * 0.2;
        boxes[hot_box + 5] = 192.0 * 0.1;
        boxes[hot_box + 9] = -192.0 * 0.1;
        let palm_out = vec![
            Tensor {
                data: boxes,
                shape: vec![1, 2016, 18],
            },
            Tensor {
                data: scores,
                shape: vec![1, 2016, 1],
            },
        ];

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
                data: vec![0.0; 63],
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
            PipelineConfig::default(),
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

    /// A square ROI centred at `(cx, cy)` (size/rotation irrelevant to
    /// association, which matches on centres).
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
        let (mut pipe, _palm, _lm) = counting_pipeline_with_landmarks(vec![112.0f32; 63]);
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

    #[test]
    fn palm_position_reports_the_depth_proxy() {
        // A webcam gives no real hand-Z, so the provider pins palm z to a fixed
        // mid-range proxy (keeps Line's `5^((−z+350)/160)` power term constant so
        // grab — not a bogus z≈0 → ~34× term — drives the attractor).
        let (mut pipe, _palm, _lm) = counting_pipeline();
        let frame = consistent_frame();
        let hands = pipe
            .process(&frame, Duration::from_millis(33))
            .expect("process");
        assert_eq!(hands.len(), 1);
        assert!(
            (hands[0].palm_position.z - MEDIAPIPE_DEPTH_PROXY_MM).abs() < 1e-3,
            "palm z = {} (want {MEDIAPIPE_DEPTH_PROXY_MM})",
            hands[0].palm_position.z
        );
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

    #[test]
    fn live_deadzone_source_overrides_config_grab() {
        // The tuning UI shares an atomic f32-bits cell with the worker pipeline;
        // process() must pick up a re-tune before deriving this frame's grab.
        let (mut pipe, _p, _l) = counting_pipeline();
        let cell = Arc::new(AtomicU32::new(0.0_f32.to_bits()));
        pipe.set_live_deadzone_source(Arc::clone(&cell));
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
        cell.store(0.99_f32.to_bits(), Ordering::Relaxed);
        let h1 = pipe.process(&frame, dt).expect("frame 1");
        assert!(
            h1[0].grab_strength < 1e-6,
            "deadzoned grab {}",
            h1[0].grab_strength
        );
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
}
