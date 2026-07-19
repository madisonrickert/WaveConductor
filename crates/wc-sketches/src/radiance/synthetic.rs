//! Deterministic synthetic body performer.
//!
//! One generator, three consumers (the spec's testing keystone):
//!
//! - unit tests (mask/edge math, below);
//! - the attract-mode phantom (`screensaver.rs`) — a slow drifting ellipse
//!   cluster;
//! - the capture scenarios' dancer (`systems/debug.rs`) — the same cluster
//!   with larger, faster limb swings plus synthetic landmarks/audio.
//!
//! Everything is a pure function of `t` (virtual seconds), so fixed-dt
//! captures are reproducible frame-for-frame. Mask space is the pinned
//! contract's: 256×256 **`Rgba8Unorm`** (channel `i` = body slot `i`), UV
//! origin top-left, y down. The synthetic writers drive a single body in
//! **slot 0**, so they rasterize into channel R and zero the other channels.

use bevy::prelude::*;
use wc_core::audio::input::AudioAnalysis;
use wc_core::input::body::{EdgePoint, MASK_BYTES, MASK_CHANNELS, MASK_SIZE, MAX_EDGE_POINTS};

/// One soft ellipse blob in mask-UV space.
#[derive(Clone, Copy, Debug)]
pub struct Ellipse {
    /// Center in mask UV (0..1, y down).
    pub center: Vec2,
    /// Semi-axes in mask UV.
    pub radii: Vec2,
}

/// A phantom body: six blobs (head, torso, two arms, two legs).
#[derive(Clone, Copy, Debug)]
pub struct PhantomPose {
    /// Blob cluster; the union rasterizes into the silhouette.
    pub blobs: [Ellipse; 6],
}

/// Blob indices (documented so limb landmarks can anchor to them).
pub const BLOB_HEAD: usize = 0;
/// See [`BLOB_HEAD`].
pub const BLOB_TORSO: usize = 1;
/// See [`BLOB_HEAD`].
pub const BLOB_ARM_L: usize = 2;
/// See [`BLOB_HEAD`].
pub const BLOB_ARM_R: usize = 3;
/// See [`BLOB_HEAD`].
pub const BLOB_LEG_L: usize = 4;
/// See [`BLOB_HEAD`].
pub const BLOB_LEG_R: usize = 5;

/// Build the pose at time `t` with the given sway/limb amplitudes around a
/// horizontal centre `cx0`. `sway_amp` ~0.05 reads as an idle drift;
/// `limb_amp` ~0.09 as dancing.
#[must_use]
#[allow(
    clippy::similar_names,
    reason = "arm_l_swing/arm_r_swing are the paired left/right limb swings; \
              renaming one hurts the symmetry the six-blob layout relies on"
)]
fn pose_at(t: f32, sway_amp: f32, limb_amp: f32, cx0: f32) -> PhantomPose {
    let sway = (t * 0.35).sin() * sway_amp;
    let bob = (t * 0.9).sin() * 0.015;
    let cx = cx0 + sway;
    let arm_l_swing = (t * 0.8).sin() * limb_amp;
    let arm_r_swing = (t * 0.8 + 2.1).sin() * limb_amp;
    let leg_shift = (t * 0.5).sin() * limb_amp * 0.4;
    PhantomPose {
        blobs: [
            // Head.
            Ellipse {
                center: Vec2::new(cx + sway * 0.4, 0.30 + bob),
                radii: Vec2::new(0.055, 0.065),
            },
            // Torso.
            Ellipse {
                center: Vec2::new(cx, 0.52 + bob),
                radii: Vec2::new(0.09, 0.16),
            },
            // Arms (vertical-ish blobs swinging outward from the shoulders).
            Ellipse {
                center: Vec2::new(cx - 0.13 - arm_l_swing.abs(), 0.46 + arm_l_swing),
                radii: Vec2::new(0.035, 0.11),
            },
            Ellipse {
                center: Vec2::new(cx + 0.13 + arm_r_swing.abs(), 0.46 + arm_r_swing),
                radii: Vec2::new(0.035, 0.11),
            },
            // Legs.
            Ellipse {
                center: Vec2::new(cx - 0.05 + leg_shift, 0.76 + bob),
                radii: Vec2::new(0.045, 0.14),
            },
            Ellipse {
                center: Vec2::new(cx + 0.05 - leg_shift, 0.76 + bob),
                radii: Vec2::new(0.045, 0.14),
            },
        ],
    }
}

