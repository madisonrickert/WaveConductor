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

/// Tunables for the pipeline.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Mirror x (webcam-as-mirror).
    pub mirror: bool,
    /// Minimum palm-detection score to accept.
    pub palm_score_threshold: f32,
    /// Minimum landmark-presence score to keep a hand.
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
    /// threshold), when its centre leaves the frame ([`roi_on_screen`]), or when
    /// the frame is unusable.
    tracked: SmallVec<[RoiRect; MAX_HANDS]>,
    /// Optional live source for [`PipelineConfig::grab_rest_deadzone`], shared
    /// (lock-free `f32` bits) with the provider so a tuning UI can re-tune the
    /// deadzone while the worker thread runs. Refreshed at the top of each
    /// [`Self::process`]; `None` (tests, no UI) leaves the config value in force.
    live_grab_deadzone: Option<Arc<AtomicU32>>,
    /// Diagnostics for the most recent processed frame.
    last_diagnostics: PipelineDiagnostics,
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

        // Square-pad to the larger side so detection coords are aspect-correct.
        let stage_start = Instant::now();
        let square = square_pad(frame);
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
            if let Some((hand, next_roi)) = self.landmark_for(&square, roi, dt)? {
                if roi_on_screen(&next_roi) {
                    hands.push(hand);
                    next.push(next_roi);
                }
            }
            diagnostics.landmark = diagnostics.landmark.saturating_add(stage_start.elapsed());
        }
        self.tracked = next;
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
        let palm_in = to_nchw_unit(&resize(square, PALM_SIZE, PALM_SIZE), PALM_SIZE);
        let out = self.palm.run(&palm_in)?;
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
        dt: Duration,
    ) -> Result<Option<(Hand, RoiRect)>, InferenceError> {
        let crop = warp_roi(square, &roi, LM_SIZE);
        let lm_in = to_nchw_unit(&crop, LM_SIZE);
        let out = self.landmark.run(&lm_in)?;
        let (raw_lms, presence, handed) = pick_landmark_outputs(&out)?;
        if presence < self.config.presence_threshold {
            return Ok(None);
        }

        let img_landmarks = project_landmarks(raw_lms, &roi);
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

/// True if the ROI's centre (the hand's palm in normalized image coordinates) is
/// within the frame. A track whose centre has left the frame is an off-screen
/// hand: drop it rather than let it linger as a drifting phantom (the landmark
/// model can keep reporting high presence on a clamped edge crop).
fn roi_on_screen(roi: &RoiRect) -> bool {
    (0.0..=1.0).contains(&roi.cx) && (0.0..=1.0).contains(&roi.cy)
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

/// Square-pad a frame to its larger side (black bars), origin-centered.
fn square_pad(frame: &Frame) -> RgbImage {
    let side = frame.width.max(frame.height);
    let mut img = RgbImage::new(side, side);
    let ox = (side - frame.width) / 2;
    let oy = (side - frame.height) / 2;
    let w = idx(frame.width);
    for y in 0..frame.height {
        let row = idx(y) * w * 3;
        for x in 0..frame.width {
            let i = row + idx(x) * 3;
            img.put_pixel(
                ox + x,
                oy + y,
                image::Rgb([frame.rgb[i], frame.rgb[i + 1], frame.rgb[i + 2]]),
            );
        }
    }
    img
}

fn resize(img: &RgbImage, w: u32, h: u32) -> RgbImage {
    image::imageops::resize(img, w, h, FilterType::Triangle)
}

/// Convert an RGB image to an NHWC `[1, size, size, 3]` `f32` tensor in `[0,1]`.
fn to_nchw_unit(img: &RgbImage, size: u32) -> Tensor {
    let n = idx(size);
    let mut data = Vec::with_capacity(n * n * 3);
    for p in img.pixels() {
        data.push(f32::from(p[0]) / 255.0);
        data.push(f32::from(p[1]) / 255.0);
        data.push(f32::from(p[2]) / 255.0);
    }
    Tensor {
        data,
        shape: vec![1, n, n, 3],
    }
}

/// Warp the rotated normalized ROI out of `square` into an `out`×`out` RGB crop
/// (bilinear). Inverse-maps each output pixel through the ROI, mirroring
/// [`project_landmarks`].
fn warp_roi(square: &RgbImage, roi: &RoiRect, out: u32) -> RgbImage {
    let side = dim(square.width());
    let (sin, cos) = roi.rotation.sin_cos();
    let mut dst = RgbImage::new(out, out);
    let outf = dim(out);
    for oy in 0..out {
        for ox in 0..out {
            let u = (dim(ox) / outf - 0.5) * roi.size;
            let v = (dim(oy) / outf - 0.5) * roi.size;
            let nx = roi.cx + (u * cos - v * sin);
            let ny = roi.cy + (u * sin + v * cos);
            let px = sample_bilinear(square, nx * side, ny * side);
            dst.put_pixel(ox, oy, px);
        }
    }
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

fn pick_landmark_outputs(out: &[Tensor]) -> Result<(&[f32], f32, f32), InferenceError> {
    // Two [1,63] tensors (image + world landmarks) and two [1,1] scalars
    // (presence, handedness). Image landmarks are output 0; presence is the
    // first scalar, handedness the second (model output order).
    let lms = out
        .iter()
        .find(|t| t.shape == [1, 63])
        .ok_or_else(|| InferenceError::Run("landmark: no [1,63] output".into()))?;
    let scalars: Vec<f32> = out
        .iter()
        .filter(|t| t.shape == [1, 1])
        .map(|t| sigmoid(t.data.first().copied().unwrap_or(0.0)))
        .collect();
    let presence = scalars.first().copied().unwrap_or(0.0);
    let handed = scalars.get(1).copied().unwrap_or(0.5);
    Ok((&lms.data, presence, handed))
}

fn sigmoid(x: f32) -> f32 {
    if x >= 0.0 {
        1.0 / (1.0 + (-x).exp())
    } else {
        let e = x.exp();
        e / (1.0 + e)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;
    use crate::input::providers::mediapipe::capture::{FrameSource, MockFrameSource};

    fn model(name: &str, shape: &[usize]) -> Box<dyn HandInference> {
        use super::super::inference::TractInference;
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/models/hand")
            .join(name);
        let bytes = std::fs::read(path).expect("read model");
        Box::new(TractInference::load(&bytes, shape).expect("load model"))
    }

    fn real_pipeline() -> Pipeline {
        Pipeline::new(
            model("palm_detection.onnx", &[1, 192, 192, 3]),
            model("hand_landmark.onnx", &[1, 224, 224, 3]),
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
    /// `cargo rund` uses (our code at opt-level 1, tract/image at opt-level 3).
    /// Not a correctness test — a measurement harness for the framerate work.
    /// Run with:
    ///   `cargo test -p wc-core --features hand-tracking-mediapipe \
    ///    -- --ignored --nocapture profile_pipeline_stages`
    #[test]
    #[ignore = "measurement harness, not a correctness assertion; run with --nocapture"]
    fn profile_pipeline_stages() {
        use std::time::Instant;

        let mut palm = model("palm_detection.onnx", &[1, 192, 192, 3]);
        let mut landmark = model("hand_landmark.onnx", &[1, 224, 224, 3]);

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

    /// Inference-backend latency comparison: tract (CPU) vs ort (`CoreML`) on the
    /// raw palm + landmark forward passes (data-independent, so a zeros tensor
    /// measures it faithfully). The hard before/after for the GPU-inference move.
    /// Needs the `hand-tracking-mediapipe-ort` feature. Run with:
    ///   `cargo test -p wc-core --features hand-tracking-mediapipe-ort \
    ///    -- --ignored --nocapture profile_inference_backends`
    #[cfg(feature = "hand-tracking-mediapipe-ort")]
    #[test]
    #[ignore = "measurement harness, not a correctness assertion; run with --nocapture"]
    fn profile_inference_backends() {
        use super::super::inference::TractInference;
        use super::super::inference_ort::OrtInference;
        use std::time::Instant;

        let bench = |iters: u32, body: &mut dyn FnMut()| -> f64 {
            body(); // warm-up (the first ort run also compiles the CoreML model)
            let t = Instant::now();
            for _ in 0..iters {
                body();
            }
            (t.elapsed().as_secs_f64() * 1000.0) / f64::from(iters)
        };

        let read = |name: &str| {
            std::fs::read(
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../assets/models/hand")
                    .join(name),
            )
            .expect("read model")
        };
        let palm_bytes = read("palm_detection.onnx");
        let lm_bytes = read("hand_landmark.onnx");

        let palm_in = Tensor {
            data: vec![0.0; idx(PALM_SIZE) * idx(PALM_SIZE) * 3],
            shape: vec![1, idx(PALM_SIZE), idx(PALM_SIZE), 3],
        };
        let lm_in = Tensor {
            data: vec![0.0; idx(LM_SIZE) * idx(LM_SIZE) * 3],
            shape: vec![1, idx(LM_SIZE), idx(LM_SIZE), 3],
        };

        eprintln!("\n=== inference backend comparison (mean ms over 20 iters) ===");

        let mut tract_palm =
            TractInference::load(&palm_bytes, &[1, 192, 192, 3]).expect("tract palm");
        let mut tract_lm =
            TractInference::load(&lm_bytes, &[1, 224, 224, 3]).expect("tract landmark");
        let tp = bench(20, &mut || {
            let _ = tract_palm.run(&palm_in).expect("tract palm run");
        });
        let tl = bench(20, &mut || {
            let _ = tract_lm.run(&lm_in).expect("tract landmark run");
        });
        eprintln!("  tract  (CPU):    palm.run {tp:8.2}   landmark.run {tl:8.2}");

        let mut ort_palm = OrtInference::load(&palm_bytes).expect("ort palm");
        let mut ort_lm = OrtInference::load(&lm_bytes).expect("ort landmark");
        let op = bench(20, &mut || {
            let _ = ort_palm.run(&palm_in).expect("ort palm run");
        });
        let ol = bench(20, &mut || {
            let _ = ort_lm.run(&lm_in).expect("ort landmark run");
        });
        eprintln!("  ort    (CoreML): palm.run {op:8.2}   landmark.run {ol:8.2}");

        // A tracking frame is one landmark pass; an acquisition / re-detect frame
        // is palm + landmark. Show both per-frame budgets per backend.
        eprintln!(
            "  tracking frame:  tract {tl:7.2} ms (~{:.0} fps)   ort {ol:7.2} ms (~{:.0} fps)",
            1000.0 / tl,
            1000.0 / ol,
        );
        eprintln!(
            "  acquire/redetect:tract {:7.2} ms (~{:.0} fps)   ort {:7.2} ms (~{:.0} fps)",
            tp + tl,
            1000.0 / (tp + tl),
            op + ol,
            1000.0 / (op + ol),
        );
        eprintln!(
            "  speedup:         palm {:.1}x   landmark {:.1}x",
            tp / op,
            tl / ol
        );
        eprintln!("========================================================\n");
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

    /// Build a pipeline wired with call-counting mocks: palm yields exactly one
    /// detection, landmark yields one high-presence hand. Returns the pipeline
    /// plus the palm and landmark call counters.
    fn counting_pipeline() -> (
        Pipeline,
        std::sync::Arc<std::sync::atomic::AtomicU32>,
        std::sync::Arc<std::sync::atomic::AtomicU32>,
    ) {
        use std::sync::atomic::AtomicU32;
        use std::sync::Arc;

        // Palm: anchor 0 hot → one 0.2×0.2 detection; all other anchors drop.
        let mut scores = vec![-100.0f32; 2016];
        scores[0] = 100.0;
        let mut boxes = vec![0.0f32; 2016 * 18];
        boxes[2] = 192.0 * 0.2;
        boxes[3] = 192.0 * 0.2;
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

        // Landmark: a spread hand with high presence + handedness.
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
        let lm_out = vec![
            Tensor {
                data: lms,
                shape: vec![1, 63],
            },
            Tensor {
                data: vec![5.0],
                shape: vec![1, 1],
            },
            Tensor {
                data: vec![5.0],
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

    /// A consistent, non-empty frame for driving the pipeline.
    fn consistent_frame() -> Frame {
        Frame {
            width: 64,
            height: 48,
            rgb: vec![128u8; 64 * 48 * 3],
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
        assert!(roi_on_screen(&roi_at(0.5, 0.5)));
        assert!(roi_on_screen(&roi_at(0.0, 1.0)), "edge is still on screen");
        assert!(
            !roi_on_screen(&roi_at(-0.01, 0.5)),
            "palm left the frame on the left"
        );
        assert!(
            !roi_on_screen(&roi_at(0.5, 1.2)),
            "palm left the frame at the bottom"
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
