//! ROI geometry for the two-stage `BlazePose` pipeline: build the rotated
//! person crop from detector alignment keypoints (or the previous frame's aux
//! landmarks — detect-then-track), and project the landmark model's
//! crop-space output back to square-normalized image space.
//!
//! `MediaPipe`'s `AlignmentPointsRectsCalculator` defines the person box by two
//! alignment points: the box centre (detector keypoint 0 = mid-hip; aux
//! landmark row 33 when tracking) and a point on the circle circumscribing
//! the box (keypoint 1 / aux row 34), so the square side is twice their
//! distance. `RectTransformationCalculator` then expands by 1.25× (both
//! `pose_detection_to_roi.pbtxt` and `pose_landmarks_to_roi.pbtxt` use
//! `scale 1.25, square_long`), and the rotation brings the centre→scale-point
//! vector to vertical (target 90°).
//!
//! Coordinate spaces mirror the hand pipeline: the models run in
//! **square-norm** `[0, 1]²` (the square-padded camera frame); publication
//! converts to **content-norm** (padding bars stripped — the pinned "mask UV
//! space"), via [`ContentRect`].

use std::f32::consts::FRAC_PI_2;

use bevy::math::{Vec2, Vec3};

use super::detector::{sigmoid, PersonDetection};

/// ROI expansion factor (`RectTransformationCalculator scale_x/y: 1.25`).
pub const ROI_EXPANSION: f32 = 1.25;

/// Side length the landmark model consumes (256×256).
pub const LANDMARK_INPUT: f32 = 256.0;

/// Rows in the landmark tensor: 33 published landmarks + 2 aux tracking
/// alignment points (rows 33/34) + 4 unused rows.
pub const LANDMARK_ROWS: usize = 39;

/// Values per landmark row: x, y, z, visibility logit, presence logit.
pub const LANDMARK_VALUES: usize = 5;

/// Aux row holding the tracking ROI centre.
pub const AUX_CENTER_ROW: usize = 33;

/// Aux row holding the tracking ROI circumscribing-circle point.
pub const AUX_SCALE_ROW: usize = 34;

/// Smallest landmark-derived ROI still plausible as a track (normalized
/// square units). When the person leaves the camera the aux points can
/// collapse together while presence stays high on a clamped edge crop; size
/// is the signal the track is unusable — drop it and re-detect.
pub const MIN_TRACK_ROI_SIZE: f32 = 0.05;

/// A rotated square region of interest in normalized image coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoiRect {
    /// Centre x in `[0, 1]`.
    pub cx: f32,
    /// Centre y in `[0, 1]`.
    pub cy: f32,
    /// Side length (square) in normalized units.
    pub size: f32,
    /// Rotation in radians (CCW) aligning the centre→scale-point axis to
    /// vertical.
    pub rotation: f32,
}

/// Build the person ROI from two alignment points (see module docs):
/// centre = `center`, side = `2·|scale_point − center|·`[`ROI_EXPANSION`],
/// rotation brings the centre→scale-point vector to vertical (target 90°,
/// same convention as the hand pipeline's `roi_from_palm`).
#[must_use]
pub fn roi_from_alignment_points(center: Vec2, scale_point: Vec2) -> RoiRect {
    let d = scale_point - center;
    let rotation = FRAC_PI_2 - (-d.y).atan2(d.x);
    RoiRect {
        cx: center.x,
        cy: center.y,
        size: 2.0 * d.length() * ROI_EXPANSION,
        rotation,
    }
}

/// Person ROI from a detector hit: keypoint 0 (mid-hip) is the centre,
/// keypoint 1 (full-body circumscribing point) the scale/rotation reference.
#[must_use]
pub fn roi_from_detection(det: &PersonDetection) -> RoiRect {
    roi_from_alignment_points(det.keypoints[0], det.keypoints[1])
}

/// One decoded landmark row in square-normalized image space, with its
/// activated visibility/presence probabilities.
#[derive(Debug, Clone, Copy, Default)]
pub struct RawBodyLandmark {
    /// Square-norm position; `z` is the model's relative depth scaled by the
    /// ROI size (coarse, not metric).
    pub pos: Vec3,
    /// Visibility probability in `[0, 1]` (sigmoid of the raw logit).
    pub visibility: f32,
    /// Presence probability in `[0, 1]` (sigmoid of the raw logit).
    pub presence: f32,
}

