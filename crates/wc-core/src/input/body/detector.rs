//! `BlazePose` person-detector post-processing: SSD anchor generation, raw
//! regression decode, and single-person selection.
//!
//! The pose-detection ONNX graph emits raw box/keypoint regressions relative
//! to a fixed anchor grid (no anchor logic in the graph), exactly like the
//! palm detector. This module reproduces `MediaPipe`'s `SsdAnchorsCalculator`
//! for the 224×224 pose model (from `pose_detection_cpu.pbtxt`): 5 layers,
//! strides `[8, 16, 32, 32, 32]`, one square aspect ratio plus one
//! interpolated scale (2 anchors per location per same-stride layer),
//! `fixed_anchor_size` (sizes come from the regression), offsets 0.5 →
//! `28²·2 + 14²·2 + 7²·6 = 2254` anchors, matching the model's
//! `[1, 2254, 12]` boxes and `[1, 2254, 1]` scores outputs. Decode scales are
//! all 224; raw scores are clipped to ±100 then sigmoided.
//!
//! Radiance tracks ONE primary dancer at a time, but a kiosk stage often holds
//! several people, so [`weighted_nms_into`] runs full `MediaPipe` weighted NMS
//! and emits ALL clusters (bounded to [`MAX_PERSON_CANDIDATES`]) rather than
//! only the argmax. Each cluster is one candidate person: the argmax-score
//! seed, score-blended with every detection overlapping it (`IoU` ≥ threshold).
//! The pipeline then applies primary-dancer stickiness / the person-cycle
//! hotkey over that bounded candidate list; publishing still tracks one person.

use bevy::math::Vec2;

/// Number of SSD anchors for the 224×224 pose detector (see module docs).
pub const POSE_ANCHOR_COUNT: usize = 2254;

/// Maximum weighted-NMS person clusters [`weighted_nms_into`] emits per frame.
/// A kiosk stage rarely needs more, and the fixed bound keeps the candidate
/// buffer allocation-free and the stickiness/cycle scans cheap.
pub const MAX_PERSON_CANDIDATES: usize = 4;

/// Keypoints per detection: 0 = mid-hip (ROI centre), 1 = full-body
/// circumscribing-circle point (ROI scale/rotation), 2 = mid-shoulder,
/// 3 = upper-body point (2 and 3 unused by this pipeline).
pub const POSE_KEYPOINTS: usize = 4;

/// Floats per anchor in the raw box tensor: 4 box + `2·POSE_KEYPOINTS`.
pub const POSE_REGRESSION_LEN: usize = 4 + 2 * POSE_KEYPOINTS;

/// Detector model input side in pixels (224×224).
pub const DETECTOR_INPUT: u32 = 224;

/// Regression divisor (`x_scale = y_scale = w_scale = h_scale = 224.0`).
pub const DETECTOR_SCALE: f32 = 224.0;

/// Symmetric clip applied to raw scores before the sigmoid
/// (`score_clipping_thresh: 100.0`).
pub const SCORE_CLIP: f32 = 100.0;

/// One SSD anchor centre, normalized to `[0, 1]` over the model input.
/// `fixed_anchor_size` means width/height are always 1.0, so only the centre
/// is stored; real box sizes come from the regression.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    /// Normalized centre x in `[0, 1]`.
    pub cx: f32,
    /// Normalized centre y in `[0, 1]`.
    pub cy: f32,
}

