//! Palm-detection post-processing: decode raw regressions into detections and
//! reduce overlaps with weighted non-maximum suppression.
//!
//! The palm ONNX graph emits, per [`super::anchors::Anchor`], 18 raw box/keypoint
//! regressions plus one raw score. This module turns those into normalized
//! [`PalmDetection`]s (a box plus the 7 palm keypoints `MediaPipe` predicts) and
//! blends overlapping detections. The decode scales are model constants
//! ([`PalmDecodeOptions::mediapipe_palm_192`]).
//!
//! The 7 keypoints (wrist, index/middle/ring/pinky MCPs, thumb CMC region, and
//! the palm centre) drive the rotated ROI in [`super::landmark`].
//!
//! The pipeline ([`super::pipeline`]) is the primary consumer, through the
//! reused-buffer `_into` forms on its detect path; the allocating wrappers
//! ([`decode_palm_detections`], [`weighted_nms`]) exist for tests and carry
//! per-item `#[allow(dead_code)]`.

use bevy::math::Vec2;

use super::anchors::Anchor;

/// Number of keypoints the palm detector predicts.
pub const PALM_KEYPOINTS: usize = 7;

/// Floats per anchor in the raw box tensor: 4 box + `2·PALM_KEYPOINTS`.
pub const PALM_REGRESSION_LEN: usize = 4 + 2 * PALM_KEYPOINTS;

/// Axis-aligned rectangle in normalized `[0, 1]` image coordinates (corners).
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
        let iw = (ix1 - ix0).max(0.0);
        let ih = (iy1 - iy0).max(0.0);
        let inter = iw * ih;
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

/// A decoded palm detection in normalized image coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct PalmDetection {
    /// Sigmoid confidence in `[0, 1]`.
    pub score: f32,
    /// Bounding box.
    pub bbox: Rect,
    /// The 7 palm keypoints.
    pub keypoints: [Vec2; PALM_KEYPOINTS],
}

impl Default for PalmDetection {
    fn default() -> Self {
        Self {
            score: 0.0,
            bbox: Rect {
                xmin: 0.0,
                ymin: 0.0,
                xmax: 0.0,
                ymax: 0.0,
            },
            keypoints: [Vec2::ZERO; PALM_KEYPOINTS],
        }
    }
}

/// Decode constants for the palm detector.
#[derive(Debug, Clone)]
pub struct PalmDecodeOptions {
    /// Regression divisor for x/width (the model input size for the 192 model).
    pub x_scale: f32,
    /// Regression divisor for y/height.
    pub y_scale: f32,
    /// Symmetric clip applied to raw scores before the sigmoid.
    pub score_clip: f32,
}

impl PalmDecodeOptions {
    /// Decode constants for the vendored 192×192 palm detector.
    #[must_use]
    pub fn mediapipe_palm_192() -> Self {
        Self {
            x_scale: 192.0,
            y_scale: 192.0,
            score_clip: 100.0,
        }
    }
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
/// reused `out` buffer.
///
/// `raw_boxes` is row-major `[num_anchors, PALM_REGRESSION_LEN]`; `raw_scores`
/// is `[num_anchors]`; both index-align with `anchors`. Centre/keypoint offsets
/// are relative to the anchor centre; sizes are absolute (normalized).
///
/// `out` is cleared first (`Vec::clear` keeps capacity), so a caller that
/// reuses one buffer across frames allocates nothing in steady state — the
/// pipeline's detect path requires this (no per-frame allocation in the worker
/// loop).
pub fn decode_palm_detections_into(
    raw_boxes: &[f32],
    raw_scores: &[f32],
    anchors: &[Anchor],
    opts: &PalmDecodeOptions,
    score_threshold: f32,
    out: &mut Vec<PalmDetection>,
) {
    out.clear();
    for (i, anchor) in anchors.iter().enumerate() {
        let Some(&raw_score) = raw_scores.get(i) else {
            break;
        };
        let score = sigmoid(raw_score.clamp(-opts.score_clip, opts.score_clip));
        if score < score_threshold {
            continue;
        }
        let base = i * PALM_REGRESSION_LEN;
        let Some(reg) = raw_boxes.get(base..base + PALM_REGRESSION_LEN) else {
            break;
        };

        // Box: centre offset relative to the anchor centre, absolute size.
        let cx = reg[0] / opts.x_scale + anchor.cx;
        let cy = reg[1] / opts.y_scale + anchor.cy;
        let w = reg[2] / opts.x_scale;
        let h = reg[3] / opts.y_scale;
        let bbox = Rect {
            xmin: cx - w * 0.5,
            ymin: cy - h * 0.5,
            xmax: cx + w * 0.5,
            ymax: cy + h * 0.5,
        };

        let mut keypoints = [Vec2::ZERO; PALM_KEYPOINTS];
        for (k, kp) in keypoints.iter_mut().enumerate() {
            let kx = reg[4 + k * 2] / opts.x_scale + anchor.cx;
            let ky = reg[4 + k * 2 + 1] / opts.y_scale + anchor.cy;
            *kp = Vec2::new(kx, ky);
        }

        out.push(PalmDetection {
            score,
            bbox,
            keypoints,
        });
    }
}

/// Allocating convenience wrapper over [`decode_palm_detections_into`].
///
/// Tests/benchmarks only — the pipeline's steady-state detect path uses the
/// `_into` form with a buffer reused across frames.
#[allow(
    dead_code,
    reason = "test convenience wrapper; the production path uses the _into form"
)]
#[must_use]
pub fn decode_palm_detections(
    raw_boxes: &[f32],
    raw_scores: &[f32],
    anchors: &[Anchor],
    opts: &PalmDecodeOptions,
    score_threshold: f32,
) -> Vec<PalmDetection> {
    let mut out = Vec::new();
    decode_palm_detections_into(
        raw_boxes,
        raw_scores,
        anchors,
        opts,
        score_threshold,
        &mut out,
    );
    out
}