/// The attract phantom: slow drift, small limb motion.
#[must_use]
pub fn phantom_pose(t: f32) -> PhantomPose {
    pose_at(t, 0.05, 0.03, 0.5)
}

/// The capture dancer: bigger sway and limb swings (still deterministic).
/// The tempo/amplitude pair is chosen so the wrist-tip velocities cross the
/// sparkle tracker's ±[`crate::radiance::sparkle::FLIP_HYSTERESIS_UV_S`]
/// band — captures exercise the limb-mote layer, not just the flame.
#[must_use]
pub fn dancing_pose(t: f32) -> PhantomPose {
    pose_at(t * 2.2, 0.06, 0.14, 0.5)
}

/// Virtual seconds at which the duo scenario's second dancer enters frame
/// (frame 120 at the capture harness's fixed 1/60 s step).
pub const DUO_ENTRY_T: f32 = 2.0;
/// Seconds the second dancer's synthetic fade envelope takes to reach 1.
pub const DUO_FADE_IN_S: f32 = 0.8;

/// Duo mode's primary dancer (slot 0): the capture dancer shifted left so
/// both figures fit side by side.
#[must_use]
pub fn duo_primary_pose(t: f32) -> PhantomPose {
    pose_at(t * 1.6, 0.05, 0.09, 0.36)
}

/// Duo mode's second dancer (slot 1): offset phase + slightly different
/// tempo on the right, so the pair never moves in lockstep.
#[must_use]
pub fn duo_partner_pose(t: f32) -> PhantomPose {
    pose_at(t * 1.45 + 5.3, 0.05, 0.085, 0.66)
}

/// The second dancer's synthetic fade envelope at time `t`: 0 before
/// [`DUO_ENTRY_T`], ramping to 1 over [`DUO_FADE_IN_S`] (a linear stand-in
/// for the real tracker's attack envelope — enough to exercise the ignite
/// flare and the fade-riding render paths in captures).
#[must_use]
pub fn duo_partner_fade(t: f32) -> f32 {
    ((t - DUO_ENTRY_T) / DUO_FADE_IN_S).clamp(0.0, 1.0)
}

/// Approximate landmark UVs for the seven impulse landmarks (nose, wrists,
/// hips, ankles), anchored to the pose's blobs. Order matches
/// `radiance::systems::sim_params::IMPULSE_LANDMARKS`.
#[must_use]
pub fn dancer_landmark_uv(pose: &PhantomPose) -> [Vec2; 7] {
    let head = pose.blobs[BLOB_HEAD].center;
    let arm_l = pose.blobs[BLOB_ARM_L];
    let arm_r = pose.blobs[BLOB_ARM_R];
    let torso = pose.blobs[BLOB_TORSO];
    let leg_l = pose.blobs[BLOB_LEG_L];
    let leg_r = pose.blobs[BLOB_LEG_R];
    [
        head,                                           // nose
        arm_l.center + Vec2::new(0.0, arm_l.radii.y),   // left wrist (arm tip)
        arm_r.center + Vec2::new(0.0, arm_r.radii.y),   // right wrist
        torso.center + Vec2::new(-torso.radii.x, 0.10), // left hip
        torso.center + Vec2::new(torso.radii.x, 0.10),  // right hip
        leg_l.center + Vec2::new(0.0, leg_l.radii.y),   // left ankle
        leg_r.center + Vec2::new(0.0, leg_r.radii.y),   // right ankle
    ]
}

/// Rasterize up to four poses' smooth-union coverage into their slot
/// channels of an RGBA-interleaved `MASK_BYTES` buffer (255 inside, 0
/// outside, a soft band at the boundary — matching the EMA-softened real
/// mask). Absent slots are zeroed so a stale multi-body frame never ghosts
/// behind the synthetic dancers. `out.len()` must be [`MASK_BYTES`].
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "pixel-loop index/value conversions on bounded 0..256 / 0..1 values"
)]
#[allow(
    clippy::many_single_char_names,
    reason = "u/v/p/q/f/c mirror the field formula in the comment below \
              (f = |(p-c)/r|²) and the WGSL shader math it is the CPU twin of"
)]
pub fn rasterize_mask_slots(poses: [Option<&PhantomPose>; MASK_CHANNELS], out: &mut [u8]) {
    debug_assert_eq!(out.len(), MASK_BYTES);
    let inv = 1.0 / MASK_SIZE as f32;
    for y in 0..MASK_SIZE {
        let v = (y as f32 + 0.5) * inv;
        for x in 0..MASK_SIZE {
            let u = (x as f32 + 0.5) * inv;
            let p = Vec2::new(u, v);
            let base = (y * MASK_SIZE + x) * MASK_CHANNELS;
            for (channel, pose) in poses.iter().enumerate() {
                let Some(pose) = pose else {
                    out[base + channel] = 0;
                    continue;
                };
                // Max coverage over blobs; each blob's normalized squared
                // field f = |(p-c)/r|² crosses 1 at the boundary; a
                // smoothstep band (0.85..1.15) softens it.
                let mut cov = 0.0_f32;
                for blob in &pose.blobs {
                    let q = (p - blob.center) / blob.radii;
                    let f = q.length_squared();
                    let c = 1.0 - smoothstep(0.85, 1.15, f);
                    cov = cov.max(c);
                }
                out[base + channel] = (cov * 255.0) as u8;
            }
        }
    }
}

