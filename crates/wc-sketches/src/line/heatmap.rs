//! Heatmap-image particle spawn sampler.
//!
//! Loads a PNG, computes a per-pixel spawn weight (`luminance × alpha` reshaped
//! by the per-image [`TemplateAdjustments`] luminance remap), builds a CDF over
//! the resulting weights, and returns `count` sampled particles — each a
//! window-space (top-left origin, +y down) position transformed by the
//! adjustments plus the source image's RGB at that point.
//!
//! Direct port of v4's `src/sketches/line/heatmapSampler.ts`, extended with the
//! per-image adjustments. Used by [`crate::line::systems::spawn::spawn_line`]
//! (and the in-place re-seed) when
//! [`crate::line::settings::LineSettings::spawn_template`] is non-empty.
//!
//! ## Determinism
//!
//! The inverse-CDF sampler uses a fixed-seed RNG so particle *i* draws the same
//! uniform each call: nudging an adjustment knob shifts the existing layout
//! continuously instead of respraying every particle. This is what makes live
//! tuning smooth (see the re-seed system).
//!
//! ## Failure mode
//!
//! Any error along the path (file missing, undecodable image, all-zero weight)
//! falls back to a horizontal mid-line + sawtooth Y-jitter identical in shape to
//! the default `spawn_line` layout (with a white spawn colour = no tint) — a
//! broken template never breaks the sketch. The error is logged at `warn`.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "u32 ↔ usize ↔ f32 casts for image dimensions and CDF indices are intentional and bounded"
)]

use std::path::Path;

use bevy::math::Vec2;
use rand::{rngs::StdRng, Rng, SeedableRng};

use crate::line::template_adjustments::{remap_weight, transform_point, TemplateAdjustments};

/// Maximum sampled-grid dimension. Larger images are bilinearly downsampled
/// to this resolution before the CDF is built — a 256×256 grid (65,536 bins)
/// is plenty for spawn-density purposes and keeps the CDF allocation under a
/// megabyte regardless of source image size.
const MAX_SAMPLE_DIM: u32 = 256;

/// Fixed RNG seed so the sampler is deterministic across calls (stable tuning).
const SAMPLE_SEED: u64 = 0x5EED_5EED_5EED_5EED;

/// One sampled particle: a window-space position and the source-image RGB at
/// that point (white `[255,255,255]` from the fallback layout = no colour tint).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SampledParticle {
    /// Window-space position (top-left origin, +y down), post-transform.
    pub pos: Vec2,
    /// Source image RGB at the sampled bin.
    pub color: [u8; 3],
}

/// Sample `count` particles from a brightness heatmap image, reshaped by `adj`.
///
/// `path` — image file (anything the `image` crate decodes).
/// `canvas_w` / `canvas_h` — pixel dimensions of the target rendering canvas.
/// `count` — number of particles to sample.
/// `adj` — per-image adjustments (luminance remap + position/scale transform).
///
/// Returns exactly `count` [`SampledParticle`]s in window space. On any error or
/// a fully-zero weight grid, falls back to the private `fallback_line` helper.
#[must_use]
pub fn sample_from_heatmap(
    path: &Path,
    canvas_w: f32,
    canvas_h: f32,
    count: usize,
    adj: &TemplateAdjustments,
) -> Vec<SampledParticle> {
    match try_sample_from_heatmap(path, canvas_w, canvas_h, count, adj) {
        Ok(positions) => positions,
        Err(err) => {
            tracing::warn!(
                ?err,
                path = %path.display(),
                "heatmap sample failed; falling back to horizontal line"
            );
            fallback_line(canvas_w, canvas_h, count)
        }
    }
}

/// Inner fallible sampler. Errors bubble up to [`sample_from_heatmap`] which
/// converts them to the fallback path.
fn try_sample_from_heatmap(
    path: &Path,
    canvas_w: f32,
    canvas_h: f32,
    count: usize,
    adj: &TemplateAdjustments,
) -> Result<Vec<SampledParticle>, image::ImageError> {
    let img = image::open(path)?;

    // Downsample to keep the CDF small.
    let sample_w = (canvas_w.min(MAX_SAMPLE_DIM as f32).max(1.0)) as u32;
    let sample_h = (canvas_h.min(MAX_SAMPLE_DIM as f32).max(1.0)) as u32;
    let img = img.resize_exact(sample_w, sample_h, image::imageops::FilterType::Triangle);
    let rgba = img.to_rgba8();

    // Build the cumulative distribution over the remapped per-pixel weight, and
    // keep each bin's RGB so a chosen bin yields its source colour. Luminance
    // coefficients (0.299, 0.587, 0.114) are the standard Rec.601 conversion.
    let total_pixels = (sample_w * sample_h) as usize;
    let mut cdf: Vec<f64> = Vec::with_capacity(total_pixels);
    let mut colors: Vec<[u8; 3]> = Vec::with_capacity(total_pixels);
    let mut cumulative = 0.0_f64;
    for px in rgba.pixels() {
        let r = f32::from(px[0]);
        let g = f32::from(px[1]);
        let b = f32::from(px[2]);
        let a = f32::from(px[3]) / 255.0;
        let luminance = 0.299 * r + 0.587 * g + 0.114 * b;
        cumulative += f64::from(remap_weight(luminance, a, adj));
        cdf.push(cumulative);
        colors.push([px[0], px[1], px[2]]);
    }

    // All-zero remapped weights: nothing to sample. Hand back the fallback.
    if cumulative == 0.0 {
        return Ok(fallback_line(canvas_w, canvas_h, count));
    }

    // Inverse-CDF sampling with a fixed seed (deterministic), then transform the
    // window-space position by the adjustments.
    let mut rng = StdRng::seed_from_u64(SAMPLE_SEED);
    let scale_x = canvas_w / sample_w as f32;
    let scale_y = canvas_h / sample_h as f32;
    let canvas = Vec2::new(canvas_w, canvas_h);
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let target = rng.random::<f64>() * cumulative;
        let idx = cdf.partition_point(|&c| c < target).min(total_pixels - 1);
        let px = (idx as u32) % sample_w;
        let py = (idx as u32) / sample_w;
        let x = (px as f32 + rng.random::<f32>()) * scale_x;
        let y = (py as f32 + rng.random::<f32>()) * scale_y;
        out.push(SampledParticle {
            pos: transform_point(Vec2::new(x, y), canvas, adj),
            color: colors[idx],
        });
    }
    Ok(out)
}

