//! Landmark-stage geometry: build the rotated hand ROI from a palm detection,
//! and project the landmark model's crop-space output back to image space.
//!
//! Between the two model stages, `MediaPipe` crops a rotated square region around
//! the detected palm and feeds it to the landmark model. This module reproduces
//! that ROI (`DetectionsToRects` + `RectTransformation`: rotate so the wrist→
//! middle-MCP axis is vertical, expand 2.6×, shift the centre up by half the
//! box height) and the inverse projection of the resulting landmarks.
//!
//! The constants here were validated against the Python oracle on a real hand:
//! the ROI computed from the reference detection reproduced `MediaPipe`'s, and the
//! cropped region yielded a valid 21-landmark hand (presence 0.72). See the
//! design spec's *Spike results*.
//!
//! Foundation module: consumed by the pipeline (plan Phase 8); exercised by
//! tests until then.

use std::f32::consts::FRAC_PI_2;

use bevy::math::Vec3;

use super::palm::PalmDetection;
use crate::input::hand::{LandmarkIndex, LANDMARK_COUNT};

/// ROI expansion factor applied to the longer detection-box side.
pub const ROI_SCALE: f32 = 2.6;

/// Vertical shift of the ROI centre, in pre-scale box-height units (the centre
/// moves toward the fingers).
pub const ROI_SHIFT_Y: f32 = -0.5;

/// Side length the landmark model consumes (224×224).
pub const LANDMARK_INPUT: f32 = 224.0;

/// A rotated square region of interest in normalized image coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoiRect {
    /// Centre x in `[0, 1]`.
    pub cx: f32,
    /// Centre y in `[0, 1]`.
    pub cy: f32,
    /// Side length (square) in normalized units.
    pub size: f32,
    /// Rotation in radians (CCW), aligning the wrist→middle-MCP axis to vertical.
    pub rotation: f32,
}

/// Compute the rotated hand ROI from a palm detection.
///
/// Rotation comes from palm keypoints 0 (wrist) and 2 (middle-finger MCP); the
/// square side is `max(box_w, box_h) * ROI_SCALE`; the centre is shifted by
/// `ROI_SHIFT_Y * box_h` along the rotated y-axis. Assumes a square input image
/// (the provider square-pads the frame before detection), so no aspect
/// correction is needed.
#[must_use]
pub fn roi_from_palm(det: &PalmDetection) -> RoiRect {
    let cx = (det.bbox.xmin + det.bbox.xmax) * 0.5;
    let cy = (det.bbox.ymin + det.bbox.ymax) * 0.5;
    let w = det.bbox.xmax - det.bbox.xmin;
    let h = det.bbox.ymax - det.bbox.ymin;

    let k0 = det.keypoints[0];
    let k2 = det.keypoints[2];
    // Angle that rotates the k0→k2 vector to vertical (target 90°).
    let rotation = FRAC_PI_2 - (-(k2.y - k0.y)).atan2(k2.x - k0.x);

    // Shift the centre along the rotated frame (shift_x = 0).
    let (sin, cos) = rotation.sin_cos();
    let x_shift = -h * ROI_SHIFT_Y * sin;
    let y_shift = h * ROI_SHIFT_Y * cos;

    RoiRect {
        cx: cx + x_shift,
        cy: cy + y_shift,
        size: w.max(h) * ROI_SCALE,
        rotation,
    }
}

/// ROI expansion factor applied to the landmark bounding box when tracking.
///
/// Larger gives more motion margin between frames (the hand is less likely to
/// leave the crop before the next frame) at the cost of a smaller hand in the
/// 224 crop. `2.0` matches `MediaPipe`'s landmark→rect scale; tune during
/// hardware acceptance if tracking precision or robustness needs it.
pub const TRACK_ROI_SCALE: f32 = 2.0;

/// Vertical shift of the tracking-ROI centre, in pre-scale box-height units,
/// along the rotated (hand) axis toward the fingers — `MediaPipe`'s landmark-path
/// `shift_y`. Keeps the palm centred in the crop so the hand does not walk out of
/// it between frames (a tracking-drift cause). Same sign convention as
/// [`ROI_SHIFT_Y`].
pub const TRACK_ROI_SHIFT_Y: f32 = -0.1;