/// Single-body convenience: rasterize one pose into the **slot-0 channel**
/// and zero the rest (the screensaver phantom's shape).
pub fn rasterize_mask(pose: &PhantomPose, out: &mut [u8]) {
    rasterize_mask_slots([Some(pose), None, None, None], out);
}

/// Scalar smoothstep (WGSL semantics).
#[must_use]
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Threshold used by [`extract_edges`] — the byte form of the contract's 0.5
/// mask crossing.
pub const EDGE_THRESHOLD: u8 = 128;

/// Extract one channel's `(position, outward normal)` pairs where the RGBA
/// mask crosses [`EDGE_THRESHOLD`], **appending** to `out` until the shared
/// [`MAX_EDGE_POINTS`] cap. Returns the number of points appended. Same
/// single-pass scan shape as Plan B's worker-side extractor: an inside pixel
/// with any 4-neighbor outside is a boundary pixel; the outward normal is the
/// negated central-difference gradient (the mask is high inside, so the
/// gradient points inward).
#[allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "pixel index -> UV conversion on bounded 0..256 values"
)]
pub fn extract_edges_channel(mask: &[u8], channel: usize, out: &mut Vec<EdgePoint>) -> usize {
    debug_assert_eq!(mask.len(), MASK_BYTES);
    debug_assert!(channel < MASK_CHANNELS);
    let before = out.len();
    let inv = 1.0 / MASK_SIZE as f32;
    let at = |x: usize, y: usize| mask[(y * MASK_SIZE + x) * MASK_CHANNELS + channel];
    for y in 1..MASK_SIZE - 1 {
        for x in 1..MASK_SIZE - 1 {
            if at(x, y) < EDGE_THRESHOLD {
                continue;
            }
            let inside = |v: u8| v >= EDGE_THRESHOLD;
            let boundary = !inside(at(x - 1, y))
                || !inside(at(x + 1, y))
                || !inside(at(x, y - 1))
                || !inside(at(x, y + 1));
            if !boundary {
                continue;
            }
            // Central-difference gradient (points toward higher = inward).
            let gx = f32::from(at(x + 1, y)) - f32::from(at(x - 1, y));
            let gy = f32::from(at(x, y + 1)) - f32::from(at(x, y - 1));
            let g = Vec2::new(gx, gy);
            let len = g.length();
            if len < 1e-3 {
                continue; // flat plateau artifact; no meaningful normal
            }
            let normal = -g / len;
            out.push(EdgePoint {
                pos: Vec2::new((x as f32 + 0.5) * inv, (y as f32 + 0.5) * inv),
                normal,
            });
            if out.len() >= MAX_EDGE_POINTS {
                return out.len() - before;
            }
        }
    }
    out.len() - before
}

/// Extract every channel's edges into the contract's slot-ordered
/// concatenated layout: `out` is cleared, then each slot's boundary points
/// are appended in ascending slot order (earlier slots fill first when the
/// shared cap crowds out later ones) and `slot_counts` partitions the list.
pub fn extract_edges_slots(
    mask: &[u8],
    out: &mut Vec<EdgePoint>,
    slot_counts: &mut [usize; MASK_CHANNELS],
) {
    out.clear();
    for (channel, count) in slot_counts.iter_mut().enumerate() {
        *count = extract_edges_channel(mask, channel, out);
    }
}

/// Single-body convenience: slot-0 edges only, clearing `out` first (the
/// screensaver phantom's shape and the original single-dancer extractor).
pub fn extract_edges(mask: &[u8], out: &mut Vec<EdgePoint>) {
    out.clear();
    extract_edges_channel(mask, 0, out);
}