/// Fallback layout: a horizontal mid-line with five-strand sawtooth Y-jitter and
/// a white spawn colour (no tint). Matches the shape of the default `spawn_line`
/// layout so a broken template degrades gracefully. Window-space coordinates.
fn fallback_line(canvas_w: f32, canvas_h: f32, count: usize) -> Vec<SampledParticle> {
    let mid_y = canvas_h * 0.5;
    (0..count)
        .map(|i| {
            let x = (i as f32 / count.max(1) as f32) * canvas_w;
            let jitter_strand = (i % 5) as f32 - 2.0;
            let y = mid_y + jitter_strand * 2.0;
            SampledParticle {
                pos: Vec2::new(x, y),
                color: [255, 255, 255],
            }
        })
        .collect()
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "expect with a clear message is appropriate in test code"
)]
mod tests {
    use super::*;

    #[test]
    fn fallback_line_returns_exactly_count_positions() {
        assert_eq!(fallback_line(1280.0, 720.0, 100).len(), 100);
    }

    #[test]
    fn fallback_line_spans_canvas_width() {
        let positions = fallback_line(1280.0, 720.0, 100);
        let xs: Vec<f32> = positions.iter().map(|p| p.pos.x).collect();
        assert!(xs[0] >= 0.0 && xs[0] < 50.0);
        assert!(*xs.last().expect("non-empty positions vec") > 1200.0);
    }

    #[test]
    fn fallback_line_jitter_stays_near_mid_y() {
        let positions = fallback_line(1280.0, 720.0, 50);
        let mid_y = 360.0_f32;
        for p in &positions {
            assert!((p.pos.y - mid_y).abs() <= 4.0_f32 + 0.001);
        }
    }

    #[test]
    fn missing_file_falls_back_to_horizontal_line() {
        let path = Path::new("/this/path/does/not/exist.png");
        let positions =
            sample_from_heatmap(path, 1280.0, 720.0, 64, &TemplateAdjustments::default());
        assert_eq!(positions.len(), 64);
        let mid_y = 360.0_f32;
        for p in &positions {
            assert!((p.pos.y - mid_y).abs() <= 4.0_f32 + 0.001);
        }
    }

    #[test]
    fn zero_count_returns_empty() {
        assert!(fallback_line(1280.0, 720.0, 0).is_empty());
    }

    fn write_png(path: &std::path::Path, f: impl Fn(u32, u32) -> [u8; 4]) {
        let (w, h) = (32_u32, 8_u32);
        let mut img = image::RgbaImage::new(w, h);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = image::Rgba(f(x, y));
        }
        img.save(path).expect("save png");
    }

    #[test]
    fn sampler_is_deterministic() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("g.png");
        write_png(&path, |x, _| {
            let v = (x * 8) as u8;
            [v, v, v, 255]
        });
        let adj = TemplateAdjustments::default();
        let a = sample_from_heatmap(&path, 320.0, 320.0, 200, &adj);
        let b = sample_from_heatmap(&path, 320.0, 320.0, 200, &adj);
        assert_eq!(a.len(), 200);
        let pa: Vec<_> = a.iter().map(|s| s.pos.to_array()).collect();
        let pb: Vec<_> = b.iter().map(|s| s.pos.to_array()).collect();
        assert_eq!(pa, pb, "sampler must be deterministic for stable tuning");
    }

    #[test]
    fn invert_moves_mass_to_dark_region() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("split.png");
        // Left half black, right half white.
        write_png(&path, |x, _| {
            let v = if x < 16 { 0 } else { 255 };
            [v, v, v, 255]
        });
        let canvas = 320.0;
        let def = sample_from_heatmap(&path, canvas, canvas, 400, &TemplateAdjustments::default());
        let inv = sample_from_heatmap(
            &path,
            canvas,
            canvas,
            400,
            &TemplateAdjustments {
                invert: true,
                ..Default::default()
            },
        );
        let mean = |v: &[SampledParticle]| v.iter().map(|s| s.pos.x).sum::<f32>() / v.len() as f32;
        assert!(mean(&def) > canvas * 0.5, "default mass on the bright half");
        assert!(mean(&inv) < canvas * 0.5, "inverted mass on the dark half");
    }

    #[test]
    fn sampled_color_matches_source_region() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("red.png");
        write_png(&path, |_, _| [200, 10, 10, 255]);
        let s = sample_from_heatmap(&path, 64.0, 64.0, 50, &TemplateAdjustments::default());
        for p in &s {
            assert!(
                p.color[0] > 150 && p.color[1] < 60 && p.color[2] < 60,
                "got {:?}",
                p.color
            );
        }
    }
}