/// Reusable scratch buffers for [`weighted_nms_into`].
///
/// Owned by the caller for the life of a session (the pipeline keeps one) and
/// cleared-and-refilled on every call, so all three capacities persist and
/// steady-state NMS allocates nothing — the worker-loop no-allocation rule.
#[derive(Debug, Default)]
pub struct PalmNmsScratch {
    /// Per-detection "already merged into a cluster" mask.
    used: Vec<bool>,
    /// Indices of the cluster currently being blended (cleared per seed
    /// detection — this was a fresh `vec![i]` per loop iteration before).
    cluster: Vec<usize>,
    /// Blended output detections; swapped into the caller's `dets` on return,
    /// so the old `dets` storage becomes next call's `kept` buffer.
    kept: Vec<PalmDetection>,
}

/// Weighted non-maximum suppression (`MediaPipe`'s "weighted" mode), in place.
///
/// Repeatedly takes the highest-scoring detection, gathers every detection with
/// `IoU ≥ iou_threshold` against it, and blends the cluster's boxes and
/// keypoints into one detection (a score-weighted average), carrying the
/// cluster's maximum score. Blending — rather than plain suppression — is what
/// gives `MediaPipe` its stable palm boxes.
///
/// On return `dets` holds the blended clusters in **non-increasing score
/// order**: seeds are visited in descending score order and every blended
/// cluster carries exactly its seed's score (each other member joined a
/// cluster whose seed outscored it, so the seed is the cluster maximum).
/// Callers may truncate top-k without re-sorting — the pipeline's
/// `acquire_rois` relies on this; pinned by
/// `nms_output_is_sorted_by_descending_score`. `scratch` keeps its buffers
/// for reuse, so a caller looping with the same `dets`/`scratch` pair
/// allocates nothing in steady state.
pub fn weighted_nms_into(
    dets: &mut Vec<PalmDetection>,
    iou_threshold: f32,
    scratch: &mut PalmNmsScratch,
) {
    // sort_unstable: the stable `sort_by` allocates an auxiliary buffer for
    // slices longer than ~20 elements, and a close hand fires well over 20
    // anchors — a per-acquisition-frame allocation. Unstable ordering can
    // differ only between bit-equal scores (`total_cmp`), where either of the
    // tied detections is an equally valid NMS seed.
    dets.sort_unstable_by(|a, b| b.score.total_cmp(&a.score));
    // clear + resize keeps capacity: a reused mask refills without allocating.
    scratch.used.clear();
    scratch.used.resize(dets.len(), false);
    scratch.kept.clear();

    for i in 0..dets.len() {
        if scratch.used[i] {
            continue;
        }
        scratch.cluster.clear();
        scratch.cluster.push(i);
        for j in (i + 1)..dets.len() {
            if !scratch.used[j] && dets[i].bbox.iou(&dets[j].bbox) >= iou_threshold {
                scratch.used[j] = true;
                scratch.cluster.push(j);
            }
        }
        scratch.used[i] = true;
        scratch.kept.push(blend(dets, &scratch.cluster));
    }
    // Hand the result back through `dets`; the old `dets` storage becomes the
    // scratch's `kept` buffer for the next call. Both capacities persist.
    std::mem::swap(dets, &mut scratch.kept);
}