/// Deterministic synthetic analysis frame for the capture dancer: a slow
/// bass swell, a high-band shimmer, and a 2 Hz onset "beat".
///
/// Deviation from the brief: `AudioAnalysis` has since grown a `peak` field
/// (a decaying peak-hold level, additive beyond the pinned contract) that
/// the brief's literal predates. `peak` is derived here as `rms` bumped by
/// half the onset envelope and clamped — a peak-hold should sit at or above
/// `rms` and spike on the click, same as the real capture path.
#[must_use]
pub fn synthetic_audio(t: f32) -> AudioAnalysis {
    let bass = 0.5 + 0.4 * (t * 1.1).sin();
    let high = 0.35 + 0.3 * (t * 3.7).sin();
    // A short raised-cosine click twice a second.
    let beat_phase = (t * 2.0).fract();
    let onset = if beat_phase < 0.08 {
        1.2 * (1.0 - beat_phase / 0.08)
    } else {
        0.0
    };
    // Beat confidence mirrors the real analysis lane's shape: snap to 1.0 on
    // the beat and decay with a 0.3 s time constant. At 2 Hz it falls to
    // ~0.19 between beats, so every synthetic beat produces exactly one
    // rising edge through the pulse layer's 0.6 threshold — captures
    // exercise the contour-wave layer. (The old constant 0.8 never re-armed
    // the edge, so pulses were invisible in every capture.)
    let since_beat = beat_phase / 2.0;
    let beat_confidence = (-since_beat / 0.3).exp();
    let rms = 0.35 + 0.25 * bass;
    AudioAnalysis {
        rms,
        gain: 1.0,
        bands: [
            bass,
            bass * 0.8,
            bass * 0.6,
            0.3,
            0.3,
            high * 0.8,
            high,
            high * 0.9,
        ],
        onset,
        beat_confidence,
        peak: (rms + onset * 0.5).min(1.2),
        active: true,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;

    /// Same `t` → bit-identical mask (the determinism captures depend on).
    #[test]
    fn rasterize_is_deterministic() {
        let pose = dancing_pose(3.25);
        let mut a = vec![0u8; MASK_BYTES];
        let mut b = vec![0u8; MASK_BYTES];
        rasterize_mask(&pose, &mut a);
        rasterize_mask(&pose, &mut b);
        assert_eq!(a, b);
        // Slot-0 channel carries the body; the other channels stay dark.
        assert!(
            a.chunks_exact(MASK_CHANNELS)
                .any(|t| t[0] >= EDGE_THRESHOLD),
            "body present in channel R"
        );
        assert!(
            a.chunks_exact(MASK_CHANNELS)
                .all(|t| t[1] == 0 && t[2] == 0 && t[3] == 0),
            "unused slot channels stay dark"
        );
        assert!(a.contains(&0), "background present");
    }

    /// A single centered circle: edge count ≈ its pixel circumference, every
    /// normal points away from the center, all positions in the edge band.
    #[test]
    fn circle_edges_point_outward() {
        let pose = PhantomPose {
            blobs: [Ellipse {
                center: Vec2::new(0.5, 0.5),
                radii: Vec2::new(0.2, 0.2),
            }; 6],
        };
        let mut mask = vec![0u8; MASK_BYTES];
        rasterize_mask(&pose, &mut mask);
        let mut edges = Vec::with_capacity(MAX_EDGE_POINTS);
        extract_edges(&mask, &mut edges);
        // r = 0.2 * 256 ≈ 51 px → circumference ≈ 322 px of boundary.
        assert!(
            edges.len() > 200 && edges.len() < 800,
            "got {}",
            edges.len()
        );
        for e in &edges {
            let pos = e.pos;
            let n = e.normal;
            assert!((n.length() - 1.0).abs() < 1e-3, "unit normal");
            assert!(
                (pos - Vec2::new(0.5, 0.5)).dot(n) > 0.0,
                "outward at {pos:?} n {n:?}"
            );
            let r = (pos - Vec2::new(0.5, 0.5)).length();
            assert!((r - 0.2).abs() < 0.03, "on the rim: r = {r}");
        }
    }

    /// A stripe pattern with far more boundary pixels than the cap clamps to
    /// exactly `MAX_EDGE_POINTS`.
    #[test]
    fn extraction_clamps_to_capacity() {
        let mut mask = vec![0u8; MASK_BYTES];
        for y in 0..MASK_SIZE {
            for x in 0..MASK_SIZE {
                // 4-px stripes with soft 1-px ramps so gradients are nonzero.
                let phase = x % 8;
                mask[(y * MASK_SIZE + x) * MASK_CHANNELS] = match phase {
                    0 | 4 => 64,
                    1..=3 => 255,
                    _ => 0,
                };
            }
        }
        let mut edges = Vec::with_capacity(MAX_EDGE_POINTS);
        extract_edges(&mask, &mut edges);
        assert_eq!(edges.len(), MAX_EDGE_POINTS);
    }

    /// The dancer's landmarks stay inside the mask frame and move over time
    /// (finite differences are nonzero → real impulse velocities).
    #[test]
    fn dancer_landmarks_move_in_bounds() {
        let a = dancer_landmark_uv(&dancing_pose(1.0));
        let b = dancer_landmark_uv(&dancing_pose(1.1));
        let mut moved = 0;
        for (pa, pb) in a.iter().zip(&b) {
            assert!(pa.x > 0.0 && pa.x < 1.0 && pa.y > 0.0 && pa.y < 1.0);
            if pa.distance(*pb) > 1e-4 {
                moved += 1;
            }
        }
        assert!(moved >= 4, "limbs must actually dance ({moved} moved)");
    }

    /// Duo mode: the two dancers land in channels R and G with disjoint
    /// horizontal territories, and the slot extractor partitions the
    /// concatenated edge list correctly.
    #[test]
    fn duo_masks_fill_separate_channels() {
        let p0 = duo_primary_pose(3.0);
        let p1 = duo_partner_pose(3.0);
        let mut mask = vec![0u8; MASK_BYTES];
        rasterize_mask_slots([Some(&p0), Some(&p1), None, None], &mut mask);
        assert!(
            mask.chunks_exact(MASK_CHANNELS)
                .any(|t| t[0] >= EDGE_THRESHOLD),
            "primary in channel R"
        );
        assert!(
            mask.chunks_exact(MASK_CHANNELS)
                .any(|t| t[1] >= EDGE_THRESHOLD),
            "partner in channel G"
        );
        assert!(
            mask.chunks_exact(MASK_CHANNELS)
                .all(|t| t[2] == 0 && t[3] == 0),
            "unused slot channels stay dark"
        );

        let mut points = Vec::with_capacity(MAX_EDGE_POINTS);
        let mut counts = [0usize; MASK_CHANNELS];
        extract_edges_slots(&mask, &mut points, &mut counts);
        assert!(counts[0] > 100, "primary has a rim: {counts:?}");
        assert!(counts[1] > 100, "partner has a rim: {counts:?}");
        assert_eq!(counts[2], 0);
        assert_eq!(points.len(), counts.iter().sum::<usize>());
        // Slot ranges hold the right bodies: slot 0 (cx 0.36) sits left of
        // slot 1 (cx 0.66) on average.
        let mean_x = |range: std::ops::Range<usize>| {
            let n = range.len().max(1);
            #[allow(
                clippy::as_conversions,
                clippy::cast_precision_loss,
                reason = "edge counts < 2048, exact in f32"
            )]
            let inv = 1.0 / n as f32;
            points[range].iter().map(|e| e.pos.x).sum::<f32>() * inv
        };
        let x0 = mean_x(0..counts[0]);
        let x1 = mean_x(counts[0]..counts[0] + counts[1]);
        assert!(x0 < 0.5 && x1 > 0.5, "territories: {x0} vs {x1}");
    }

    /// The duo partner's synthetic fade: 0 before entry, ramping to 1 over
    /// the fade-in window (the ignite-flare capture hook).
    #[test]
    fn duo_partner_fade_ramps_after_entry() {
        assert!(duo_partner_fade(0.0).abs() < f32::EPSILON);
        assert!(duo_partner_fade(DUO_ENTRY_T - 0.01).abs() < f32::EPSILON);
        let mid = duo_partner_fade(DUO_ENTRY_T + DUO_FADE_IN_S * 0.5);
        assert!((mid - 0.5).abs() < 1e-3, "{mid}");
        assert!((duo_partner_fade(DUO_ENTRY_T + DUO_FADE_IN_S) - 1.0).abs() < f32::EPSILON);
        assert!((duo_partner_fade(100.0) - 1.0).abs() < f32::EPSILON);
    }

    /// Synthetic audio is deterministic and periodically produces onsets.
    #[test]
    fn synthetic_audio_is_deterministic_with_beats() {
        assert_eq!(synthetic_audio(2.0), synthetic_audio(2.0));
        let on_beat = synthetic_audio(1.0); // beat_phase 0 → onset peak
        assert!(on_beat.onset > 1.0);
        let off_beat = synthetic_audio(1.25);
        assert!(off_beat.onset.abs() < f32::EPSILON);
        assert!(on_beat.active);
    }
}
