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

use std::time::Duration;

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

/// How often to re-run palm detection while hands are being tracked. Tracking
/// alone (landmark-only) never re-validates against detection, so a spurious
/// detection could otherwise stick forever and block new hands. Re-detecting on
/// this cadence clears phantoms and picks up newly-appeared hands while still
/// skipping palm on the frames in between. Tunable; render-rate smoothing masks
/// the periodic re-detect cost.
const REDETECT_PERIOD: Duration = Duration::from_millis(500);

/// Normalized-image distance within which a fresh palm detection is taken to
/// corroborate an existing track. The same hand's palm ROI and landmark ROI have
/// near-coincident centres (well under this), while two distinct hands sit far
/// further apart, so this both confirms a track and avoids double-counting one
/// hand as two on a re-detect frame.
const REDETECT_MATCH_GATE: f32 = 0.25;

/// Consecutive re-detect frames a track may go uncorroborated by palm before it
/// is dropped. `2` tolerates a single missed palm detection of a real hand — the
/// landmark stage, which is more reliable than palm, keeps tracking it across the
/// gap so the hand never blinks — while still clearing a phantom (a landmark
/// false-positive palm never confirms) within ~`REDETECT_MISS_LIMIT ×
/// REDETECT_PERIOD` (≈1 s).
const REDETECT_MISS_LIMIT: u8 = 2;

/// A tracked hand's next-frame ROI plus how many consecutive re-detect frames it
/// has gone without a corroborating palm detection. The miss count drives
/// phantom clearing (see [`REDETECT_MISS_LIMIT`]); it is `0` while the hand is
/// confirmed and untouched on the tracking frames between re-detects.
#[derive(Debug, Clone, Copy)]
struct TrackedRoi {
    roi: RoiRect,
    misses: u8,
}

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
    /// reads exactly `0`. See [`apply_grab_deadzone`]. Tunable on hardware via
    /// `WAVECONDUCTOR_HAND_GRAB_DEADZONE`.
    pub grab_rest_deadzone: f32,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            mirror: true,
            palm_score_threshold: 0.5,
            presence_threshold: 0.5,
            grab_rest_deadzone: 0.12,
        }
    }
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
    /// Per-hand ROIs (with miss counts) carried to the next frame
    /// (detect-then-track). When non-empty, the next [`Self::process`] reuses
    /// them and skips the palm stage — which is ~2× the landmark stage and the
    /// bulk of the per-frame cost. A track keeps its landmark-derived ROI across
    /// re-detect frames for continuity (see [`reconcile_redetect`]); it is
    /// dropped when its hand is lost (landmark presence falls below threshold),
    /// when the frame is unusable, or when palm fails to corroborate it for
    /// [`REDETECT_MISS_LIMIT`] consecutive re-detects.
    tracked: SmallVec<[TrackedRoi; MAX_HANDS]>,
    /// Time accumulated since palm detection last ran. Drives periodic
    /// re-detection ([`REDETECT_PERIOD`]) so tracking can't stay locked on a
    /// stale/false ROI.
    since_detect: Duration,
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
            since_detect: Duration::ZERO,
        }
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
        let mut hands: SmallVec<[Hand; MAX_HANDS]> = SmallVec::new();
        if !frame.is_consistent() || frame.width == 0 || frame.height == 0 {
            self.tracker.end_frame();
            self.tracked.clear(); // a bad frame breaks tracking → re-acquire next
            self.since_detect = Duration::ZERO;
            return Ok(hands);
        }

        // Square-pad to the larger side so detection coords are aspect-correct.
        let square = square_pad(frame);

        // Detect-then-track: reuse the previous frame's ROIs and skip palm (the
        // dominant per-frame cost) — but re-run palm whenever nothing is tracked
        // OR the re-detect timer has elapsed. Periodic re-detection picks up
        // newly-appeared hands and clears phantoms (a landmark-only track never
        // re-validates, so a false ROI with high presence would otherwise stick
        // forever). On a re-detect frame the fresh palm detections do NOT replace
        // the tracked ROIs — that would pop a steadily-tracked hand to a
        // structurally different palm ROI twice a second, and blink the hand
        // whenever palm momentarily missed it. Instead [`reconcile_redetect`]
        // keeps each track's landmark-derived ROI for continuity, uses palm only
        // to corroborate (reset miss count), tolerate (a real hand the more
        // reliable landmark stage still tracks), or eventually drop (a phantom),
        // and to add genuinely new hands.
        self.since_detect += dt;
        let redetect = self.tracked.is_empty() || self.since_detect >= REDETECT_PERIOD;
        let to_run: SmallVec<[TrackedRoi; MAX_HANDS]> = if redetect {
            self.since_detect = Duration::ZERO;
            let palm_rois = self.acquire_rois(&square)?;
            let tracked = std::mem::take(&mut self.tracked);
            reconcile_redetect(tracked, &palm_rois)
        } else {
            std::mem::take(&mut self.tracked)
        };

        // Run the landmark stage on each ROI; keep the hand and carry its
        // next-frame ROI (derived from this frame's landmarks) plus its miss
        // count when presence holds. An ROI that loses presence is dropped, so if
        // every hand is lost `tracked` ends empty and the next frame re-acquires.
        let mut next: SmallVec<[TrackedRoi; MAX_HANDS]> = SmallVec::new();
        for tr in to_run {
            if let Some((hand, next_roi)) = self.landmark_for(&square, tr.roi, dt)? {
                hands.push(hand);
                next.push(TrackedRoi {
                    roi: next_roi,
                    misses: tr.misses,
                });
            }
        }
        self.tracked = next;
        self.tracker.end_frame();
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
        // Velocity needs the previous palm position; the tracker holds it, but a
        // simple per-frame estimate is sufficient here (refined with history in
        // a later pass). Start at zero on first sighting.
        let velocity = palm_velocity(palm_pos, palm_pos, dt);

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

