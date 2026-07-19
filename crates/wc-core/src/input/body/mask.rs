//! Segmentation-mask post-processing (worker-side): sigmoid the landmark
//! model's crop-space mask logits, inverse-warp them into the 256×256
//! content-normalized frame grid (the pinned "mask UV space" shared with the
//! published landmarks), blend against the previous frame to suppress mask
//! flicker, and quantize into the pooled `u8` buffer.
//!
//! The temporal blend is `MediaPipe`'s **uncertainty-weighted** segmentation
//! smoothing (`SegmentationSmoothingCalculator`, `combine_with_previous_ratio`)
//! rather than a uniform global EMA: each pixel's blend weight scales with how
//! *uncertain* its new value is (near 0.5 = the silhouette boundary), so the
//! boundary is stabilized hardest while confident interior/exterior pixels
//! track the new frame almost instantly. See [`uncertainty_blend`].
//!
//! All three working buffers (crop, frame, blend accumulator — 256 KB of `f32`
//! each) are allocated once in [`MaskProcessor::new`] and refilled in place:
//! the per-frame path performs no allocation (worker-loop hot-path rule).

use super::detector::sigmoid;
use super::roi::{ContentRect, RoiRect, LANDMARK_INPUT};
use super::MASK_SIZE;

/// Default mask temporal-blend strength. This is `MediaPipe`'s
/// `SegmentationSmoothingCalculator { combine_with_previous_ratio }`
/// (`pose_segmentation_filtering.pbtxt` uses `0.7`): the maximum fraction of
/// the *previous* frame mixed into a maximally-uncertain (boundary) pixel.
/// Confident pixels blend far less; see [`uncertainty_blend`]. Live-tunable
/// through `BodyLiveTuning` (Plan C's dev panel binds it).
///
/// The field/knob name still reads `mask_ema*` for continuity, but its meaning
/// is the combine-with-previous ratio, not an EMA alpha.
pub const DEFAULT_MASK_EMA_ALPHA: f32 = 0.7;

// Uncertainty polynomial coefficients from MediaPipe's
// `segmentation_smoothing_calculator.cc` (CPU `blending_fn`). They approximate
// the squared binary-entropy uncertainty `alpha(p) = clamp(1 - (1 - H(p))², 0,
// 1)` as a 5th-degree polynomial in `x = (p - 0.5)²`, where `p` is the new
// mask value. Cited verbatim so the blend is bit-for-bit the upstream CPU path.
const UNCERTAINTY_C1: f32 = 5.688_42;
const UNCERTAINTY_C2: f32 = -0.748_699;
const UNCERTAINTY_C3: f32 = -57.805_1;
const UNCERTAINTY_C4: f32 = 291.309;
const UNCERTAINTY_C5: f32 = -624.717;

/// Uncertainty-weighted temporal blend of a new mask `target` into the
/// previous-frame accumulator `acc`, per `MediaPipe`'s
/// `SegmentationSmoothingCalculator` CPU path
/// (`segmentation_smoothing_calculator.cc`):
///
/// ```text
/// x           = (new - 0.5)²
/// uncertainty = 1 - min(1, x·(c1 + x·(c2 + x·(c3 + x·(c4 + x·c5)))))
/// out         = new + (prev - new) · (uncertainty · ratio)
/// ```
///
/// `uncertainty ∈ [0, 1]` peaks at `new = 0.5` (the silhouette boundary) and
/// falls to ≈0 as `new` approaches 0 or 1 (confident background/foreground),
/// so boundary pixels blend up to `ratio` toward the previous frame while
/// confident pixels track the new frame almost exactly. Each pixel is
/// independent, so the write-in-place is exact. `ratio` is clamped to `[0, 1]`.
pub fn uncertainty_blend(acc: &mut [f32], target: &[f32], ratio: f32) {
    let r = ratio.clamp(0.0, 1.0);
    for (acc, &new) in acc.iter_mut().zip(target) {
        let prev = *acc;
        let t = new - 0.5;
        let x = t * t;
        let poly = x
            * (UNCERTAINTY_C1
                + x * (UNCERTAINTY_C2
                    + x * (UNCERTAINTY_C3 + x * (UNCERTAINTY_C4 + x * UNCERTAINTY_C5))));
        let uncertainty = 1.0 - poly.min(1.0);
        *acc = new + (prev - new) * (uncertainty * r);
    }
}

