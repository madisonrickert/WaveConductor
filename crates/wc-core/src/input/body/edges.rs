//! Silhouette edge extraction: scan the temporally-blended mask for
//! [`EDGE_THRESHOLD`] crossings between neighbouring texels and emit up to
//! [`MAX_EDGE_POINTS`] `(position, outward normal)` pairs.
//!
//! Runs on the worker (a single 256² pass, negligible next to inference).
//! The output is Plan C's particle-emission surface (uploaded as a storage
//! buffer) and doubles as the silhouette rim source. The caller supplies a
//! buffer with capacity [`MAX_EDGE_POINTS`]; extraction clear-refills it and
//! clamps at capacity, so it never allocates (worker hot-path rule).

use bevy::math::Vec2;

use super::{EdgePoint, MASK_SIZE, MAX_EDGE_POINTS};

/// Iso-level at which the mask boundary is traced.
pub const EDGE_THRESHOLD: f32 = 0.5;

/// Extract silhouette edge points from a `MASK_SIZE`² smoothed mask
/// (row-major, values in `[0, 1]`) into `out` (cleared first; capacity must
/// be ≥ [`MAX_EDGE_POINTS`], which the pooled payload and `SilhouetteEdges`
/// guarantee by construction).
///
/// Two passes in deterministic scan order: horizontal crossings (between
/// x and x+1) then vertical (between y and y+1). Each crossing interpolates
/// the sub-texel position and takes the outward normal from the mask
/// gradient (central differences, clamped at borders): inside > threshold >
/// outside, so the outward direction is −gradient. Degenerate zero-gradient
/// crossings are skipped rather than given a fake normal.
pub fn extract_edges(mask: &[f32], out: &mut Vec<EdgePoint>) {
    out.clear();
    debug_assert_eq!(mask.len(), MASK_SIZE * MASK_SIZE);
    let n = MASK_SIZE;
    let nf = cellf(n);
    // Horizontal crossings: between (x, y) and (x+1, y).
    for y in 0..n {
        for x in 0..n - 1 {
            if out.len() == MAX_EDGE_POINTS {
                return;
            }
            let a = mask[y * n + x];
            let b = mask[y * n + x + 1];
            if !crosses(a, b) {
                continue;
            }
            let t = (EDGE_THRESHOLD - a) / (b - a);
            let pos = Vec2::new((cellf(x) + 0.5 + t) / nf, (cellf(y) + 0.5) / nf);
            let sample_x = if t < 0.5 { x } else { x + 1 };
            if let Some(normal) = outward_normal(mask, sample_x, y) {
                out.push(EdgePoint { pos, normal });
            }
        }
    }
    // Vertical crossings: between (x, y) and (x, y+1).
    for y in 0..n - 1 {
        for x in 0..n {
            if out.len() == MAX_EDGE_POINTS {
                return;
            }
            let a = mask[y * n + x];
            let b = mask[(y + 1) * n + x];
            if !crosses(a, b) {
                continue;
            }
            let t = (EDGE_THRESHOLD - a) / (b - a);
            let pos = Vec2::new((cellf(x) + 0.5) / nf, (cellf(y) + 0.5 + t) / nf);
            let sample_y = if t < 0.5 { y } else { y + 1 };
            if let Some(normal) = outward_normal(mask, x, sample_y) {
                out.push(EdgePoint { pos, normal });
            }
        }
    }
}

/// Whether the mask value crosses [`EDGE_THRESHOLD`] between two texels.
/// Strict inequality: a texel exactly at the threshold is not a crossing on
/// its own (its neighbour pair on the other side will be).
fn crosses(a: f32, b: f32) -> bool {
    (a - EDGE_THRESHOLD) * (b - EDGE_THRESHOLD) < 0.0
}

/// Outward unit normal at texel `(x, y)`: −normalize(∇mask), central
/// differences with border clamping. `None` when the local gradient is
/// degenerate (flat plateau — cannot orient a normal).
fn outward_normal(mask: &[f32], x: usize, y: usize) -> Option<Vec2> {
    let n = MASK_SIZE;
    let xl = x.saturating_sub(1);
    let xr = (x + 1).min(n - 1);
    let yu = y.saturating_sub(1);
    let yd = (y + 1).min(n - 1);
    let g = Vec2::new(
        mask[y * n + xr] - mask[y * n + xl],
        mask[yd * n + x] - mask[yu * n + x],
    );
    let len = g.length();
    if len > f32::EPSILON {
        Some(-g / len)
    } else {
        None
    }
}

