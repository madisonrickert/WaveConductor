//! Per-image adjustments for the Line spawn template: the luminance remap
//! (white/black point, gamma, invert) that reshapes spawn density, the
//! position/scale transform of the sampled coordinates, and the RGB pack used
//! to carry a per-particle spawn colour through the GPU `Particle` struct.
//!
//! Defaults are the identity: an image with default adjustments samples and
//! renders exactly as it did before this module existed. The math here is pure
//! (no Bevy systems) so it is unit-testable without a `World`.

use bevy::math::Vec2;
use bevy::reflect::Reflect;
use serde::{Deserialize, Serialize};

/// Per-image tuning knobs. `position`/`scale` are `[f32; 2]` (serde-trivial,
/// avoids depending on glam's serde feature); convert to [`Vec2`] at use sites.
/// [`Default`] is the identity (no remap, no transform, no colour tint), so an
/// image without saved adjustments behaves exactly as before.
#[derive(Clone, Debug, PartialEq, Reflect, Serialize, Deserialize)]
pub struct TemplateAdjustments {
    /// Upper luminance (0..1) mapped to full spawn weight.
    pub white_point: f32,
    /// Lower luminance (0..1) mapped to zero spawn weight.
    pub black_point: f32,
    /// Spawn in the dark regions instead of the bright ones.
    pub invert: bool,
    /// Response-curve exponent applied to the remapped luminance (1.0 = linear).
    pub gamma: f32,
    /// Canvas-normalized offset; `±1.0` shifts by half the canvas on that axis.
    pub position: [f32; 2],
    /// Per-axis zoom about the canvas centre (1.0 = original size).
    pub scale: [f32; 2],
    /// Blend toward the image pixel colour, `0..1` (driven as a live render
    /// uniform, not baked into the spawn).
    pub color_influence: f32,
}

impl Default for TemplateAdjustments {
    fn default() -> Self {
        Self {
            white_point: 1.0,
            black_point: 0.0,
            invert: false,
            gamma: 1.0,
            position: [0.0, 0.0],
            scale: [1.0, 1.0],
            color_influence: 0.0,
        }
    }
}

/// Per-pixel spawn weight after the luminance remap. `luminance_0_255` is the
/// Rec.601 luminance of the pixel, `alpha_0_1` its normalized alpha. At defaults
/// this is `(luminance/255) * alpha` — the old `luminance * alpha` up to the
/// `/255` normalization the remap introduces (the relative distribution that
/// inverse-CDF sampling depends on is unchanged at defaults).
#[must_use]
pub fn remap_weight(luminance_0_255: f32, alpha_0_1: f32, adj: &TemplateAdjustments) -> f32 {
    let lum = (luminance_0_255 / 255.0).clamp(0.0, 1.0);
    // eps-guarded so white <= black degrades to a hard threshold, not NaN.
    let span = (adj.white_point - adj.black_point).max(1e-4);
    let mut t = ((lum - adj.black_point) / span).clamp(0.0, 1.0);
    t = t.powf(adj.gamma.max(1e-3));
    if adj.invert {
        t = 1.0 - t;
    }
    t * alpha_0_1
}

/// Transform a window-space sample about the canvas centre: scale zooms, then
/// position shifts by `position * (canvas / 2)` per axis. Off-canvas results are
/// returned as-is (the sim/camera handle off-screen positions).
#[must_use]
pub fn transform_point(sampled: Vec2, canvas: Vec2, adj: &TemplateAdjustments) -> Vec2 {
    let center = canvas * 0.5;
    let scale = Vec2::new(adj.scale[0], adj.scale[1]);
    let offset = Vec2::new(adj.position[0], adj.position[1]) * center;
    center + (sampled - center) * scale + offset
}

/// Pack an RGB8 triple into an `f32` slot bit-for-bit (stored via
/// [`f32::from_bits`], recovered in WGSL via `bitcast<u32>`). The byte layout is
/// `0x00RRGGBB`, so the WGSL side reads `r = (bits >> 16) & 0xFF`, etc. The
/// result is an opaque bit pattern — never do float math on it.
#[must_use]
pub fn pack_rgb8(rgb: [u8; 3]) -> f32 {
    f32::from_bits(u32::from_be_bytes([0, rgb[0], rgb[1], rgb[2]]))
}

/// Inverse of [`pack_rgb8`].
#[must_use]
pub fn unpack_rgb8(packed: f32) -> [u8; 3] {
    let [_, r, g, b] = packed.to_bits().to_be_bytes();
    [r, g, b]
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Vec2;

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-5, "got {a}, want {b}");
    }

    #[test]
    fn defaults_are_identity_weight() {
        let adj = TemplateAdjustments::default();
        approx(remap_weight(255.0, 1.0, &adj), 1.0);
        approx(remap_weight(0.0, 1.0, &adj), 0.0);
        approx(remap_weight(128.0, 0.5, &adj), (128.0 / 255.0) * 0.5);
    }

    #[test]
    fn black_and_white_point_clamp() {
        let adj = TemplateAdjustments {
            black_point: 0.25,
            white_point: 0.75,
            ..Default::default()
        };
        approx(remap_weight(0.25 * 255.0, 1.0, &adj), 0.0);
        approx(remap_weight(0.75 * 255.0, 1.0, &adj), 1.0);
        approx(remap_weight(0.5 * 255.0, 1.0, &adj), 0.5);
    }

    #[test]
    fn gamma_bends_curve() {
        let adj = TemplateAdjustments {
            gamma: 2.0,
            ..Default::default()
        };
        approx(remap_weight(0.5 * 255.0, 1.0, &adj), 0.25);
    }

    #[test]
    fn invert_flips() {
        let adj = TemplateAdjustments {
            invert: true,
            ..Default::default()
        };
        approx(remap_weight(255.0, 1.0, &adj), 0.0);
        approx(remap_weight(0.0, 1.0, &adj), 1.0);
    }

    #[test]
    fn degenerate_white_le_black_is_threshold_not_nan() {
        let adj = TemplateAdjustments {
            black_point: 0.5,
            white_point: 0.5,
            ..Default::default()
        };
        assert!(remap_weight(0.9 * 255.0, 1.0, &adj).is_finite());
    }

    #[test]
    fn transform_default_is_identity() {
        let adj = TemplateAdjustments::default();
        let canvas = Vec2::new(1280.0, 720.0);
        let p = Vec2::new(300.0, 200.0);
        assert_eq!(transform_point(p, canvas, &adj), p);
    }

    #[test]
    fn transform_scale_zooms_about_center() {
        let adj = TemplateAdjustments {
            scale: [2.0, 2.0],
            ..Default::default()
        };
        let canvas = Vec2::new(1000.0, 1000.0);
        assert_eq!(
            transform_point(Vec2::new(500.0, 500.0), canvas, &adj),
            Vec2::new(500.0, 500.0)
        );
        approx(
            transform_point(Vec2::new(600.0, 500.0), canvas, &adj).x,
            700.0,
        );
    }

    #[test]
    fn transform_position_shifts_by_half_canvas_per_unit() {
        let adj = TemplateAdjustments {
            position: [1.0, 0.0],
            ..Default::default()
        };
        let canvas = Vec2::new(1000.0, 800.0);
        approx(
            transform_point(Vec2::new(500.0, 400.0), canvas, &adj).x,
            1000.0,
        );
    }

    #[test]
    fn pack_unpack_round_trips() {
        for rgb in [[0, 0, 0], [255, 255, 255], [10, 20, 30], [1, 2, 3]] {
            assert_eq!(unpack_rgb8(pack_rgb8(rgb)), rgb);
        }
    }
}