// --- detect-then-track reconciliation ------------------------------------

/// Reconcile existing tracks with the palm detections from a re-detect frame.
///
/// Existing tracks keep their (landmark-derived) ROI for continuity — they are
/// never replaced by a palm ROI, which would pop a steadily-tracked hand. A palm
/// detection within [`REDETECT_MATCH_GATE`] of a track corroborates it (miss
/// count reset to `0`); an uncorroborated track is tolerated until it has missed
/// [`REDETECT_MISS_LIMIT`] consecutive re-detects, then dropped (phantom
/// clearing). Each palm detection corroborates at most one track; detections
/// that match no track are appended as new tracks. The result is capped at
/// [`MAX_HANDS`], existing tracks taking priority.
fn reconcile_redetect(
    tracked: SmallVec<[TrackedRoi; MAX_HANDS]>,
    palm_rois: &[RoiRect],
) -> SmallVec<[TrackedRoi; MAX_HANDS]> {
    let mut out: SmallVec<[TrackedRoi; MAX_HANDS]> = SmallVec::new();
    // Which palm detections have corroborated a track (so each confirms at most
    // one, and the leftovers become new hands).
    let mut claimed = [false; MAX_HANDS];

    for t in tracked {
        let matched = palm_rois.iter().enumerate().find(|(i, p)| {
            !claimed.get(*i).copied().unwrap_or(true)
                && roi_center_dist(&t.roi, p) <= REDETECT_MATCH_GATE
        });
        if let Some((i, _)) = matched {
            if let Some(slot) = claimed.get_mut(i) {
                *slot = true;
            }
            out.push(TrackedRoi {
                roi: t.roi,
                misses: 0,
            });
        } else if t.misses + 1 < REDETECT_MISS_LIMIT {
            out.push(TrackedRoi {
                roi: t.roi,
                misses: t.misses + 1,
            });
        }
        // else: missed too many consecutive re-detects → dropped (phantom).
    }

    for (i, p) in palm_rois.iter().enumerate() {
        if out.len() >= MAX_HANDS {
            break;
        }
        if !claimed.get(i).copied().unwrap_or(true) {
            out.push(TrackedRoi { roi: *p, misses: 0 });
        }
    }
    out
}