/// Decay `acc` toward zero: `acc −= acc · alpha` per element (the
/// person-absent mask fade). `alpha` is clamped to `[0, 1]`.
pub fn ema_decay(acc: &mut [f32], alpha: f32) {
    let a = alpha.clamp(0.0, 1.0);
    for v in acc.iter_mut() {
        *v -= *v * a;
    }
}

/// Owns the mask working buffers and the temporal-blend state.
pub struct MaskProcessor {
    /// Sigmoid-activated crop-space mask (`MASK_SIZE`², refilled per frame).
    crop: Vec<f32>,
    /// Frame-space (content-norm) warped mask for the current frame.
    frame: Vec<f32>,
    /// Temporal-blend accumulator (previous filtered frame) — what consumers
    /// see via [`Self::smoothed`].
    ema: Vec<f32>,
    /// Whether `ema` holds real history (first frame copies instead of
    /// blending, so a fresh track has no fade-in lag from the zero state).
    has_history: bool,
}

impl MaskProcessor {
    /// Allocate the three working buffers (the only allocation this type
    /// ever performs).
    #[must_use]
    pub fn new() -> Self {
        Self {
            crop: vec![0.0; MASK_SIZE * MASK_SIZE],
            frame: vec![0.0; MASK_SIZE * MASK_SIZE],
            ema: vec![0.0; MASK_SIZE * MASK_SIZE],
            has_history: false,
        }
    }

    /// Forget all mask state (track lost / worker restart).
    pub fn reset(&mut self) {
        self.ema.fill(0.0);
        self.has_history = false;
    }

    /// Ingest one crop-space mask: sigmoid `mask_logits` (row-major
    /// `MASK_SIZE`², the landmark model's `[1, 256, 256, 1]` output),
    /// inverse-warp through `roi`/`content` into frame space, and
    /// uncertainty-weighted-blend against the previous frame with combine ratio
    /// `ratio` ([`uncertainty_blend`]). Extra/short input is clamped defensively
    /// (the pipeline validates the tensor shape before calling).
    pub fn ingest(&mut self, mask_logits: &[f32], roi: &RoiRect, content: ContentRect, ratio: f32) {
        // 1. Activate the crop (65 k sigmoids, trivially cheap next to the
        //    model itself).
        for (dst, logit) in self.crop.iter_mut().zip(mask_logits) {
            *dst = sigmoid(*logit);
        }
        // 2. Inverse-warp: for each frame texel, find its square-norm
        //    position, rotate/scale into the crop's upright frame, and
        //    bilinearly sample the crop (0 outside — no person beyond the ROI).
        let (sin, cos) = roi.rotation.sin_cos();
        let inv_size = if roi.size > 0.0 { 1.0 / roi.size } else { 0.0 };
        let n = cellf(MASK_SIZE);
        for y in 0..MASK_SIZE {
            let v = (cellf(y) + 0.5) / n;
            for x in 0..MASK_SIZE {
                let u = (cellf(x) + 0.5) / n;
                let sq = content.from_content_norm(u, v);
                let dx = sq.x - roi.cx;
                let dy = sq.y - roi.cy;
                // Rotate by −rotation (transpose) into the crop frame.
                let cu = dx * cos + dy * sin;
                let cv = -dx * sin + dy * cos;
                let px = (cu * inv_size + 0.5) * LANDMARK_INPUT;
                let py = (cv * inv_size + 0.5) * LANDMARK_INPUT;
                self.frame[y * MASK_SIZE + x] = if inv_size > 0.0
                    && (0.0..LANDMARK_INPUT).contains(&px)
                    && (0.0..LANDMARK_INPUT).contains(&py)
                {
                    sample_bilinear(&self.crop, px, py)
                } else {
                    0.0
                };
            }
        }
        // 3. Uncertainty-weighted temporal blend (first frame copies — no
        //    fade-in lag).
        if self.has_history {
            uncertainty_blend(&mut self.ema, &self.frame, ratio);
        } else {
            self.ema.copy_from_slice(&self.frame);
            self.has_history = true;
        }
    }

    /// Fade the mask toward empty (called on person-absent frames so a stale
    /// silhouette never lingers). No-op before the first ingest.
    pub fn decay(&mut self, alpha: f32) {
        if self.has_history {
            ema_decay(&mut self.ema, alpha);
        }
    }

    /// The temporally-blended frame-space mask (`MASK_SIZE`² values in
    /// `[0, 1]`) — the edge extractor's input.
    #[must_use]
    pub fn smoothed(&self) -> &[f32] {
        &self.ema
    }

