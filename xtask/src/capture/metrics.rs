//! Pure per-frame image metrics over captured PNGs (no GPU, no app).
//!
//! These cheap metrics tell the agent *which* frames to open and view; the
//! agent applies the visual judgment (no LLM/vision API). All metrics operate
//! on the decoded `RgbaImage`; means are in 0..=255 channel units.

#![allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "metric sums need usize->f64 for averaging; precision loss is acceptable for image stats"
)]

use image::RgbaImage;
use serde::Serialize;

/// Which area of the frame a region metric covers.
#[derive(Debug, Clone, Copy)]
pub enum Region {
    /// The whole frame.
    Full,
    /// The centre 50% box (excludes a 25% border on every side).
    Center,
}

/// Per-frame metric bundle emitted to `metrics.json`.
#[derive(Debug, Clone, Serialize)]
pub struct FrameMetrics {
    /// Sim-frame index this metric describes.
    pub frame: u32,
    /// Mean RGB over the full frame (0..=255).
    pub full_mean: [f64; 3],
    /// Mean RGB over the centre box (0..=255).
    pub center_mean: [f64; 3],
    /// Global luma standard deviation (uniformity; low = flat frame).
    pub global_std: f64,
    /// Mean absolute per-channel delta vs the previous captured frame, or
    /// `null` for the first frame (frozen-vs-animated signal).
    pub delta_prev: Option<f64>,
}

/// Mean RGB over a region, in 0..=255 channel units.
pub fn region_mean(img: &RgbaImage, region: Region) -> [f64; 3] {
    let (w, h) = img.dimensions();
    let (x0, y0, x1, y1) = match region {
        Region::Full => (0, 0, w, h),
        Region::Center => (w / 4, h / 4, w - w / 4, h - h / 4),
    };
    let mut sum = [0.0_f64; 3];
    let mut count = 0.0_f64;
    for y in y0..y1 {
        for x in x0..x1 {
            let p = img.get_pixel(x, y).0;
            sum[0] += f64::from(p[0]);
            sum[1] += f64::from(p[1]);
            sum[2] += f64::from(p[2]);
            count += 1.0;
        }
    }
    if count == 0.0 {
        return [0.0; 3];
    }
    [sum[0] / count, sum[1] / count, sum[2] / count]
}

/// Global luma standard deviation (Rec. 601 luma), a uniformity measure.
pub fn global_std(img: &RgbaImage) -> f64 {
    let lumas: Vec<f64> = img
        .pixels()
        .map(|p| 0.299 * f64::from(p.0[0]) + 0.587 * f64::from(p.0[1]) + 0.114 * f64::from(p.0[2]))
        .collect();
    if lumas.is_empty() {
        return 0.0;
    }
    let n = lumas.len() as f64;
    let mean = lumas.iter().sum::<f64>() / n;
    let var = lumas.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / n;
    var.sqrt()
}

/// Rec. 601 luma from an already-computed RGB channel-mean triple (0..=255),
/// e.g. [`FrameMetrics::full_mean`]. Same weights as the per-pixel luma used
/// by [`global_std`], applied to a mean instead of every pixel — cheap reuse
/// of a metric the caller has typically already computed. Used by the
/// `--update-baselines` near-zero-luminance guard in `capture.rs` to catch
/// all-black frames (e.g. a headless/backgrounded capture) before they are
/// blessed as a baseline.
pub fn luma_from_mean(mean_rgb: [f64; 3]) -> f64 {
    0.299 * mean_rgb[0] + 0.587 * mean_rgb[1] + 0.114 * mean_rgb[2]
}

/// Mean absolute per-channel difference between two same-size frames, averaged
/// over all four RGBA channels (0..=255). This is a coarse frozen-vs-animated
/// signal (does the frame move at all?), distinct from the RGB-only baseline
/// gate in [`crate::capture::diff`]. Returns `f64::INFINITY` if dimensions
/// differ (caller flags as a hard change).
pub fn frame_mean_abs_delta(a: &RgbaImage, b: &RgbaImage) -> f64 {
    if a.dimensions() != b.dimensions() {
        return f64::INFINITY;
    }
    let mut sum = 0.0_f64;
    let mut count = 0.0_f64;
    for (pa, pb) in a.pixels().zip(b.pixels()) {
        for c in 0..4 {
            sum += (f64::from(pa.0[c]) - f64::from(pb.0[c])).abs();
            count += 1.0;
        }
    }
    if count == 0.0 {
        0.0
    } else {
        sum / count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn solid(w: u32, h: u32, rgb: [u8; 3]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba([rgb[0], rgb[1], rgb[2], 255]))
    }

    #[test]
    fn region_mean_of_solid_image_is_that_color() {
        let img = solid(10, 10, [100, 150, 200]);
        let m = region_mean(&img, Region::Full);
        assert!((m[0] - 100.0).abs() < 0.01);
        assert!((m[1] - 150.0).abs() < 0.01);
        assert!((m[2] - 200.0).abs() < 0.01);
    }

    #[test]
    fn uniform_image_has_zero_std() {
        let img = solid(8, 8, [40, 40, 40]);
        assert!(global_std(&img) < 0.01);
    }

    #[test]
    fn luma_from_mean_matches_rec601_weights() {
        let luma = luma_from_mean([100.0, 150.0, 200.0]);
        // 0.299*100 + 0.587*150 + 0.114*200 = 29.9 + 88.05 + 22.8 = 140.75
        assert!((luma - 140.75).abs() < 0.01);
    }

    #[test]
    fn luma_from_mean_black_is_near_zero() {
        assert!(luma_from_mean([0.0, 0.0, 0.0]).abs() < 0.01);
    }

    #[test]
    fn frame_delta_identical_is_zero_different_is_positive() {
        let a = solid(4, 4, [10, 10, 10]);
        let b = solid(4, 4, [10, 10, 10]);
        let c = solid(4, 4, [20, 10, 10]);
        assert!(frame_mean_abs_delta(&a, &b) < 0.01);
        assert!((frame_mean_abs_delta(&a, &c) - 2.5).abs() < 0.01); // 10/4 channels avg
    }

    #[test]
    fn center_region_excludes_borders() {
        // Border red, center green; the Center region (the 50% box, indices
        // [w/4, w-w/4) = [2, 8) here) is painted entirely green so the metric
        // must read green — proving it samples the center and excludes the red
        // border. (A green patch smaller than the center box would let red leak
        // into the mean and is not a meaningful exclusion test.)
        let mut img = solid(10, 10, [255, 0, 0]);
        for y in 2..8 {
            for x in 2..8 {
                img.put_pixel(x, y, Rgba([0, 255, 0, 255]));
            }
        }
        let m = region_mean(&img, Region::Center);
        assert!(m[1] > m[0]); // green dominates the center
        assert!(m[0] < 1.0); // no red leaked in from the border
    }
}