/// `usize` → `f32` for mask-grid indices (all ≤ 256, exact in `f32`).
fn cellf(v: usize) -> f32 {
    f32::from(u16::try_from(v).unwrap_or(u16::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Vec2;

    /// Binary disc mask: 1.0 inside radius `r` texels of `centre`, else 0.0.
    fn disc(centre: Vec2, r: f32) -> Vec<f32> {
        let mut m = vec![0.0_f32; MASK_SIZE * MASK_SIZE];
        for y in 0..MASK_SIZE {
            for x in 0..MASK_SIZE {
                let p = Vec2::new(cellf(x) + 0.5, cellf(y) + 0.5);
                if p.distance(centre) < r {
                    m[y * MASK_SIZE + x] = 1.0;
                }
            }
        }
        m
    }

    fn cellf(v: usize) -> f32 {
        u16::try_from(v).map_or(0.0, f32::from)
    }

    #[test]
    fn circle_yields_perimeter_points_with_outward_unit_normals() {
        let centre = Vec2::new(128.0, 128.0);
        let mask = disc(centre, 60.0);
        let mut out = Vec::with_capacity(MAX_EDGE_POINTS);
        extract_edges(&mask, &mut out);
        // A radius-60 disc crosses ~2 texels per row over ~120 rows plus the
        // same per column: ≈ 480 crossings. Wide band for discretization.
        assert!(
            (380..=600).contains(&out.len()),
            "unexpected edge count {}",
            out.len()
        );
        let centre_uv = centre / cellf(MASK_SIZE);
        for p in &out {
            // Unit-length normal…
            assert!(
                (p.normal.length() - 1.0).abs() < 1e-3,
                "normal={:?}",
                p.normal
            );
            // …pointing away from the disc centre (outward).
            let radial = p.pos - centre_uv;
            assert!(
                radial.dot(p.normal) > 0.0,
                "normal {:?} not outward at {:?}",
                p.normal,
                p.pos
            );
            // Positions stay in the unit square, on the circle (± one texel).
            let r_uv = 60.0 / cellf(MASK_SIZE);
            assert!((radial.length() - r_uv).abs() < 2.0 / cellf(MASK_SIZE));
        }
    }

    #[test]
    fn torso_blob_edges_have_axis_aligned_normals_on_the_flanks() {
        // A filled axis-aligned rectangle (torso stand-in): x ∈ [96, 160),
        // y ∈ [64, 192).
        let mut mask = vec![0.0_f32; MASK_SIZE * MASK_SIZE];
        for y in 64..192 {
            for x in 96..160 {
                mask[y * MASK_SIZE + x] = 1.0;
            }
        }
        let mut out = Vec::with_capacity(MAX_EDGE_POINTS);
        extract_edges(&mask, &mut out);
        // 2 horizontal crossings × 128 rows + 2 vertical × 64 columns = 384.
        assert!(
            (350..=420).contains(&out.len()),
            "unexpected edge count {}",
            out.len()
        );
        // Points on the left flank (x ≈ 96/256, away from corners) must point
        // straight −x.
        let mut checked = 0;
        for p in &out {
            if (p.pos.x - 96.0 / 256.0).abs() < 1.5 / 256.0
                && p.pos.y > 100.0 / 256.0
                && p.pos.y < 150.0 / 256.0
            {
                assert!(p.normal.x < -0.9, "left-flank normal {:?}", p.normal);
                assert!(p.normal.y.abs() < 0.3);
                checked += 1;
            }
        }
        assert!(checked > 10, "too few left-flank samples: {checked}");
    }

    #[test]
    fn capacity_clamps_without_reallocating() {
        // Vertical bands, width 2 (period 4): every band edge crosses 0.5 —
        // ~128 crossings per row × 256 rows, far beyond MAX_EDGE_POINTS.
        // Deviation from the brief's single-texel-wide (period-2) stripes:
        // with period 2 every interior crossing's chosen sample column has
        // identical left/right neighbours (same parity → same value), so the
        // central-difference gradient is exactly zero and outward_normal
        // correctly skips it as unorientable — only ~256 boundary-clamp
        // artifacts survive, never reaching capacity. Width-2 bands keep
        // every interior crossing's gradient nonzero while still producing
        // far more than MAX_EDGE_POINTS crossings, so the capacity-clamp,
        // no-realloc, and pointer-stability invariants are still exercised.
        let mut mask = vec![0.0_f32; MASK_SIZE * MASK_SIZE];
        for y in 0..MASK_SIZE {
            for x in 0..MASK_SIZE {
                if x % 4 < 2 {
                    mask[y * MASK_SIZE + x] = 1.0;
                }
            }
        }
        let mut out = Vec::with_capacity(MAX_EDGE_POINTS);
        let ptr = out.as_ptr();
        extract_edges(&mask, &mut out);
        assert_eq!(out.len(), MAX_EDGE_POINTS, "must clamp at capacity");
        assert_eq!(out.capacity(), MAX_EDGE_POINTS, "must never grow");
        assert_eq!(out.as_ptr(), ptr, "must never reallocate");
    }

    #[test]
    fn refill_clears_previous_points() {
        let mask_a = disc(Vec2::new(128.0, 128.0), 40.0);
        let empty = vec![0.0_f32; MASK_SIZE * MASK_SIZE];
        let mut out = Vec::with_capacity(MAX_EDGE_POINTS);
        extract_edges(&mask_a, &mut out);
        assert!(!out.is_empty());
        extract_edges(&empty, &mut out);
        assert!(out.is_empty(), "clear-refill semantics");
    }
}