/// Generate the fixed anchor grid for the 224×224 pose detector (module docs
/// have the parameter provenance). Anchor order matches the model's output
/// rows so regressions can be indexed by anchor.
#[must_use]
pub fn generate_pose_anchors() -> Vec<Anchor> {
    // pose_detection_cpu.pbtxt: strides [8, 16, 32, 32, 32]; consecutive equal
    // strides accumulate anchors at one feature-map resolution — aspect_ratios
    // [1.0] contributes 1 and the interpolated scale contributes 1, so each
    // same-stride layer adds 2 anchors per location.
    const STRIDES: [u32; 5] = [8, 16, 32, 32, 32];
    let mut anchors = Vec::with_capacity(POSE_ANCHOR_COUNT);
    let mut layer = 0;
    while layer < STRIDES.len() {
        let mut anchors_per_location = 0_usize;
        let mut last = layer;
        while last < STRIDES.len() && STRIDES[last] == STRIDES[layer] {
            anchors_per_location += 2;
            last += 1;
        }
        let stride = STRIDES[layer];
        let fm = DETECTOR_INPUT.div_ceil(stride);
        for y in 0..fm {
            // anchor_offset_x/y = 0.5: cell centres.
            let cy = (grid_f32(y) + 0.5) / grid_f32(fm);
            for x in 0..fm {
                let cx = (grid_f32(x) + 0.5) / grid_f32(fm);
                for _ in 0..anchors_per_location {
                    anchors.push(Anchor { cx, cy });
                }
            }
        }
        layer = last;
    }
    anchors
}

/// Axis-aligned rectangle in normalized `[0, 1]` image coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub xmin: f32,
    /// Top edge.
    pub ymin: f32,
    /// Right edge.
    pub xmax: f32,
    /// Bottom edge.
    pub ymax: f32,
}

impl Rect {
    /// Intersection-over-union with another rectangle.
    #[must_use]
    pub fn iou(&self, other: &Rect) -> f32 {
        let ix0 = self.xmin.max(other.xmin);
        let iy0 = self.ymin.max(other.ymin);
        let ix1 = self.xmax.min(other.xmax);
        let iy1 = self.ymax.min(other.ymax);
        let inter = (ix1 - ix0).max(0.0) * (iy1 - iy0).max(0.0);
        let a = (self.xmax - self.xmin).max(0.0) * (self.ymax - self.ymin).max(0.0);
        let b = (other.xmax - other.xmin).max(0.0) * (other.ymax - other.ymin).max(0.0);
        let union = a + b - inter;
        if union <= 0.0 {
            0.0
        } else {
            inter / union
        }
    }
}

/// A decoded person detection in normalized image coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PersonDetection {
    /// Sigmoid confidence in `[0, 1]`.
    pub score: f32,
    /// Bounding box.
    pub bbox: Rect,
    /// The 4 alignment keypoints (see [`POSE_KEYPOINTS`]).
    pub keypoints: [Vec2; POSE_KEYPOINTS],
}

/// Numerically-stable logistic sigmoid.
#[must_use]
pub fn sigmoid(x: f32) -> f32 {
    if x >= 0.0 {
        1.0 / (1.0 + (-x).exp())
    } else {
        let e = x.exp();
        e / (1.0 + e)
    }
}

/// Decode raw model outputs into detections above `score_threshold`, into a
/// reused `out` buffer (`clear()` keeps capacity — the pipeline reuses one
/// buffer across frames, so steady-state decode allocates nothing).
///
/// `raw_boxes` is row-major `[num_anchors, POSE_REGRESSION_LEN]`;
/// `raw_scores` is `[num_anchors]`; both index-align with `anchors`.
/// Centre/keypoint offsets are relative to the anchor centre; sizes are
/// absolute (normalized), all divided by [`DETECTOR_SCALE`].
pub fn decode_pose_detections_into(
    raw_boxes: &[f32],
    raw_scores: &[f32],
    anchors: &[Anchor],
    score_threshold: f32,
    out: &mut Vec<PersonDetection>,
) {
    out.clear();
    for (i, anchor) in anchors.iter().enumerate() {
        let Some(&raw_score) = raw_scores.get(i) else {
            break;
        };
        let score = sigmoid(raw_score.clamp(-SCORE_CLIP, SCORE_CLIP));
        if score < score_threshold {
            continue;
        }
        let base = i * POSE_REGRESSION_LEN;
        let Some(reg) = raw_boxes.get(base..base + POSE_REGRESSION_LEN) else {
            break;
        };
        // Box: centre offset relative to the anchor centre, absolute size.
        let cx = reg[0] / DETECTOR_SCALE + anchor.cx;
        let cy = reg[1] / DETECTOR_SCALE + anchor.cy;
        let w = reg[2] / DETECTOR_SCALE;
        let h = reg[3] / DETECTOR_SCALE;
        let bbox = Rect {
            xmin: cx - w * 0.5,
            ymin: cy - h * 0.5,
            xmax: cx + w * 0.5,
            ymax: cy + h * 0.5,
        };
        let mut keypoints = [Vec2::ZERO; POSE_KEYPOINTS];
        for (k, kp) in keypoints.iter_mut().enumerate() {
            *kp = Vec2::new(
                reg[4 + k * 2] / DETECTOR_SCALE + anchor.cx,
                reg[4 + k * 2 + 1] / DETECTOR_SCALE + anchor.cy,
            );
        }
        out.push(PersonDetection {
            score,
            bbox,
            keypoints,
        });
    }
}