/// Project the landmark model's `[195]` crop-space output back to
/// square-normalized image coordinates (inverse of the ROI warp, mirroring
/// the hand pipeline's `project_landmarks`), activating the visibility and
/// presence logits. Returns a stack array — no allocation on the frame path.
#[must_use]
pub fn project_body_landmarks(raw: &[f32], roi: &RoiRect) -> [RawBodyLandmark; LANDMARK_ROWS] {
    let (sin, cos) = roi.rotation.sin_cos();
    let mut out = [RawBodyLandmark::default(); LANDMARK_ROWS];
    for (i, lm) in out.iter_mut().enumerate() {
        let base = i * LANDMARK_VALUES;
        let lx = raw.get(base).copied().unwrap_or(0.0);
        let ly = raw.get(base + 1).copied().unwrap_or(0.0);
        let lz = raw.get(base + 2).copied().unwrap_or(0.0);
        let vis = raw.get(base + 3).copied().unwrap_or(0.0);
        let pres = raw.get(base + 4).copied().unwrap_or(0.0);
        // Crop pixel → centred unit → scaled by ROI size → rotated → translated.
        let u = (lx / LANDMARK_INPUT - 0.5) * roi.size;
        let v = (ly / LANDMARK_INPUT - 0.5) * roi.size;
        lm.pos = Vec3::new(
            roi.cx + u * cos - v * sin,
            roi.cy + u * sin + v * cos,
            lz / LANDMARK_INPUT * roi.size,
        );
        lm.visibility = sigmoid(vis);
        lm.presence = sigmoid(pres);
    }
    out
}

/// Next-frame tracking ROI from this frame's aux alignment rows (33 centre,
/// 34 scale point) — `MediaPipe`'s `pose_landmarks_to_roi` path, letting
/// tracking frames skip the detector entirely.
#[must_use]
pub fn roi_from_body_landmarks(rows: &[RawBodyLandmark; LANDMARK_ROWS]) -> RoiRect {
    roi_from_alignment_points(
        rows[AUX_CENTER_ROW].pos.truncate(),
        rows[AUX_SCALE_ROW].pos.truncate(),
    )
}

/// True if a landmark-derived ROI is worth carrying into the next frame:
/// centre still inside the camera content (not drifted into a padding bar),
/// finite, and at least [`MIN_TRACK_ROI_SIZE`].
#[must_use]
pub fn roi_trackable(roi: &RoiRect, content: ContentRect) -> bool {
    content.contains(roi.cx, roi.cy) && roi.size.is_finite() && roi.size >= MIN_TRACK_ROI_SIZE
}

/// The camera content rectangle inside the square-padded image, in
/// square-normalized coordinates (adapted from the hand pipeline — see its
/// `ContentRect` for the full rationale: padding bars live *inside* `[0, 1]²`
/// of the square, so bare range tests treat an off-camera person as
/// on-screen).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContentRect {
    /// Left edge (square-norm).
    pub x0: f32,
    /// Top edge (square-norm).
    pub y0: f32,
    /// Right edge (square-norm).
    pub x1: f32,
    /// Bottom edge (square-norm).
    pub y1: f32,
}