/// Full-landmark indices that `MediaPipe` keeps when it converts full hand
/// landmarks into the partial list consumed by `HandLandmarksToRectCalculator`.
///
/// Upstream then computes both rotation and bounds on this partial list. The
/// calculator's internal constants `4, 6, 8` are indices **within this partial
/// list**, mapping to full landmarks 5, 9, and 13 (index/middle/ring MCPs).
const TRACK_LANDMARK_INDICES: [usize; 12] = [
    LandmarkIndex::Wrist.as_index(),
    LandmarkIndex::ThumbCmc.as_index(),
    LandmarkIndex::ThumbMcp.as_index(),
    LandmarkIndex::ThumbIp.as_index(),
    LandmarkIndex::IndexMcp.as_index(),
    LandmarkIndex::IndexPip.as_index(),
    LandmarkIndex::MiddleMcp.as_index(),
    LandmarkIndex::MiddlePip.as_index(),
    LandmarkIndex::RingMcp.as_index(),
    LandmarkIndex::RingPip.as_index(),
    LandmarkIndex::PinkyMcp.as_index(),
    LandmarkIndex::PinkyPip.as_index(),
];

/// Full-landmark index for the first outer MCP used by the rotation baseline.
const ROT_REF_INDEX_MCP: usize = LandmarkIndex::IndexMcp.as_index();
/// Full-landmark index for the middle MCP used by the rotation baseline.
const ROT_REF_MIDDLE_MCP: usize = LandmarkIndex::MiddleMcp.as_index();
/// Full-landmark index for the second outer MCP used by the rotation baseline.
const ROT_REF_RING_MCP: usize = LandmarkIndex::RingMcp.as_index();

/// Compute the next-frame tracking ROI directly from this frame's landmarks, so
/// tracking frames can skip the expensive palm-detection stage (`MediaPipe`'s
/// detect-then-track design).
///
/// Rotation aligns the wrist→weighted-mean(index/middle/ring MCP) axis to
/// vertical, matching `MediaPipe`'s partial-landmark path. The square side is the
/// longer side of that same partial landmark set's bounding box measured *in the
/// rotated frame* (so the rotated square tightly bounds the hand), expanded by
/// [`TRACK_ROI_SCALE`]; the centre is that box's centre, shifted toward the
/// fingers by [`TRACK_ROI_SHIFT_Y`] along the rotated axis. `landmarks` are
/// normalized `[0, 1]` image coordinates, as produced by [`project_landmarks`].
#[must_use]
pub fn roi_from_landmarks(landmarks: &[Vec3; LANDMARK_COUNT]) -> RoiRect {
    let wrist = landmarks[LandmarkIndex::Wrist.as_index()];
    // MediaPipe's weighted mean after full→partial mapping:
    // `((index_mcp + ring_mcp) / 2 + middle_mcp) / 2`.
    let ref_pt = ((landmarks[ROT_REF_INDEX_MCP] + landmarks[ROT_REF_RING_MCP]) / 2.0
        + landmarks[ROT_REF_MIDDLE_MCP])
        / 2.0;
    // Bring the wrist→ref axis to vertical (target 90°).
    let rotation = FRAC_PI_2 - (-(ref_pt.y - wrist.y)).atan2(ref_pt.x - wrist.x);

    // Bounding box of MediaPipe's partial tracking landmark set in the ROI's
    // upright frame
    // (each point rotated by -rotation), matching project_landmarks' convention
    // so a crop from this ROI inverts correctly.
    let (sin, cos) = rotation.sin_cos();
    let mut min_u = f32::MAX;
    let mut min_v = f32::MAX;
    let mut max_u = f32::MIN;
    let mut max_v = f32::MIN;
    for idx in TRACK_LANDMARK_INDICES {
        let lm = landmarks[idx];
        let u = lm.x * cos + lm.y * sin;
        let v = -lm.x * sin + lm.y * cos;
        min_u = min_u.min(u);
        max_u = max_u.max(u);
        min_v = min_v.min(v);
        max_v = max_v.max(v);
    }
    // Centre of the rotated box, rotated back into image space, then shifted
    // toward the fingers along the rotated axis (shift_y) — same form as
    // roi_from_palm, using the rotated-frame box height.
    let cu = (min_u + max_u) * 0.5;
    let cv = (min_v + max_v) * 0.5;
    let height = max_v - min_v;
    RoiRect {
        cx: cu * cos - cv * sin - height * TRACK_ROI_SHIFT_Y * sin,
        cy: cu * sin + cv * cos + height * TRACK_ROI_SHIFT_Y * cos,
        size: (max_u - min_u).max(max_v - min_v) * TRACK_ROI_SCALE,
        rotation,
    }
}

