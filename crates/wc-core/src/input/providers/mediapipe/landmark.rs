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
#![allow(dead_code)]

use std::f32::consts::FRAC_PI_2;

use bevy::math::Vec3;

use super::palm::PalmDetection;
use crate::input::hand::LANDMARK_COUNT;

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
