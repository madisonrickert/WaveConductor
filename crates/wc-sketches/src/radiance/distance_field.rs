//! Silhouette distance field: a per-frame 256² chamfer distance transform of
//! the person mask, feeding the beat-wave shader.
//!
//! The beat pulses render as *contour waves of the dancer's silhouette* —
//! light fronts that detach from the body's edge and travel outward keeping
//! the body's shape (nested outlines), not circles around a point. The
//! fragment shader needs "distance from the silhouette" at every pixel; this
//! module computes it on the CPU with a classic two-pass 3×3 chamfer
//! transform over the same 256² mask the silhouette fill samples, and
//! publishes it as an `R8Unorm` texture (`0..1` = `0..`[`DIST_MAX_TEXELS`]
//! texels). 256² is 65k texels; the two passes cost a fraction of a
//! millisecond and run only when [`SilhouetteEdges::generation`] advances
//! (~30 Hz body frames, not render frames).
//!
//! Hot-path posture: the scratch buffer and output image are allocated once
//! at sketch spawn; [`update_distance_field`] mutates them in place and
//! allocates nothing.

use bevy::image::Image;
use bevy::prelude::*;
use wc_core::input::body::{MaskTexture, SilhouetteEdges, MASK_SIZE};

/// Distance value (in mask texels) that maps to 1.0 in the `R8Unorm` output.
/// 160 texels is ~0.63 of the mask square — at a 1080-px-tall window that is
/// ~675 px of wave travel before the field saturates, with ~2.6 px of
/// quantization per R8 step (well under the wave's ~60 px width).
pub const DIST_MAX_TEXELS: f32 = 160.0;

/// Mask coverage threshold for "inside the body" (the body-tracking
/// contract's fixed edge threshold).
pub const MASK_INSIDE_THRESHOLD: u8 = 128;

/// Chamfer weights ×12 as integers (3-4 chamfer: orthogonal 3, diagonal 4 —
/// the standard integer approximation of 1/√2 stepping, error < 6%).
const ORTHO_COST: u32 = 3;
/// See [`ORTHO_COST`].
const DIAG_COST: u32 = 4;
/// "Infinite" seed for texels with no body anywhere near.
const FAR: u32 = u32::MAX / 2;

/// Owns the distance-field texture + scratch. Inserted at Radiance spawn,
/// removed at exit (the image handle is this resource's only owner besides
/// the pulse material, both dropped on exit — mechanism 1, entity/resource
/// owned).
#[derive(Resource)]
pub struct RadianceDistanceField {
    /// The published `R8Unorm` 256² distance texture.
    pub image: Handle<Image>,
    /// Preallocated chamfer scratch (one `u32` per texel).
    scratch: Vec<u32>,
    /// Last [`SilhouetteEdges::generation`] the field was computed for.
    last_generation: u64,
}

impl RadianceDistanceField {
    /// Wrap a freshly-allocated image handle with zeroed scratch.
    #[must_use]
    pub fn new(image: Handle<Image>) -> Self {
        Self {
            image,
            scratch: vec![0; MASK_SIZE * MASK_SIZE],
            last_generation: u64::MAX,
        }
    }
}

/// Two-pass 3-4 chamfer distance transform: `out[i]` = distance from texel
/// `i` to the nearest body texel (`mask >= MASK_INSIDE_THRESHOLD`), in
/// units of [`DIST_MAX_TEXELS`] mapped to `0..=255`. Body-interior texels
/// are 0. Pure over the buffers for testability; `scratch` must be
/// `MASK_SIZE²` long, `out` likewise.
pub fn chamfer_distance(mask: &[u8], scratch: &mut [u32], out: &mut [u8]) {
    debug_assert_eq!(mask.len(), MASK_SIZE * MASK_SIZE);
    debug_assert_eq!(scratch.len(), MASK_SIZE * MASK_SIZE);
    debug_assert_eq!(out.len(), MASK_SIZE * MASK_SIZE);

    // Seed: body = 0, everything else = far.
    for (s, &m) in scratch.iter_mut().zip(mask.iter()) {
        *s = if m >= MASK_INSIDE_THRESHOLD { 0 } else { FAR };
    }
    chamfer_from_seeded(scratch, out);
}

/// `Update` (gated `in_state(AppState::Radiance)`, before the pulse driver):
/// recompute the distance field when a new body frame has arrived
/// (generation-gated, like the edge upload). Skips cleanly when any surface
/// is missing (headless tests, feature-reduced harnesses).
pub fn update_distance_field(
    edges: Option<Res<'_, SilhouetteEdges>>,
    mask: Option<Res<'_, MaskTexture>>,
    field: Option<ResMut<'_, RadianceDistanceField>>,
    mut images: ResMut<'_, Assets<Image>>,
) {
    let (Some(edges), Some(mask), Some(mut field)) = (edges, mask, field) else {
        return;
    };
    if edges.generation == field.last_generation {
        return;
    }
    field.last_generation = edges.generation;

    // The mask image (read) and the output image (written) live in the same
    // `Assets<Image>` store, which cannot hand out overlapping borrows. The
    // seed pass is the only step that needs the mask, so: seed the scratch
    // from a short-lived mask borrow, then run the relaxation passes into
    // the output image under a second borrow. `mem::take` frees the scratch
    // from `field` so both borrows stay disjoint; no bytes are copied and
    // nothing allocates.
    let mut scratch = std::mem::take(&mut field.scratch);
    let field_handle = field.image.clone();
    {
        let Some(mask_data) = images.get(&mask.0).and_then(|m| m.data.as_ref()) else {
            field.scratch = scratch;
            return;
        };
        for (s, &m) in scratch.iter_mut().zip(mask_data.iter()) {
            *s = if m >= MASK_INSIDE_THRESHOLD { 0 } else { FAR };
        }
    }
    if let Some(mut out_image) = images.get_mut(&field_handle) {
        if let Some(out_data) = out_image.data.as_mut() {
            chamfer_from_seeded(&mut scratch, out_data);
        }
    }
    field.scratch = scratch;
}