    /// Quantize the smoothed mask into a single-channel byte buffer (one byte
    /// per texel; the pooled payload written in place — no allocation).
    pub fn write_u8(&self, out: &mut [u8]) {
        for (dst, &v) in out.iter_mut().zip(&self.ema) {
            *dst = byte(v * 255.0);
        }
    }

    /// Quantize the smoothed mask into channel `channel` of an interleaved
    /// `stride`-bytes-per-texel buffer (the RGBA payload: `stride = 4`,
    /// channel = the body's slot per the pinned channel convention). Other
    /// channels are untouched; no allocation.
    pub fn write_channel(&self, out: &mut [u8], stride: usize, channel: usize) {
        debug_assert!(
            channel < stride,
            "channel {channel} outside stride {stride}"
        );
        for (texel, &v) in out.chunks_exact_mut(stride).zip(&self.ema) {
            texel[channel] = byte(v * 255.0);
        }
    }
}

impl Default for MaskProcessor {
    fn default() -> Self {
        Self::new()
    }
}

/// Bilinear sample of a `MASK_SIZE`² scalar grid at continuous index
/// coordinates, clamped to the edge (same convention as the hand pipeline's
/// RGB `sample_bilinear`).
fn sample_bilinear(m: &[f32], x: f32, y: f32) -> f32 {
    let max = cellf(MASK_SIZE - 1);
    let xc = x.clamp(0.0, max);
    let yc = y.clamp(0.0, max);
    let fx = xc - xc.floor();
    let fy = yc - yc.floor();
    let x0 = floor_index(xc);
    let y0 = floor_index(yc);
    let x1 = (x0 + 1).min(MASK_SIZE - 1);
    let y1 = (y0 + 1).min(MASK_SIZE - 1);
    let p00 = m[y0 * MASK_SIZE + x0];
    let p10 = m[y0 * MASK_SIZE + x1];
    let p01 = m[y1 * MASK_SIZE + x0];
    let p11 = m[y1 * MASK_SIZE + x1];
    let top = p00 + (p10 - p00) * fx;
    let bot = p01 + (p11 - p01) * fx;
    top + (bot - top) * fy
}

/// `usize` → `f32` for mask-grid indices (all ≤ 256, exact in `f32`).
fn cellf(v: usize) -> f32 {
    f32::from(u16::try_from(v).unwrap_or(u16::MAX))
}

/// Floor a finite, clamped, grid-bounded float to a mask index.
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is finite, clamped >= 0, and bounded by MASK_SIZE; float->int has no From/TryFrom"
)]
fn floor_index(v: f32) -> usize {
    v.max(0.0).floor() as usize
}