impl ContentRect {
    /// Content rect for a `frame_w × frame_h` camera frame square-padded to
    /// its larger side (origin-centred padding, matching `square_pad_into`).
    #[must_use]
    pub fn for_frame(frame_w: u32, frame_h: u32) -> Self {
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

    /// Map a square-normalized point into content-normalized coordinates
    /// (`x' = (x − x0)/(x1 − x0)`, `y'` analog, `z` passes through). This is
    /// the publication step that makes landmark xy live in mask UV space.
    ///
    /// # Invariant
    /// `for_frame` enforces non-zero frame dims, so `x1 > x0` and `y1 > y0`;
    /// the divisions are safe (debug-asserted).
    #[must_use]
    pub fn to_content_norm(self, p: Vec3) -> Vec3 {
        let w = self.x1 - self.x0;
        let h = self.y1 - self.y0;
        debug_assert!(w > 0.0, "content rect has zero width: {self:?}");
        debug_assert!(h > 0.0, "content rect has zero height: {self:?}");
        Vec3::new((p.x - self.x0) / w, (p.y - self.y0) / h, p.z)
    }

    /// Inverse of [`Self::to_content_norm`] for a 2-D point: map
    /// content-normalized `(u, v)` back into square-normalized coordinates.
    /// The mask warp iterates output texels in content space and needs their
    /// square-norm position to invert the ROI transform.
    #[must_use]
    pub fn from_content_norm(self, u: f32, v: f32) -> Vec2 {
        Vec2::new(
            self.x0 + u * (self.x1 - self.x0),
            self.y0 + v * (self.y1 - self.y0),
        )
    }

    /// Whether the square-normalized point `(cx, cy)` lies within the content.
    #[must_use]
    pub fn contains(self, cx: f32, cy: f32) -> bool {
        (self.x0..=self.x1).contains(&cx) && (self.y0..=self.y1).contains(&cy)
    }
}

/// `u32` → `f32` for image dimensions (≤ 65535 for realistic frames).
fn dim(v: u32) -> f32 {
    f32::from(u16::try_from(v).unwrap_or(u16::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    #[test]
    fn alignment_roi_scales_and_centres_on_the_first_point() {
        // Scale point straight above the centre (image y grows downward, so
        // "up" is −y): rotation target is exactly met → 0.
        let roi = roi_from_alignment_points(Vec2::new(0.5, 0.6), Vec2::new(0.5, 0.4));
        assert!((roi.cx - 0.5).abs() < 1e-6);
        assert!((roi.cy - 0.6).abs() < 1e-6);
        // side = 2 × dist(0.2) × 1.25 = 0.5.
        assert!((roi.size - 0.5).abs() < 1e-5, "size={}", roi.size);
        assert!(roi.rotation.abs() < 1e-5, "rot={}", roi.rotation);
    }

    #[test]
    fn alignment_roi_rotates_a_sideways_body_upright() {
        // Scale point to the RIGHT of the centre (a person lying sideways):
        // the crop must rotate 90° to bring them upright.
        let roi = roi_from_alignment_points(Vec2::new(0.4, 0.5), Vec2::new(0.6, 0.5));
        assert!(
            (roi.rotation - FRAC_PI_2).abs() < 1e-5,
            "rot={}",
            roi.rotation
        );
    }

    #[test]
    fn project_centre_landmark_maps_to_roi_centre() {
        let roi = RoiRect {
            cx: 0.5,
            cy: 0.5,
            size: 0.4,
            rotation: 0.0,
        };
        let mut raw = [0.0_f32; LANDMARK_ROWS * LANDMARK_VALUES];
        raw[0] = LANDMARK_INPUT / 2.0; // x = 128 (crop centre)
        raw[1] = LANDMARK_INPUT / 2.0; // y = 128
        raw[3] = 10.0; // visibility logit → sigmoid ≈ 1
        raw[4] = -10.0; // presence logit → sigmoid ≈ 0
        let rows = project_body_landmarks(&raw, &roi);
        assert!((rows[0].pos.x - 0.5).abs() < 1e-5);
        assert!((rows[0].pos.y - 0.5).abs() < 1e-5);
        assert!(rows[0].visibility > 0.99);
        assert!(rows[0].presence < 0.01);
    }

    #[test]
    fn project_offset_landmark_scales_by_roi_size() {
        let roi = RoiRect {
            cx: 0.5,
            cy: 0.5,
            size: 0.4,
            rotation: 0.0,
        };
        let mut raw = [0.0_f32; LANDMARK_ROWS * LANDMARK_VALUES];
        // Crop x = 3/4 width → u = 0.25 → +0.25·0.4 = +0.1 → image x 0.6.
        raw[0] = LANDMARK_INPUT * 0.75;
        raw[1] = LANDMARK_INPUT / 2.0;
        let rows = project_body_landmarks(&raw, &roi);
        assert!((rows[0].pos.x - 0.6).abs() < 1e-5, "x={}", rows[0].pos.x);
        assert!((rows[0].pos.y - 0.5).abs() < 1e-5);
    }

    #[test]
    fn tracking_roi_comes_from_the_aux_rows() {
        let mut rows = [RawBodyLandmark::default(); LANDMARK_ROWS];
        rows[AUX_CENTER_ROW].pos = Vec3::new(0.5, 0.55, 0.0);
        rows[AUX_SCALE_ROW].pos = Vec3::new(0.5, 0.35, 0.0);
        let roi = roi_from_body_landmarks(&rows);
        assert!((roi.cx - 0.5).abs() < 1e-6);
        assert!((roi.cy - 0.55).abs() < 1e-6);
        assert!((roi.size - 0.5).abs() < 1e-5); // 2 × 0.2 × 1.25
        assert!(roi.rotation.abs() < 1e-5);
    }

    #[test]
    fn content_rect_strips_landscape_bars_and_round_trips() {
        // 1280×720 → square side 1280, bars top/bottom: y ∈ [0.21875, 0.78125].
        let content = ContentRect::for_frame(1280, 720);
        assert!((content.y0 - 0.21875).abs() < 1e-6);
        assert!((content.y1 - 0.78125).abs() < 1e-6);
        let p = content.to_content_norm(Vec3::new(0.5, 0.21875, 0.0));
        assert!((p.y - 0.0).abs() < 1e-6);
        // from_content_norm inverts to_content_norm.
        let sq = content.from_content_norm(0.25, 0.75);
        let back = content.to_content_norm(Vec3::new(sq.x, sq.y, 0.0));
        assert!((back.x - 0.25).abs() < 1e-6 && (back.y - 0.75).abs() < 1e-6);
    }

    #[test]
    fn roi_trackable_rejects_offscreen_tiny_and_nonfinite() {
        let content = ContentRect::for_frame(64, 64);
        let ok = RoiRect {
            cx: 0.5,
            cy: 0.5,
            size: 0.4,
            rotation: 0.0,
        };
        assert!(roi_trackable(&ok, content));
        let tiny = RoiRect { size: 0.01, ..ok };
        assert!(!roi_trackable(&tiny, content));
        let offscreen = RoiRect { cx: 1.4, ..ok };
        assert!(!roi_trackable(&offscreen, content));
        let bad = RoiRect {
            size: f32::NAN,
            ..ok
        };
        assert!(!roi_trackable(&bad, content));
    }
}