/// Centre-to-centre distance between two ROIs in normalized image units.
fn roi_center_dist(a: &RoiRect, b: &RoiRect) -> f32 {
    (a.cx - b.cx).hypot(a.cy - b.cy)
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
    fn tracking_frame_skips_palm_detection() {
        use std::sync::atomic::Ordering;
        let (mut pipe, palm_calls, lm_calls) = counting_pipeline();
        let frame = consistent_frame();
        // dt well under REDETECT_PERIOD so frame 2 tracks rather than re-detects.
        let dt = Duration::from_millis(33);

        // Frame 1 acquires: palm detection runs, then the landmark stage.
        let h1 = pipe.process(&frame, dt).expect("frame 1");
        assert_eq!(h1.len(), 1, "frame 1 should acquire one hand");
        assert_eq!(
            palm_calls.load(Ordering::Relaxed),
            1,
            "palm runs to acquire"
        );
        assert_eq!(lm_calls.load(Ordering::Relaxed), 1);

        // Frame 2 tracks: it reuses frame 1's ROI and must NOT re-run palm.
        let h2 = pipe.process(&frame, dt).expect("frame 2");
        assert_eq!(h2.len(), 1, "frame 2 should track the hand");
        assert_eq!(
            palm_calls.load(Ordering::Relaxed),
            1,
            "tracking frame must skip palm detection"
        );
        assert_eq!(
            lm_calls.load(Ordering::Relaxed),
            2,
            "landmark runs every frame"
        );
    }

    #[test]
    fn redetects_after_the_interval() {
        use std::sync::atomic::Ordering;
        let (mut pipe, palm_calls, _lm) = counting_pipeline();
        let frame = consistent_frame();
        // Each step's dt is below REDETECT_PERIOD (500ms), but two accumulate
        // past it. Frame 1 acquires (palm #1); frame 2 tracks (no palm); frame 3
        // crosses the interval → palm re-runs. This is what prevents a spurious
        // ROI from sticking forever and lets new hands be picked up.
        let dt = Duration::from_millis(400);

        pipe.process(&frame, dt).expect("frame 1 (acquire)");
        assert_eq!(palm_calls.load(Ordering::Relaxed), 1, "acquire runs palm");

        pipe.process(&frame, dt).expect("frame 2 (track)");
        assert_eq!(
            palm_calls.load(Ordering::Relaxed),
            1,
            "still within the re-detect interval → track only"
        );

        pipe.process(&frame, dt).expect("frame 3 (re-detect)");
        assert_eq!(
            palm_calls.load(Ordering::Relaxed),
            2,
            "interval elapsed → palm re-runs"
        );
    }

    /// A square ROI centred at `(cx, cy)` (size/rotation irrelevant to
    /// reconciliation, which matches on centres only).
    fn roi_at(cx: f32, cy: f32) -> RoiRect {
        RoiRect {
            cx,
            cy,
            size: 0.3,
            rotation: 0.0,
        }
    }

    /// Build a track set from `(cx, cy, misses)` triples.
    fn tracks(items: &[(f32, f32, u8)]) -> SmallVec<[TrackedRoi; MAX_HANDS]> {
        items
            .iter()
            .map(|&(cx, cy, misses)| TrackedRoi {
                roi: roi_at(cx, cy),
                misses,
            })
            .collect()
    }

    #[test]
    fn redetect_corroborated_track_keeps_its_own_roi() {
        // A track that had missed once; palm now sees the same hand slightly
        // offset (within the gate). Its ROI must NOT be replaced by the palm ROI
        // (continuity — no twice-a-second pop), and the miss count resets.
        let out = reconcile_redetect(tracks(&[(0.5, 0.5, 1)]), &[roi_at(0.55, 0.52)]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].misses, 0, "corroboration resets the miss count");
        assert!(
            (out[0].roi.cx - 0.5).abs() < 1e-6 && (out[0].roi.cy - 0.5).abs() < 1e-6,
            "keeps the track's own landmark ROI, not the palm ROI",
        );
    }

    #[test]
    fn redetect_tolerates_one_miss_then_drops_phantom() {
        // First uncorroborated re-detect: tolerated so a real hand the landmark
        // stage still tracks does not blink.
        let out = reconcile_redetect(tracks(&[(0.5, 0.5, 0)]), &[]);
        assert_eq!(out.len(), 1, "a single palm miss must not drop the hand");
        assert_eq!(out[0].misses, 1);
        // Second consecutive miss: dropped (phantom cleared within ~1 s).
        let out2 = reconcile_redetect(out, &[]);
        assert!(out2.is_empty(), "dropped after REDETECT_MISS_LIMIT misses");
    }

    #[test]
    fn redetect_adds_unmatched_palm_as_new_hand() {
        // Existing hand near (0.3,0.3); palm sees it plus a second hand far off.
        let out = reconcile_redetect(
            tracks(&[(0.3, 0.3, 0)]),
            &[roi_at(0.3, 0.31), roi_at(0.8, 0.8)],
        );
        assert_eq!(out.len(), 2, "existing hand corroborated + new hand added");
        assert!(out.iter().any(|t| (t.roi.cx - 0.8).abs() < 1e-6));
    }

    #[test]
    fn redetect_from_empty_acquires_all_detections() {
        let out = reconcile_redetect(SmallVec::new(), &[roi_at(0.2, 0.2), roi_at(0.7, 0.7)]);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|t| t.misses == 0));
    }

    #[test]
    fn redetect_far_palm_does_not_corroborate() {
        // Track at (0.2,0.2); a palm at (0.7,0.2) is well beyond the gate, so it
        // must not corroborate — the track accrues a miss and the far palm is
        // added as its own new hand.
        let out = reconcile_redetect(tracks(&[(0.2, 0.2, 0)]), &[roi_at(0.7, 0.2)]);
        assert_eq!(out.len(), 2);
        let track = out
            .iter()
            .find(|t| (t.roi.cx - 0.2).abs() < 1e-6)
            .expect("original track kept");
        assert_eq!(track.misses, 1, "a far palm must not corroborate the track");
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
        assert!(apply_grab_deadzone(0.5, 1.5) < 1e-6, "degenerate deadzone clamps");
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
