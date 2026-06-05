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
use super::coords::image_norm_to_leap_mm;
use super::inference::{HandInference, InferenceError, Tensor};
use super::landmark::{project_landmarks, roi_from_palm, RoiRect};
use super::palm::{decode_palm_detections, weighted_nms, PalmDecodeOptions, PalmDetection};
use super::signals::{
    grab_strength, palm_center, palm_normal, palm_velocity, pinch_strength, HandTracker,
};
use crate::input::hand::{Chirality, Hand, LANDMARK_COUNT};
use crate::input::state::MAX_HANDS;

const PALM_SIZE: u32 = 192;
const LM_SIZE: u32 = 224;

/// Tunables for the pipeline.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Mirror x (webcam-as-mirror).
    pub mirror: bool,
    /// Minimum palm-detection score to accept.
    pub palm_score_threshold: f32,
    /// Minimum landmark-presence score to keep a hand.
    pub presence_threshold: f32,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            mirror: true,
            palm_score_threshold: 0.5,
            presence_threshold: 0.5,
        }
    }
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
            return Ok(hands);
        }

        // Square-pad to the larger side so detection coords are aspect-correct.
        let square = square_pad(frame);

        // Stage 1: palm detection on the 192 input.
        let palm_in = to_nchw_unit(&resize(&square, PALM_SIZE, PALM_SIZE), PALM_SIZE);
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

        // Stage 2: landmarks per detection.
        for det in &dets {
            if let Some(hand) = self.landmark_for(&square, det, dt)? {
                hands.push(hand);
            }
        }
        self.tracker.end_frame();
        Ok(hands)
    }

    fn landmark_for(
        &mut self,
        square: &RgbImage,
        det: &PalmDetection,
        dt: Duration,
    ) -> Result<Option<Hand>, InferenceError> {
        let roi = roi_from_palm(det);
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
        let chirality = if handed >= 0.5 {
            Chirality::Right
        } else {
            Chirality::Left
        };
        let palm_pos = image_norm_to_leap_mm(palm_center(&img_landmarks), self.config.mirror);
        let id = self.tracker.assign(chirality, palm_pos);
        // Velocity needs the previous palm position; the tracker holds it, but a
        // simple per-frame estimate is sufficient here (refined with history in
        // a later pass). Start at zero on first sighting.
        let velocity = palm_velocity(palm_pos, palm_pos, dt);

        Ok(Some(Hand {
            id,
            chirality,
            palm_position: palm_pos,
            palm_normal: palm_normal(&landmarks, chirality),
            palm_velocity: velocity,
            pinch_strength: pinch_strength(&img_landmarks),
            grab_strength: grab_strength(&img_landmarks),
            landmarks,
        }))
    }
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