/// Full `MediaPipe` weighted NMS, emitting up to `max_candidates` person
/// clusters into `out` (cleared then refilled — reuse one buffer to stay
/// allocation-free). Each cluster is the argmax-score seed among the
/// not-yet-consumed detections, score-blended with every detection whose `IoU`
/// against the seed is ≥ `iou_threshold`; those detections are then removed
/// from the pool and the next seed is taken. Clusters come out in descending
/// seed-score order (so `out[0]` is the top person).
///
/// `dets` is consumed in place: consumed detections have their score marked
/// with a negative sentinel. The caller's decode buffer is rebuilt every frame,
/// so the mutation is harmless; taking `&mut` avoids a per-frame "used" mask
/// allocation (AGENTS.md hot-path rule).
pub fn weighted_nms_into(
    dets: &mut [PersonDetection],
    iou_threshold: f32,
    max_candidates: usize,
    out: &mut Vec<PersonDetection>,
) {
    out.clear();
    while out.len() < max_candidates {
        // Argmax over the detections not yet consumed (score >= 0).
        let mut seed_idx: Option<usize> = None;
        let mut seed_score = f32::NEG_INFINITY;
        for (i, d) in dets.iter().enumerate() {
            if d.score >= 0.0 && d.score > seed_score {
                seed_score = d.score;
                seed_idx = Some(i);
            }
        }
        let Some(seed_idx) = seed_idx else {
            break; // pool exhausted
        };
        let seed = dets[seed_idx];
        // Blend the seed with every overlapping detection; consume each.
        let mut total = 0.0_f32;
        let mut bbox = Rect {
            xmin: 0.0,
            ymin: 0.0,
            xmax: 0.0,
            ymax: 0.0,
        };
        let mut keypoints = [Vec2::ZERO; POSE_KEYPOINTS];
        for d in dets.iter_mut() {
            if d.score < 0.0 || seed.bbox.iou(&d.bbox) < iou_threshold {
                continue;
            }
            let w = d.score;
            total += w;
            bbox.xmin += d.bbox.xmin * w;
            bbox.ymin += d.bbox.ymin * w;
            bbox.xmax += d.bbox.xmax * w;
            bbox.ymax += d.bbox.ymax * w;
            for (acc, kp) in keypoints.iter_mut().zip(d.keypoints.iter()) {
                *acc += *kp * w;
            }
            d.score = -1.0; // consumed
        }
        if total <= 0.0 {
            // Degenerate scores: emit the seed verbatim (weighted mean is
            // undefined). The seed is already marked consumed above.
            out.push(seed);
            continue;
        }
        let inv = 1.0 / total;
        bbox.xmin *= inv;
        bbox.ymin *= inv;
        bbox.xmax *= inv;
        bbox.ymax *= inv;
        for kp in &mut keypoints {
            *kp *= inv;
        }
        out.push(PersonDetection {
            // The seed is the cluster maximum by construction.
            score: seed.score,
            bbox,
            keypoints,
        });
    }
}

