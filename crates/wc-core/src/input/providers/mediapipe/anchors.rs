//! SSD anchor generation for the `MediaPipe` palm detector.
//!
//! The palm-detection ONNX graph emits *raw* box/keypoint regressions relative
//! to a fixed grid of anchors (the graph contains no anchor logic). This module
//! reproduces `MediaPipe`'s `SsdAnchorsCalculator` for the 192×192 palm model so
//! [`super::palm`] can decode those regressions into image-space detections.
//!
//! For this model the anchors carry only centres (`fixed_anchor_size` → width =
//! height = 1.0); the real box size comes from the regression. The parameters
//! are pinned by the model's output count: strides `[8, 16, 16, 16]` over a 192
//! input with one square aspect ratio plus one interpolated scale yields
//! `24²·2 + 12²·6 = 2016` anchors, matching the `[1, 2016, 18]` output.
//!
//! Foundation module: consumed by the pipeline (plan Phase 8); exercised by
//! tests until then.
#![allow(dead_code)]

/// One SSD anchor. Width/height are 1.0 for the fixed-size palm anchors; the
/// centre is normalized to `[0, 1]` over the model input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    /// Normalized centre x in `[0, 1]`.
    pub cx: f32,
    /// Normalized centre y in `[0, 1]`.
    pub cy: f32,
    /// Anchor width (1.0 for fixed-size palm anchors).
    pub w: f32,
    /// Anchor height (1.0 for fixed-size palm anchors).
    pub h: f32,
}

/// Parameters for [`generate_palm_anchors`]. [`Self::mediapipe_palm_192`] is the
/// configuration for the vendored `palm_detection.onnx`.
#[derive(Debug, Clone)]
pub struct PalmAnchorOptions {
    /// Model input width in pixels.
    pub input_width: u32,
    /// Model input height in pixels.
    pub input_height: u32,
    /// Smallest anchor scale (unused for fixed-size anchors but part of the
    /// canonical calculation that sets the per-location anchor count).
    pub min_scale: f32,
    /// Largest anchor scale.
    pub max_scale: f32,
    /// Per-layer output strides. Consecutive equal strides accumulate anchors
    /// at the same feature-map resolution.
    pub strides: Vec<u32>,
    /// Grid offset applied to each cell centre (`MediaPipe` uses 0.5).
    pub anchor_offset_x: f32,
    /// Grid offset applied to each cell centre (`MediaPipe` uses 0.5).
    pub anchor_offset_y: f32,
}

impl PalmAnchorOptions {
    /// Anchor configuration for the vendored 192×192 palm detector.
    #[must_use]
    pub fn mediapipe_palm_192() -> Self {
        Self {
            input_width: 192,
            input_height: 192,
            min_scale: 0.148_437_5,
            max_scale: 0.75,
            strides: vec![8, 16, 16, 16],
            anchor_offset_x: 0.5,
            anchor_offset_y: 0.5,
        }
    }
}

/// Generate the fixed grid of SSD anchors for the palm detector.
///
/// Mirrors `MediaPipe`'s `SsdAnchorsCalculator` with `aspect_ratios = [1.0]`,
/// `interpolated_scale_aspect_ratio = 1.0`, and `fixed_anchor_size = true`. The
/// returned order matches the model's anchor order so [`super::palm`] can index
/// regressions by anchor.
#[must_use]
pub fn generate_palm_anchors(opts: &PalmAnchorOptions) -> Vec<Anchor> {
    let num_layers = opts.strides.len();
    let mut anchors = Vec::new();
    let mut layer_id = 0;

    while layer_id < num_layers {
        // Accumulate per-location anchor count across consecutive equal strides.
        // aspect_ratios = [1.0] contributes 1 and the interpolated scale
        // contributes 1, so each same-stride layer adds 2.
        let mut anchors_per_location = 0usize;
        let mut last = layer_id;
        while last < num_layers && opts.strides[last] == opts.strides[layer_id] {
            anchors_per_location += 2;
            last += 1;
        }

        let stride = opts.strides[layer_id];
        let fm_h = div_ceil(opts.input_height, stride);
        let fm_w = div_ceil(opts.input_width, stride);

        for y in 0..fm_h {
            let cy = (f32_from(y) + opts.anchor_offset_y) / f32_from(fm_h);
            for x in 0..fm_w {
                let cx = (f32_from(x) + opts.anchor_offset_x) / f32_from(fm_w);
                for _ in 0..anchors_per_location {
                    anchors.push(Anchor {
                        cx,
                        cy,
                        w: 1.0,
                        h: 1.0,
                    });
                }
            }
        }
        layer_id = last;
    }

    anchors
}

/// Ceiling division for positive integers.
fn div_ceil(a: u32, b: u32) -> u32 {
    a.div_ceil(b)
}

/// Lossless `u32` → `f32` for the grid sizes/indices here (all ≤ 192).
fn f32_from(v: u32) -> f32 {
    // Grid dimensions and indices are tiny (≤ 192), well within f32's exact
    // integer range, so this conversion is exact.
    f32::from(u16::try_from(v).unwrap_or(u16::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_2016_anchors_for_palm_192() {
        let anchors = generate_palm_anchors(&PalmAnchorOptions::mediapipe_palm_192());
        // 24×24×2 (stride 8) + 12×12×6 (stride-16 group of three layers) = 2016,
        // matching the model's [1, 2016, 18] output.
        assert_eq!(anchors.len(), 2016);
    }

    #[test]
    fn first_anchor_is_top_left_of_the_stride8_grid() {
        let anchors = generate_palm_anchors(&PalmAnchorOptions::mediapipe_palm_192());
        // Layer 0: stride 8 → 24×24 grid; first cell centre = 0.5/24.
        let a = anchors[0];
        assert!((a.cx - 0.5 / 24.0).abs() < 1e-6, "cx={}", a.cx);
        assert!((a.cy - 0.5 / 24.0).abs() < 1e-6, "cy={}", a.cy);
        assert!((a.w - 1.0).abs() < 1e-6 && (a.h - 1.0).abs() < 1e-6);
    }

    #[test]
    fn stride16_group_starts_after_the_1152_stride8_anchors() {
        let anchors = generate_palm_anchors(&PalmAnchorOptions::mediapipe_palm_192());
        // Stride-8 layer: 24×24×2 = 1152 anchors. Index 1152 is the first
        // stride-16 anchor: 12×12 grid, first cell centre = 0.5/12.
        let a = anchors[1152];
        assert!((a.cx - 0.5 / 12.0).abs() < 1e-6, "cx={}", a.cx);
        assert!((a.cy - 0.5 / 12.0).abs() < 1e-6, "cy={}", a.cy);
    }

    #[test]
    fn last_anchor_is_bottom_right_of_the_stride16_grid() {
        let anchors = generate_palm_anchors(&PalmAnchorOptions::mediapipe_palm_192());
        let a = anchors[anchors.len() - 1];
        assert!((a.cx - 11.5 / 12.0).abs() < 1e-6, "cx={}", a.cx);
        assert!((a.cy - 11.5 / 12.0).abs() < 1e-6, "cy={}", a.cy);
    }

    #[test]
    fn anchors_repeat_per_location() {
        let anchors = generate_palm_anchors(&PalmAnchorOptions::mediapipe_palm_192());
        // The two stride-8 anchors at the first location share a centre.
        assert_eq!(anchors[0], anchors[1]);
        // Within the stride-16 group, the six anchors at a location share a centre.
        assert_eq!(anchors[1152], anchors[1157]);
    }
}