/// Round a `[0, 255]`-clamped float to a mask byte.
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is clamped to [0, 255]; float->int has no From/TryFrom"
)]
fn byte(v: f32) -> u8 {
    v.clamp(0.0, 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncertainty_blend_boundary_pixel_pulls_toward_previous() {
        // new = 0.5 is maximally uncertain (the silhouette boundary):
        // uncertainty = 1, so out = new + (prev − new)·ratio
        //                        = 0.5 + (1.0 − 0.5)·0.7 = 0.85.
        let mut acc = vec![1.0_f32];
        uncertainty_blend(&mut acc, &[0.5], 0.7);
        assert!((acc[0] - 0.85).abs() < 1e-6, "acc={}", acc[0]);
    }

    #[test]
    fn uncertainty_blend_confident_pixel_tracks_the_new_frame() {
        // new = 0.0 / 1.0 are fully confident (uncertainty ≈ 0): the previous
        // frame is essentially ignored, so the output tracks the new value.
        let mut bg = vec![1.0_f32];
        uncertainty_blend(&mut bg, &[0.0], 0.7);
        assert!(bg[0].abs() < 1e-3, "confident background acc={}", bg[0]);
        let mut fg = vec![0.0_f32];
        uncertainty_blend(&mut fg, &[1.0], 0.7);
        assert!(
            (fg[0] - 1.0).abs() < 1e-3,
            "confident foreground acc={}",
            fg[0]
        );
    }

    #[test]
    fn uncertainty_blend_ratio_zero_is_passthrough() {
        // ratio 0 drops the previous frame entirely: out = new everywhere,
        // whatever the per-pixel uncertainty.
        let mut acc = vec![1.0_f32, 0.3, 0.9];
        uncertainty_blend(&mut acc, &[0.5, 0.5, 0.5], 0.0);
        assert_eq!(acc, vec![0.5, 0.5, 0.5]);
    }

    #[test]
    fn ema_decay_fades_toward_zero() {
        let mut acc = vec![1.0_f32];
        ema_decay(&mut acc, 0.5);
        assert!((acc[0] - 0.5).abs() < 1e-6);
        for _ in 0..30 {
            ema_decay(&mut acc, 0.5);
        }
        assert!(acc[0] < 1e-3);
    }

    /// Full-content identity-ish setup: a square "camera" frame so the
    /// content rect is the whole square, and an ROI covering the whole frame
    /// unrotated — the warp becomes (approximately) the identity.
    fn identity_setup() -> (ContentRect, RoiRect) {
        (
            ContentRect::for_frame(256, 256),
            RoiRect {
                cx: 0.5,
                cy: 0.5,
                size: 1.0,
                rotation: 0.0,
            },
        )
    }

    #[test]
    fn first_ingest_seeds_the_ema_without_history_lag() {
        let (content, roi) = identity_setup();
        // Strongly-positive logits everywhere → sigmoid ≈ 1 across the crop.
        let logits = vec![10.0_f32; MASK_SIZE * MASK_SIZE];
        let mut p = MaskProcessor::new();
        p.ingest(&logits, &roi, content, 0.25);
        // First frame copies (no EMA lag from the zero-initialized history).
        let centre = p.smoothed()[(MASK_SIZE / 2) * MASK_SIZE + MASK_SIZE / 2];
        assert!(centre > 0.99, "centre={centre}");
    }

    #[test]
    fn ingest_warps_a_centred_blob_to_the_frame_centre() {
        let (content, roi) = identity_setup();
        // Person square in crop pixels [96, 160)²: +8 logits inside, −8 out.
        let mut logits = vec![-8.0_f32; MASK_SIZE * MASK_SIZE];
        for y in 96..160 {
            for x in 96..160 {
                logits[y * MASK_SIZE + x] = 8.0;
            }
        }
        let mut p = MaskProcessor::new();
        p.ingest(&logits, &roi, content, 1.0);
        let m = p.smoothed();
        let centre = m[128 * MASK_SIZE + 128];
        let corner = m[4 * MASK_SIZE + 4];
        assert!(centre > 0.9, "centre={centre}");
        assert!(corner < 0.1, "corner={corner}");
    }

    #[test]
    fn pixels_outside_the_roi_read_zero() {
        let content = ContentRect::for_frame(256, 256);
        // Tiny ROI in the upper-left: everything far from it must be 0 even
        // though the crop itself is fully "person".
        let roi = RoiRect {
            cx: 0.2,
            cy: 0.2,
            size: 0.2,
            rotation: 0.0,
        };
        let logits = vec![10.0_f32; MASK_SIZE * MASK_SIZE];
        let mut p = MaskProcessor::new();
        p.ingest(&logits, &roi, content, 1.0);
        let m = p.smoothed();
        assert!(m[240 * MASK_SIZE + 240] < 1e-6, "far corner must be empty");
        // ROI centre (0.2, 0.2) ≈ texel (51, 51) on the 256 grid.
        let inside = m[51 * MASK_SIZE + 51];
        assert!(inside > 0.9, "roi interior={inside}");
    }

    #[test]
    fn write_channel_targets_one_interleaved_channel() {
        let (content, roi) = identity_setup();
        let logits = vec![10.0_f32; MASK_SIZE * MASK_SIZE];
        let mut p = MaskProcessor::new();
        p.ingest(&logits, &roi, content, 1.0);
        // RGBA-interleaved buffer, slot 1 → channel G (index 1).
        let mut out = vec![7_u8; MASK_SIZE * MASK_SIZE * 4];
        p.write_channel(&mut out, 4, 1);
        let centre = (128 * MASK_SIZE + 128) * 4;
        assert_eq!(out[centre + 1], 255, "target channel written");
        assert_eq!(out[centre], 7, "other channels untouched");
        assert_eq!(out[centre + 2], 7);
        assert_eq!(out[centre + 3], 7);
    }

    #[test]
    fn write_u8_quantizes_the_full_range() {
        let (content, roi) = identity_setup();
        let logits = vec![10.0_f32; MASK_SIZE * MASK_SIZE];
        let mut p = MaskProcessor::new();
        p.ingest(&logits, &roi, content, 1.0);
        let mut out = vec![0_u8; MASK_SIZE * MASK_SIZE];
        p.write_u8(&mut out);
        assert_eq!(out[128 * MASK_SIZE + 128], 255);
        p.reset();
        p.write_u8(&mut out);
        assert_eq!(out[128 * MASK_SIZE + 128], 0);
    }
}
