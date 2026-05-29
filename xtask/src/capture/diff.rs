//! Tolerance-based baseline diff between a captured frame and its baseline.
//!
//! Not a pixel-perfect gate: GPU/driver float differences make exact matching
//! brittle. We report mean per-pixel absolute difference and the percentage of
//! pixels whose max-channel delta exceeds a per-pixel threshold; a frame passes
//! when its mean abs diff is within tolerance. The agent reviews flagged frames.

#![allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "diff sums need usize->f64 for averaging; precision loss is acceptable for image stats"
)]

use image::RgbaImage;
use serde::Serialize;

/// Diff verdict for one captured frame vs its baseline.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct FrameDiff {
    /// Mean per-channel absolute difference (0..=255); `INFINITY` on size
    /// mismatch.
    pub mean_abs_diff: f64,
    /// Percentage of pixels whose max-channel delta exceeds `threshold`.
    pub pct_over_threshold: f64,
}

impl FrameDiff {
    /// Whether this frame is within `tolerance` (max acceptable mean abs diff).
    pub fn passes(&self, tolerance: f64) -> bool {
        self.mean_abs_diff <= tolerance
    }
}

/// Compute the diff between a captured frame and its baseline. `threshold` is
/// the per-pixel max-channel delta (0..=255) above which a pixel is "changed".
pub fn diff_frames(current: &RgbaImage, baseline: &RgbaImage, threshold: u8) -> FrameDiff {
    if current.dimensions() != baseline.dimensions() {
        return FrameDiff {
            mean_abs_diff: f64::INFINITY,
            pct_over_threshold: 100.0,
        };
    }
    let mut sum = 0.0_f64;
    let mut channels = 0.0_f64;
    let mut changed_pixels = 0.0_f64;
    let mut total_pixels = 0.0_f64;
    let thresh = f64::from(threshold);
    for (pc, pb) in current.pixels().zip(baseline.pixels()) {
        let mut max_delta = 0.0_f64;
        for c in 0..3 {
            let d = (f64::from(pc.0[c]) - f64::from(pb.0[c])).abs();
            sum += d;
            channels += 1.0;
            if d > max_delta {
                max_delta = d;
            }
        }
        total_pixels += 1.0;
        if max_delta > thresh {
            changed_pixels += 1.0;
        }
    }
    let mean_abs_diff = if channels == 0.0 { 0.0 } else { sum / channels };
    let pct_over_threshold = if total_pixels == 0.0 {
        0.0
    } else {
        100.0 * changed_pixels / total_pixels
    };
    FrameDiff {
        mean_abs_diff,
        pct_over_threshold,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn solid(rgb: [u8; 3]) -> RgbaImage {
        RgbaImage::from_pixel(8, 8, Rgba([rgb[0], rgb[1], rgb[2], 255]))
    }

    #[test]
    fn identical_images_diff_zero() {
        let a = solid([50, 50, 50]);
        let d = diff_frames(&a, &a, 10);
        assert!(d.mean_abs_diff < 0.01);
        assert!(d.pct_over_threshold < 0.01);
    }

    #[test]
    fn known_delta_matches_expected() {
        let a = solid([10, 10, 10]);
        let b = solid([20, 10, 10]); // +10 on R only -> mean over 3 ch = 10/3
        let d = diff_frames(&a, &b, 5);
        assert!((d.mean_abs_diff - (10.0 / 3.0)).abs() < 0.01);
        assert!((d.pct_over_threshold - 100.0).abs() < 0.01); // every pixel changed (R by 10 > 5)
    }

    #[test]
    fn tolerance_boundary_passes_and_fails() {
        let a = solid([10, 10, 10]);
        let b = solid([12, 10, 10]); // small change
        let d = diff_frames(&a, &b, 5);
        assert!(d.passes(2.0)); // mean ~0.67 < 2.0 tolerance
        assert!(!d.passes(0.1)); // mean ~0.67 > 0.1 tolerance
    }

    #[test]
    fn size_mismatch_is_max_diff() {
        let a = solid([10, 10, 10]);
        let b = RgbaImage::from_pixel(4, 4, Rgba([10, 10, 10, 255]));
        let d = diff_frames(&a, &b, 5);
        assert!(d.mean_abs_diff.is_infinite());
        assert!(!d.passes(1000.0));
    }
}