/// The two chamfer relaxation passes over an already-seeded scratch (see
/// [`chamfer_distance`], which seeds and then calls this; the system seeds
/// directly from the borrowed mask to avoid staging a copy of the bytes).
pub fn chamfer_from_seeded(scratch: &mut [u32], out: &mut [u8]) {
    // Forward pass.
    for y in 0..MASK_SIZE {
        for x in 0..MASK_SIZE {
            let i = y * MASK_SIZE + x;
            let mut d = scratch[i];
            if d == 0 {
                continue;
            }
            if x > 0 {
                d = d.min(scratch[i - 1] + ORTHO_COST);
            }
            if y > 0 {
                let up = i - MASK_SIZE;
                d = d.min(scratch[up] + ORTHO_COST);
                if x > 0 {
                    d = d.min(scratch[up - 1] + DIAG_COST);
                }
                if x + 1 < MASK_SIZE {
                    d = d.min(scratch[up + 1] + DIAG_COST);
                }
            }
            scratch[i] = d;
        }
    }
    // Backward pass + normalization.
    for y in (0..MASK_SIZE).rev() {
        for x in (0..MASK_SIZE).rev() {
            let i = y * MASK_SIZE + x;
            let mut d = scratch[i];
            if d == 0 {
                out[i] = 0;
                continue;
            }
            if x + 1 < MASK_SIZE {
                d = d.min(scratch[i + 1] + ORTHO_COST);
            }
            if y + 1 < MASK_SIZE {
                let down = i + MASK_SIZE;
                d = d.min(scratch[down] + ORTHO_COST);
                if x > 0 {
                    d = d.min(scratch[down - 1] + DIAG_COST);
                }
                if x + 1 < MASK_SIZE {
                    d = d.min(scratch[down + 1] + DIAG_COST);
                }
            }
            scratch[i] = d;
            #[allow(
                clippy::as_conversions,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss,
                reason = "d/3 <= ~2*MASK_SIZE texels, exact in f32; clamped \
                          into u8 range before the cast"
            )]
            {
                let texels = d as f32 / ORTHO_COST as f32;
                out[i] = (texels / DIST_MAX_TEXELS * 255.0).clamp(0.0, 255.0) as u8;
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;

    /// A single body texel: distance grows outward and is exact along the
    /// axes (3-4 chamfer, ortho steps).
    #[test]
    fn chamfer_distances_grow_from_a_point() {
        let mut mask = vec![0_u8; MASK_SIZE * MASK_SIZE];
        let cx = 128;
        let cy = 128;
        mask[cy * MASK_SIZE + cx] = 255;
        let mut scratch = vec![0_u32; MASK_SIZE * MASK_SIZE];
        let mut out = vec![0_u8; MASK_SIZE * MASK_SIZE];
        chamfer_distance(&mask, &mut scratch, &mut out);

        assert_eq!(out[cy * MASK_SIZE + cx], 0, "body texel is distance 0");
        // 16 texels straight right: 16/160 of the range.
        let d16 = f32::from(out[cy * MASK_SIZE + cx + 16]);
        let expect = 16.0 / DIST_MAX_TEXELS * 255.0;
        assert!(
            (d16 - expect).abs() <= 2.0,
            "axis distance ~exact: {d16} vs {expect}"
        );
        // Distance is monotone along the axis.
        let d32 = f32::from(out[cy * MASK_SIZE + cx + 32]);
        assert!(d32 > d16, "farther texel reads farther");
        // Diagonal ~sqrt(2) ratio (4/3 chamfer approximation).
        let ddiag = f32::from(out[(cy + 16) * MASK_SIZE + cx + 16]);
        let ratio = ddiag / d16;
        assert!(
            (1.25..=1.45).contains(&ratio),
            "diagonal/ortho ratio ~1.33: {ratio}"
        );
    }

    /// An empty mask saturates the whole field at 255 (no body anywhere).
    #[test]
    fn empty_mask_saturates() {
        let mask = vec![0_u8; MASK_SIZE * MASK_SIZE];
        let mut scratch = vec![0_u32; MASK_SIZE * MASK_SIZE];
        let mut out = vec![0_u8; MASK_SIZE * MASK_SIZE];
        chamfer_distance(&mask, &mut scratch, &mut out);
        assert!(out.iter().all(|&d| d == 255), "no body -> field saturated");
    }

    /// A filled half-plane: distance equals the row gap to the boundary.
    #[test]
    fn half_plane_distance_is_row_gap() {
        let mut mask = vec![0_u8; MASK_SIZE * MASK_SIZE];
        for y in 0..128 {
            for x in 0..MASK_SIZE {
                mask[y * MASK_SIZE + x] = 255;
            }
        }
        let mut scratch = vec![0_u32; MASK_SIZE * MASK_SIZE];
        let mut out = vec![0_u8; MASK_SIZE * MASK_SIZE];
        chamfer_distance(&mask, &mut scratch, &mut out);
        let d10 = f32::from(out[(127 + 10) * MASK_SIZE + 64]);
        let expect = 10.0 / DIST_MAX_TEXELS * 255.0;
        assert!((d10 - expect).abs() <= 2.0, "{d10} vs {expect}");
    }
}