/// Allocating convenience wrapper over [`weighted_nms_into`].
///
/// Tests/benchmarks only — the pipeline's steady-state detect path uses the
/// `_into` form with a [`PalmNmsScratch`] reused across frames.
#[allow(
    dead_code,
    reason = "test convenience wrapper; the production path uses the _into form"
)]
#[must_use]
pub fn weighted_nms(mut dets: Vec<PalmDetection>, iou_threshold: f32) -> Vec<PalmDetection> {
    let mut scratch = PalmNmsScratch::default();
    weighted_nms_into(&mut dets, iou_threshold, &mut scratch);
    dets
}

/// Score-weighted average of a cluster of detections; keeps the max score.
fn blend(dets: &[PalmDetection], cluster: &[usize]) -> PalmDetection {
    let total: f32 = cluster.iter().map(|&k| dets[k].score).sum();
    if total <= 0.0 {
        return dets[cluster[0]].clone();
    }
    let mut bbox = Rect {
        xmin: 0.0,
        ymin: 0.0,
        xmax: 0.0,
        ymax: 0.0,
    };
    let mut keypoints = [Vec2::ZERO; PALM_KEYPOINTS];
    let mut max_score = 0.0_f32;
    for &k in cluster {
        let d = &dets[k];
        let wgt = d.score / total;
        bbox.xmin += d.bbox.xmin * wgt;
        bbox.ymin += d.bbox.ymin * wgt;
        bbox.xmax += d.bbox.xmax * wgt;
        bbox.ymax += d.bbox.ymax * wgt;
        for (acc, kp) in keypoints.iter_mut().zip(d.keypoints.iter()) {
            *acc += *kp * wgt;
        }
        max_score = max_score.max(d.score);
    }
    PalmDetection {
        score: max_score,
        bbox,
        keypoints,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigmoid_is_bounded_and_monotonic() {
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-6);
        assert!(sigmoid(10.0) > 0.99);
        assert!(sigmoid(-10.0) < 0.01);
        assert!(sigmoid(1.0) > sigmoid(0.5));
    }

    #[test]
    fn iou_of_identical_rects_is_one_and_disjoint_is_zero() {
        let a = Rect {
            xmin: 0.0,
            ymin: 0.0,
            xmax: 1.0,
            ymax: 1.0,
        };
        let b = a;
        let c = Rect {
            xmin: 5.0,
            ymin: 5.0,
            xmax: 6.0,
            ymax: 6.0,
        };
        assert!((a.iou(&b) - 1.0).abs() < 1e-6);
        assert!(a.iou(&c).abs() < 1e-6);
    }

    #[test]
    fn decode_places_box_at_anchor_centre_for_zero_offsets() {
        // Zero regression offsets, a small absolute size: the box centres on the
        // anchor and the keypoints sit at the anchor centre.
        let anchor = Anchor {
            cx: 0.5,
            cy: 0.5,
            w: 1.0,
            h: 1.0,
        };
        let opts = PalmDecodeOptions::mediapipe_palm_192();
        let mut raw = vec![0.0; PALM_REGRESSION_LEN];
        raw[2] = opts.x_scale * 0.2; // width 0.2
        raw[3] = opts.y_scale * 0.2; // height 0.2
        let dets = decode_palm_detections(&raw, &[100.0], &[anchor], &opts, 0.5);
        assert_eq!(dets.len(), 1);
        let d = &dets[0];
        assert!((d.bbox.xmin - 0.4).abs() < 1e-5, "{:?}", d.bbox);
        assert!((d.bbox.xmax - 0.6).abs() < 1e-5, "{:?}", d.bbox);
        assert!((d.keypoints[0].x - 0.5).abs() < 1e-5);
        assert!(d.score > 0.99); // raw 100 → sigmoid ≈ 1
    }

    #[test]
    fn decode_drops_below_threshold_scores() {
        let anchor = Anchor {
            cx: 0.5,
            cy: 0.5,
            w: 1.0,
            h: 1.0,
        };
        let opts = PalmDecodeOptions::mediapipe_palm_192();
        let raw = vec![0.0; PALM_REGRESSION_LEN];
        // raw score 0 → sigmoid 0.5; threshold 0.6 drops it.
        let dets = decode_palm_detections(&raw, &[0.0], &[anchor], &opts, 0.6);
        assert!(dets.is_empty());
    }

    fn det(score: f32, x: f32, y: f32, size: f32) -> PalmDetection {
        PalmDetection {
            score,
            bbox: Rect {
                xmin: x,
                ymin: y,
                xmax: x + size,
                ymax: y + size,
            },
            keypoints: [Vec2::new(x, y); PALM_KEYPOINTS],
        }
    }

    #[test]
    fn nms_blends_overlap_and_keeps_separated_cluster() {
        let dets = vec![
            det(0.9, 0.0, 0.0, 1.0),
            det(0.8, 0.05, 0.05, 1.0), // overlaps the first
            det(0.7, 5.0, 5.0, 1.0),   // far away
        ];
        let kept = weighted_nms(dets, 0.3);
        assert_eq!(kept.len(), 2);
        // The blended top cluster carries the cluster's max score (0.9) and a
        // centre between the two overlapping boxes.
        assert!((kept[0].score - 0.9).abs() < 1e-6);
        assert!(kept[0].bbox.xmin > 0.0 && kept[0].bbox.xmin < 0.05);
    }

    #[test]
    fn nms_output_is_sorted_by_descending_score() {
        // Ordering invariant the pipeline's acquire_rois relies on (it
        // truncates to the top MAX_HANDS without re-sorting): seeds are
        // visited in descending score order and every blended cluster carries
        // its seed's (maximal) score, so the output is non-increasing even
        // for deliberately shuffled input with merged clusters.
        let dets = vec![
            det(0.55, 5.0, 5.0, 1.0),
            det(0.9, 0.0, 0.0, 1.0),
            det(0.6, 10.0, 10.0, 1.0),
            det(0.85, 0.05, 0.05, 1.0), // merges into the 0.9 cluster
            det(0.7, 5.05, 5.0, 1.0),   // seeds the cluster that absorbs 0.55
        ];
        let kept = weighted_nms(dets, 0.3);
        assert_eq!(kept.len(), 3);
        for pair in kept.windows(2) {
            assert!(
                pair[0].score >= pair[1].score,
                "NMS output must be non-increasing by score: {} then {}",
                pair[0].score,
                pair[1].score
            );
        }
        // The cluster maxima land in seed order: 0.9, 0.7, 0.6.
        assert!((kept[0].score - 0.9).abs() < 1e-6);
        assert!((kept[1].score - 0.7).abs() < 1e-6);
        assert!((kept[2].score - 0.6).abs() < 1e-6);
    }

    #[test]
    fn nms_scratch_reuse_is_identical_to_fresh_allocation() {
        // The bit-identical bar for the scratch refactor: dirty one
        // `PalmNmsScratch` with a first input, re-run it on a different
        // (larger) input, and require both results to equal the allocating
        // wrapper's (which builds fresh buffers every call).
        let case_a = vec![
            det(0.9, 0.0, 0.0, 1.0),
            det(0.8, 0.05, 0.05, 1.0),
            det(0.7, 5.0, 5.0, 1.0),
        ];
        let case_b = vec![
            det(0.5, 2.0, 2.0, 1.0),
            det(0.95, 0.1, 0.1, 1.0),
            det(0.6, 0.12, 0.1, 1.0),
            det(0.4, 9.0, 9.0, 1.0),
        ];
        let mut scratch = PalmNmsScratch::default();
        let mut a = case_a.clone();
        weighted_nms_into(&mut a, 0.3, &mut scratch); // dirties used/cluster/kept
        let mut b = case_b.clone();
        weighted_nms_into(&mut b, 0.3, &mut scratch);
        assert_eq!(a, weighted_nms(case_a, 0.3));
        assert_eq!(b, weighted_nms(case_b, 0.3));
    }

    #[test]
    fn decode_into_reused_buffer_matches_fresh_allocation() {
        // A reused (dirty, over-capacity) decode buffer must produce exactly
        // what the allocating wrapper does — clear() semantics, no stale tail.
        let anchor = Anchor {
            cx: 0.5,
            cy: 0.5,
            w: 1.0,
            h: 1.0,
        };
        let opts = PalmDecodeOptions::mediapipe_palm_192();
        let mut raw = vec![0.0; PALM_REGRESSION_LEN];
        raw[2] = opts.x_scale * 0.2;
        raw[3] = opts.y_scale * 0.2;
        let mut out = vec![PalmDetection::default(); 7]; // dirty + larger than needed
        decode_palm_detections_into(&raw, &[100.0], &[anchor], &opts, 0.5, &mut out);
        assert_eq!(
            out,
            decode_palm_detections(&raw, &[100.0], &[anchor], &opts, 0.5)
        );
    }
}
