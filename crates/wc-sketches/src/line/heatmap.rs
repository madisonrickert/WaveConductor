//! Heatmap-image particle spawn sampler.
//!
//! Loads a PNG, computes per-pixel luminance × alpha, builds a CDF over the
//! resulting weights, and returns `count` sampled (x, y) positions in
//! window-space (top-left origin, +y down).
//!
//! Direct port of v4's `src/sketches/line/heatmapSampler.ts`. Used by
//! [`crate::line::systems::spawn::spawn_line`] when
//! [`crate::line::settings::LineSettings::spawn_template`] is non-empty.
//!
//! ## Failure mode
//!
//! Any error along the path (file missing, undecodable image, all-zero
//! luminance × alpha) falls back to a horizontal mid-line + sawtooth Y-jitter
//! identical in shape to the default `spawn_line` layout — a broken template
//! never breaks the sketch. The error is logged at `warn` level so a misnamed
//! file is discoverable in logs.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "u32 ↔ usize ↔ f32 casts for image dimensions and CDF indices are intentional and bounded"
)]

use std::path::Path;

use bevy::math::Vec2;
use rand::Rng;

/// Maximum sampled-grid dimension. Larger images are bilinearly downsampled
/// to this resolution before the CDF is built — a 256×256 grid (65,536 bins)
/// is plenty for spawn-density purposes and keeps the CDF allocation under a
/// megabyte regardless of source image size.
const MAX_SAMPLE_DIM: u32 = 256;

/// Sample `count` particles from a brightness heatmap image.
///
/// `path` — image file (PNG / JPG / WebP — anything the `image` crate decodes,
/// gated by enabled features in `wc-sketches/Cargo.toml`).
/// `canvas_w` / `canvas_h` — pixel dimensions of the target rendering canvas.
/// `count` — number of particles to sample.
///
/// Returns `Vec<Vec2>` with exactly `count` (x, y) positions in window space
/// (top-left origin, +y down). The caller is responsible for translating to
/// world space if the sketch uses a centered coordinate system — see the
/// caller in `crate::line::systems::spawn::spawn_line` for the canonical
/// conversion.
///
/// On any error or a fully-zero weight grid, falls back to
/// [`fallback_line`] so the sketch still renders something.
pub fn sample_from_heatmap(path: &Path, canvas_w: f32, canvas_h: f32, count: usize) -> Vec<Vec2> {
    match try_sample_from_heatmap(path, canvas_w, canvas_h, count) {
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
) -> Result<Vec<Vec2>, image::ImageError> {
    let img = image::open(path)?;

    // Downsample to keep the CDF small. Source pixels above `MAX_SAMPLE_DIM`
    // on either axis are bilinearly averaged into the sample grid.
    let sample_w = (canvas_w.min(MAX_SAMPLE_DIM as f32).max(1.0)) as u32;
    let sample_h = (canvas_h.min(MAX_SAMPLE_DIM as f32).max(1.0)) as u32;
    let img = img.resize_exact(sample_w, sample_h, image::imageops::FilterType::Triangle);
    let rgba = img.to_rgba8();

    // Build the cumulative distribution over per-pixel luminance × alpha.
    // Luminance coefficients (0.299, 0.587, 0.114) are the standard Rec. 601
    // grayscale conversion — same constants v4 used.
    let total_pixels = (sample_w * sample_h) as usize;
    let mut cdf: Vec<f64> = Vec::with_capacity(total_pixels);
    let mut cumulative = 0.0_f64;
    for px in rgba.pixels() {
        let r = f64::from(px[0]);
        let g = f64::from(px[1]);
        let b = f64::from(px[2]);
        let a = f64::from(px[3]) / 255.0;
        let luminance = 0.299 * r + 0.587 * g + 0.114 * b;
        cumulative += luminance * a;
        cdf.push(cumulative);
    }

    // All-black or fully-transparent image: nothing to sample. Hand back the
    // fallback so the caller still gets `count` positions.
    if cumulative == 0.0 {
        return Ok(fallback_line(canvas_w, canvas_h, count));
    }

    // Inverse-CDF sampling: roll a uniform `target` in `[0, cumulative)`,
    // binary-search the CDF for the first bin whose cumulative weight is
    // `>= target`. Sub-pixel jitter inside the chosen bin avoids visible
    // grid aliasing.
    let mut rng = rand::rng();
    let scale_x = canvas_w / sample_w as f32;
    let scale_y = canvas_h / sample_h as f32;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let target = rng.random::<f64>() * cumulative;
        let idx = cdf.partition_point(|&c| c < target);
        let idx = idx.min(total_pixels - 1);
        let px = (idx as u32) % sample_w;
        let py = (idx as u32) / sample_w;
        let x = (px as f32 + rng.random::<f32>()) * scale_x;
        let y = (py as f32 + rng.random::<f32>()) * scale_y;
        out.push(Vec2::new(x, y));
    }
    Ok(out)
}

/// Fallback layout: a horizontal mid-line with five-strand sawtooth Y-jitter.
/// Matches the shape of the default `spawn_line` layout so a broken template
/// degrades gracefully to the v5-line-sim baseline. Returns window-space
/// coordinates (top-left origin, +y down) — the same coordinate system
/// [`sample_from_heatmap`] returns.
fn fallback_line(canvas_w: f32, canvas_h: f32, count: usize) -> Vec<Vec2> {
    let mid_y = canvas_h * 0.5;
    (0..count)
        .map(|i| {
            let x = (i as f32 / count.max(1) as f32) * canvas_w;
            // Five-strand sawtooth: ((i % 5) - 2) * 2 px around mid_y.
            let jitter_strand = (i % 5) as f32 - 2.0;
            let y = mid_y + jitter_strand * 2.0;
            Vec2::new(x, y)
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
        let positions = fallback_line(1280.0, 720.0, 100);
        assert_eq!(positions.len(), 100);
    }

    #[test]
    fn fallback_line_spans_canvas_width() {
        let positions = fallback_line(1280.0, 720.0, 100);
        let xs: Vec<f32> = positions.iter().map(|p| p.x).collect();
        // First x near 0, last x near canvas_w (not exact due to i/count spacing).
        assert!(xs[0] >= 0.0 && xs[0] < 50.0);
        assert!(*xs.last().expect("non-empty positions vec") > 1200.0);
    }

    #[test]
    fn fallback_line_jitter_stays_near_mid_y() {
        let positions = fallback_line(1280.0, 720.0, 50);
        let mid_y = 360.0_f32;
        for p in &positions {
            // Sawtooth amplitude is ((0..5) - 2) * 2 ∈ [-4, +4] px.
            assert!((p.y - mid_y).abs() <= 4.0_f32 + 0.001);
        }
    }

    #[test]
    fn missing_file_falls_back_to_horizontal_line() {
        let path = Path::new("/this/path/does/not/exist.png");
        let positions = sample_from_heatmap(path, 1280.0, 720.0, 64);
        assert_eq!(positions.len(), 64);
        // The fallback path's Y values cluster around mid-Y.
        let mid_y = 360.0_f32;
        for p in &positions {
            assert!((p.y - mid_y).abs() <= 4.0_f32 + 0.001);
        }
    }

    #[test]
    fn zero_count_returns_empty() {
        let positions = fallback_line(1280.0, 720.0, 0);
        assert!(positions.is_empty());
    }
}