/// Project the landmark model's crop-space output back to normalized image
/// coordinates.
///
/// `raw` is the model's `[63]` output: 21 landmarks of `(x, y, z)` in pixels of
/// the `LANDMARK_INPUT`-sized crop. The inverse of the crop transform maps each
/// landmark back into the full (square) image's normalized `[0, 1]` space; `z`
/// is scaled by the ROI size as a coarse depth proxy (hand-Z is best-effort).
#[must_use]
pub fn project_landmarks(raw: &[f32], roi: &RoiRect) -> [Vec3; LANDMARK_COUNT] {
    let (sin, cos) = roi.rotation.sin_cos();
    let mut out = [Vec3::ZERO; LANDMARK_COUNT];
    for (i, lm) in out.iter_mut().enumerate() {
        let base = i * 3;
        let lx = raw.get(base).copied().unwrap_or(0.0);
        let ly = raw.get(base + 1).copied().unwrap_or(0.0);
        let lz = raw.get(base + 2).copied().unwrap_or(0.0);

        // Crop pixel → centered unit → scaled by ROI size → rotated → translated.
        let u = (lx / LANDMARK_INPUT - 0.5) * roi.size;
        let v = (ly / LANDMARK_INPUT - 0.5) * roi.size;
        let rx = u * cos - v * sin;
        let ry = u * sin + v * cos;
        *lm = Vec3::new(roi.cx + rx, roi.cy + ry, lz / LANDMARK_INPUT * roi.size);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::palm::{Rect, PALM_KEYPOINTS};
    use super::*;
    use bevy::math::Vec2;

    fn keypoints_from(pairs: [(f32, f32); PALM_KEYPOINTS]) -> [Vec2; PALM_KEYPOINTS] {
        let mut kp = [Vec2::ZERO; PALM_KEYPOINTS];
        for (k, p) in kp.iter_mut().zip(pairs.iter()) {
            *k = Vec2::new(p.0, p.1);
        }
        kp
    }

    #[test]
    fn roi_matches_oracle_on_the_reference_detection() {
        // Real values from the Python oracle's top detection on the canonical
        // hand image; the oracle's ROI was (0.5781, 0.4110, 0.2648, 1.6086).
        // box center (0.5272, 0.4091), w=h=0.1018 → corners:
        let det = PalmDetection {
            score: 0.846,
            bbox: Rect {
                xmin: 0.5272 - 0.1018 / 2.0,
                ymin: 0.4091 - 0.1018 / 2.0,
                xmax: 0.5272 + 0.1018 / 2.0,
                ymax: 0.4091 + 0.1018 / 2.0,
            },
            keypoints: keypoints_from([
                (0.4793, 0.4031),
                (0.5767, 0.4332),
                (0.5719, 0.4066),
                (0.5652, 0.3829),
                (0.5568, 0.3608),
                (0.5023, 0.4401),
                (0.5405, 0.4588),
            ]),
        };
        let roi = roi_from_palm(&det);
        assert!((roi.cx - 0.5781).abs() < 2e-3, "cx={}", roi.cx);
        assert!((roi.cy - 0.4110).abs() < 2e-3, "cy={}", roi.cy);
        assert!((roi.size - 0.2648).abs() < 2e-3, "size={}", roi.size);
        assert!((roi.rotation - 1.6086).abs() < 2e-3, "rot={}", roi.rotation);
    }

    #[test]
    fn roi_size_is_scaled_long_side() {
        let det = PalmDetection {
            score: 1.0,
            bbox: Rect {
                xmin: 0.4,
                ymin: 0.4,
                xmax: 0.5,
                ymax: 0.7,
            }, // w=0.1, h=0.3 → long=0.3
            keypoints: keypoints_from([
                (0.45, 0.7),
                (0.0, 0.0),
                (0.45, 0.4),
                (0.0, 0.0),
                (0.0, 0.0),
                (0.0, 0.0),
                (0.0, 0.0),
            ]),
        };
        let roi = roi_from_palm(&det);
        assert!(
            (roi.size - 0.3 * ROI_SCALE).abs() < 1e-5,
            "size={}",
            roi.size
        );
    }

    #[test]
    fn rotation_follows_the_k0_k2_axis() {
        // k0→k2 pointing straight up (k2 above k0 in image) → no rotation needed.
        let up = PalmDetection {
            score: 1.0,
            bbox: Rect {
                xmin: 0.4,
                ymin: 0.4,
                xmax: 0.6,
                ymax: 0.6,
            },
            keypoints: keypoints_from([
                (0.5, 0.6),
                (0.0, 0.0),
                (0.5, 0.4),
                (0.0, 0.0),
                (0.0, 0.0),
                (0.0, 0.0),
                (0.0, 0.0),
            ]),
        };
        assert!(
            roi_from_palm(&up).rotation.abs() < 1e-4,
            "up rot={}",
            roi_from_palm(&up).rotation
        );

        // k0→k2 pointing right (horizontal axis, as in the real reference image)
        // → rotate the crop 90° to bring the hand upright.
        let right = PalmDetection {
            score: 1.0,
            bbox: Rect {
                xmin: 0.4,
                ymin: 0.4,
                xmax: 0.6,
                ymax: 0.6,
            },
            keypoints: keypoints_from([
                (0.4, 0.5),
                (0.0, 0.0),
                (0.6, 0.5),
                (0.0, 0.0),
                (0.0, 0.0),
                (0.0, 0.0),
                (0.0, 0.0),
            ]),
        };
        assert!(
            (roi_from_palm(&right).rotation - FRAC_PI_2).abs() < 1e-4,
            "right rot={}",
            roi_from_palm(&right).rotation
        );
    }

    /// Build a 21-landmark hand in normalized image coords with a known
    /// axis-aligned tracking bbox. All landmarks sit at the centre except the
    /// `MediaPipe` partial-tracking landmarks that pin bbox extremes and rotation.
    fn upright_tracking_hand() -> [Vec3; LANDMARK_COUNT] {
        let mut lm = [Vec3::new(0.5, 0.5, 0.0); LANDMARK_COUNT];
        lm[LandmarkIndex::Wrist.as_index()] = Vec3::new(0.5, 0.7, 0.0); // bottom (max y)
        lm[LandmarkIndex::IndexMcp.as_index()] = Vec3::new(0.45, 0.4, 0.0);
        lm[LandmarkIndex::MiddleMcp.as_index()] = Vec3::new(0.5, 0.4, 0.0);
        lm[LandmarkIndex::RingMcp.as_index()] = Vec3::new(0.55, 0.4, 0.0);
        lm[LandmarkIndex::MiddlePip.as_index()] = Vec3::new(0.5, 0.3, 0.0); // top (min y)
        lm[LandmarkIndex::IndexPip.as_index()] = Vec3::new(0.4, 0.5, 0.0); // left (min x)
        lm[LandmarkIndex::PinkyMcp.as_index()] = Vec3::new(0.6, 0.5, 0.0); // right (max x)
        lm
    }

    #[test]
    fn track_roi_uses_mediapipe_partial_landmark_bbox() {
        // Upright partial-tracking hand → no rotation; bbox x∈[0.4,0.6]
        // y∈[0.3,0.7] → long side 0.4. Deliberately put excluded fingertips far
        // outside the hand; MediaPipe's landmark→rect path ignores them.
        let mut hand = upright_tracking_hand();
        hand[LandmarkIndex::ThumbTip.as_index()] = Vec3::new(0.1, 0.9, 0.0);
        hand[LandmarkIndex::MiddleTip.as_index()] = Vec3::new(0.9, 0.0, 0.0);
        hand[LandmarkIndex::PinkyTip.as_index()] = Vec3::new(0.8, 0.95, 0.0);

        let roi = roi_from_landmarks(&hand);
        assert!(roi.rotation.abs() < 1e-4, "rot={}", roi.rotation);
        assert!((roi.cx - 0.5).abs() < 1e-4, "cx={}", roi.cx);
        assert!(
            (roi.cy - 0.46).abs() < 1e-4,
            "cy={} (want 0.46 after shift)",
            roi.cy
        );
        assert!(
            (roi.size - 0.4 * TRACK_ROI_SCALE).abs() < 1e-4,
            "size={}",
            roi.size
        );
    }

    #[test]
    fn track_roi_rotation_uses_partial_indices_mapped_to_mcps() {
        // Upstream constants 4,6,8 are partial-list indices. With full 21
        // landmarks they map to index/middle/ring MCPs (5,9,13), not full
        // landmarks 4,6,8. Put the old full-index interpretation above the wrist
        // and the mapped MCP baseline to the right; the rightward MCP baseline
        // must win and rotate the crop 90°.
        let mut lm = [Vec3::new(0.5, 0.5, 0.0); LANDMARK_COUNT];
        lm[LandmarkIndex::Wrist.as_index()] = Vec3::new(0.5, 0.5, 0.0);
        lm[LandmarkIndex::ThumbTip.as_index()] = Vec3::new(0.5, 0.3, 0.0);
        lm[LandmarkIndex::IndexPip.as_index()] = Vec3::new(0.5, 0.3, 0.0);
        lm[LandmarkIndex::IndexTip.as_index()] = Vec3::new(0.5, 0.3, 0.0);
        lm[LandmarkIndex::IndexMcp.as_index()] = Vec3::new(0.7, 0.5, 0.0);
        lm[LandmarkIndex::MiddleMcp.as_index()] = Vec3::new(0.7, 0.5, 0.0);
        lm[LandmarkIndex::RingMcp.as_index()] = Vec3::new(0.7, 0.5, 0.0);
        assert!(
            (roi_from_landmarks(&lm).rotation - FRAC_PI_2).abs() < 1e-4,
            "rot={}",
            roi_from_landmarks(&lm).rotation
        );
    }

    #[test]
    fn project_center_landmark_maps_to_roi_center() {
        let roi = RoiRect {
            cx: 0.5,
            cy: 0.5,
            size: 0.4,
            rotation: 0.0,
        };
        // One landmark at the crop centre (112,112) → ROI centre.
        let mut raw = [0.0_f32; LANDMARK_COUNT * 3];
        raw[0] = LANDMARK_INPUT / 2.0;
        raw[1] = LANDMARK_INPUT / 2.0;
        let out = project_landmarks(&raw, &roi);
        assert!((out[0].x - 0.5).abs() < 1e-5, "x={}", out[0].x);
        assert!((out[0].y - 0.5).abs() < 1e-5, "y={}", out[0].y);
    }

    #[test]
    fn project_offset_landmark_scales_by_roi_size_unrotated() {
        let roi = RoiRect {
            cx: 0.5,
            cy: 0.5,
            size: 0.4,
            rotation: 0.0,
        };
        let mut raw = [0.0_f32; LANDMARK_COUNT * 3];
        // Crop x = 3/4 width → u = 0.25 → +0.25*0.4 = +0.1 → image x 0.6.
        raw[0] = LANDMARK_INPUT * 0.75;
        raw[1] = LANDMARK_INPUT / 2.0;
        let out = project_landmarks(&raw, &roi);
        assert!((out[0].x - 0.6).abs() < 1e-5, "x={}", out[0].x);
        assert!((out[0].y - 0.5).abs() < 1e-5, "y={}", out[0].y);
    }
}