/// Lossless `u32` → `f32` for grid sizes/indices here (all ≤ 224).
fn grid_f32(v: u32) -> f32 {
    f32::from(u16::try_from(v).unwrap_or(u16::MAX))
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions; expect_used is denied workspace-wide for non-test code"
)]
mod tests {
    use super::*;

    #[test]
    fn produces_2254_anchors_for_pose_224() {
        // 28×28×2 (stride 8) + 14×14×2 (stride 16) + 7×7×6 (stride-32 group
        // of three layers) = 2254, matching the model's [1, 2254, 12] output.
        let anchors = generate_pose_anchors();
        assert_eq!(anchors.len(), POSE_ANCHOR_COUNT);
    }

    #[test]
    fn anchor_grid_layout_matches_the_ssd_config() {
        let anchors = generate_pose_anchors();
        // Layer 0: stride 8 → 28×28 grid; first cell centre = 0.5/28.
        assert!(
            (anchors[0].cx - 0.5 / 28.0).abs() < 1e-6,
            "cx={}",
            anchors[0].cx
        );
        assert!(
            (anchors[0].cy - 0.5 / 28.0).abs() < 1e-6,
            "cy={}",
            anchors[0].cy
        );
        // The two stride-8 anchors at a location share a centre.
        assert_eq!(anchors[0], anchors[1]);
        // Stride-8 layer holds 28×28×2 = 1568; index 1568 is the first
        // stride-16 anchor (14×14 grid, centre 0.5/14).
        assert!((anchors[1568].cx - 0.5 / 14.0).abs() < 1e-6);
        assert!((anchors[1568].cy - 0.5 / 14.0).abs() < 1e-6);
        // Stride-16 layer holds 14×14×2 = 392; index 1960 is the first
        // stride-32 anchor (7×7 grid, six anchors per location, centre 0.5/7).
        assert!((anchors[1960].cx - 0.5 / 7.0).abs() < 1e-6);
        assert_eq!(anchors[1960], anchors[1965]);
        // Last anchor: bottom-right of the 7×7 grid.
        let last = anchors[anchors.len() - 1];
        assert!((last.cx - 6.5 / 7.0).abs() < 1e-6);
        assert!((last.cy - 6.5 / 7.0).abs() < 1e-6);
    }

    #[test]
    fn decode_places_box_and_keypoints_at_anchor_for_zero_offsets() {
        let anchor = Anchor { cx: 0.5, cy: 0.5 };
        let mut raw = vec![0.0_f32; POSE_REGRESSION_LEN];
        raw[2] = DETECTOR_SCALE * 0.4; // width 0.4
        raw[3] = DETECTOR_SCALE * 0.4; // height 0.4
        let mut out = Vec::new();
        decode_pose_detections_into(&raw, &[100.0], &[anchor], 0.5, &mut out);
        assert_eq!(out.len(), 1);
        let d = &out[0];
        assert!(d.score > 0.99); // raw 100 → sigmoid ≈ 1
        assert!((d.bbox.xmin - 0.3).abs() < 1e-5, "{:?}", d.bbox);
        assert!((d.bbox.ymax - 0.7).abs() < 1e-5, "{:?}", d.bbox);
        for kp in &d.keypoints {
            assert!((kp.x - 0.5).abs() < 1e-5 && (kp.y - 0.5).abs() < 1e-5);
        }
    }

    #[test]
    fn decode_offsets_keypoints_relative_to_the_anchor() {
        let anchor = Anchor { cx: 0.25, cy: 0.75 };
        let mut raw = vec![0.0_f32; POSE_REGRESSION_LEN];
        // Keypoint 1 (full-body scale point): +0.1 in x, −0.2 in y.
        raw[6] = DETECTOR_SCALE * 0.1;
        raw[7] = -DETECTOR_SCALE * 0.2;
        let mut out = Vec::new();
        decode_pose_detections_into(&raw, &[100.0], &[anchor], 0.5, &mut out);
        let kp1 = out[0].keypoints[1];
        assert!((kp1.x - 0.35).abs() < 1e-5, "kp1={kp1:?}");
        assert!((kp1.y - 0.55).abs() < 1e-5, "kp1={kp1:?}");
    }

    #[test]
    fn decode_drops_below_threshold_scores() {
        let anchor = Anchor { cx: 0.5, cy: 0.5 };
        let raw = vec![0.0_f32; POSE_REGRESSION_LEN];
        let mut out = Vec::new();
        // raw 0 → sigmoid 0.5; threshold 0.6 drops it.
        decode_pose_detections_into(&raw, &[0.0], &[anchor], 0.6, &mut out);
        assert!(out.is_empty());
    }

    fn det(score: f32, x: f32, y: f32, size: f32) -> PersonDetection {
        PersonDetection {
            score,
            bbox: Rect {
                xmin: x,
                ymin: y,
                xmax: x + size,
                ymax: y + size,
            },
            keypoints: [Vec2::new(x, y); POSE_KEYPOINTS],
        }
    }

    #[test]
    fn weighted_nms_blends_the_top_cluster_and_separates_a_far_person() {
        let mut dets = vec![
            det(0.7, 5.0, 5.0, 1.0),   // a second person, far away — own cluster
            det(0.9, 0.0, 0.0, 1.0),   // seed (argmax)
            det(0.8, 0.05, 0.05, 1.0), // overlaps the seed — blended in
        ];
        let mut out = Vec::new();
        weighted_nms_into(&mut dets, 0.3, MAX_PERSON_CANDIDATES, &mut out);
        // Two clusters: the blended top person, then the far one.
        assert_eq!(out.len(), 2, "{out:?}");
        let top = &out[0];
        assert!((top.score - 0.9).abs() < 1e-6);
        assert!(
            top.bbox.xmin > 0.0 && top.bbox.xmin < 0.05,
            "{:?}",
            top.bbox
        );
        assert!(top.keypoints[0].x < 0.05);
        // The far person is a separate cluster carrying its own score.
        assert!((out[1].score - 0.7).abs() < 1e-6);
        assert!(out[1].keypoints[0].x > 4.0, "{:?}", out[1]);
    }

    #[test]
    fn weighted_nms_is_bounded_and_descending() {
        // Five well-separated people, cap 4 → the four highest scorers, in
        // descending score order.
        let mut dets = vec![
            det(0.5, 0.0, 0.0, 0.1),
            det(0.9, 1.0, 0.0, 0.1),
            det(0.6, 2.0, 0.0, 0.1),
            det(0.8, 3.0, 0.0, 0.1),
            det(0.7, 4.0, 0.0, 0.1),
        ];
        let mut out = Vec::new();
        weighted_nms_into(&mut dets, 0.3, MAX_PERSON_CANDIDATES, &mut out);
        assert_eq!(out.len(), MAX_PERSON_CANDIDATES);
        let scores: Vec<f32> = out.iter().map(|d| d.score).collect();
        assert!(
            scores.windows(2).all(|w| w[0] >= w[1]),
            "not descending: {scores:?}"
        );
        assert!((scores[0] - 0.9).abs() < 1e-6);
        // The 0.5 detection is dropped by the cap.
        assert!(scores.iter().all(|&s| (s - 0.5).abs() > 1e-6), "{scores:?}");
    }

    #[test]
    fn weighted_nms_of_empty_emits_nothing() {
        let mut out = vec![det(1.0, 0.0, 0.0, 1.0)]; // pre-filled to prove clear
        weighted_nms_into(&mut [], 0.3, MAX_PERSON_CANDIDATES, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn sigmoid_is_bounded_and_monotonic() {
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-6);
        assert!(sigmoid(10.0) > 0.99);
        assert!(sigmoid(-10.0) < 0.01);
        assert!(sigmoid(1.0) > sigmoid(0.5));
    }
}
