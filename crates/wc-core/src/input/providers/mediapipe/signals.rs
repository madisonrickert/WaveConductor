//! Derived per-hand signals from the 21 landmarks.
//!
//! The landmark model gives positions, presence, handedness, and world
//! landmarks; the [`crate::input::hand::Hand`] fields the rest of the app
//! consumes (`pinch_strength`, `grab_strength`, `palm_normal`, `palm_velocity`,
//! stable `id`) are derived here with documented, deterministic geometry. The
//! pinch/grab magnitudes are normalized by hand scale so they are roughly
//! distance-invariant; their exact thresholds are tuned against real hands
//! during the Phase 6/8 hardware validation.
//!
//! Foundation module: consumed by the pipeline (plan Phase 8); exercised by
//! tests until then.
#![allow(dead_code)]

use std::time::Duration;

use bevy::math::Vec3;

use crate::input::hand::{Chirality, LandmarkIndex, LANDMARK_COUNT};

/// Reference hand scale: wrist → middle-finger MCP distance. Used to normalize
/// pinch/grab so they don't change with the hand's distance from the camera.
#[must_use]
pub fn hand_scale(lm: &[Vec3; LANDMARK_COUNT]) -> f32 {
    let wrist = lm[LandmarkIndex::Wrist.as_index()];
    let middle_mcp = lm[LandmarkIndex::MiddleMcp.as_index()];
    wrist.distance(middle_mcp).max(f32::EPSILON)
}

/// Pinch strength in `[0, 1]`: thumb-tip ↔ index-tip proximity, normalized by
/// hand scale. `1.0` when the tips touch, falling to `0.0` once they are about
/// half a hand-scale apart.
#[must_use]
pub fn pinch_strength(lm: &[Vec3; LANDMARK_COUNT]) -> f32 {
    let thumb = lm[LandmarkIndex::ThumbTip.as_index()];
    let index = lm[LandmarkIndex::IndexTip.as_index()];
    let dist = thumb.distance(index) / hand_scale(lm);
    // dist 0 → 1.0; dist >= 0.5 → 0.0.
    (1.0 - dist / 0.5).clamp(0.0, 1.0)
}

/// Grab strength in `[0, 1]`: mean fingertip closure toward the palm centre,
/// normalized by hand scale. `0.0` for an open hand (tips extended ~one
/// hand-scale out), approaching `1.0` as the four fingers curl into a fist.
#[must_use]
pub fn grab_strength(lm: &[Vec3; LANDMARK_COUNT]) -> f32 {
    let palm = palm_center(lm);
    let scale = hand_scale(lm);
    let tips = [
        LandmarkIndex::IndexTip,
        LandmarkIndex::MiddleTip,
        LandmarkIndex::RingTip,
        LandmarkIndex::PinkyTip,
    ];
    let count = f32::from(u8::try_from(tips.len()).unwrap_or(1));
    let mean: f32 = tips
        .iter()
        .map(|t| lm[t.as_index()].distance(palm) / scale)
        .sum::<f32>()
        / count;
    // Open hand: mean ≈ 1.0 (tips a hand-scale out) → 0.0.
    // Fist: mean ≈ 0.3 (tips near palm) → ~1.0.
    ((1.0 - mean) / 0.7).clamp(0.0, 1.0)
}

/// Palm centre: centroid of the wrist and the index/pinky MCP knuckles.
#[must_use]
pub fn palm_center(lm: &[Vec3; LANDMARK_COUNT]) -> Vec3 {
    let wrist = lm[LandmarkIndex::Wrist.as_index()];
    let index_mcp = lm[LandmarkIndex::IndexMcp.as_index()];
    let pinky_mcp = lm[LandmarkIndex::PinkyMcp.as_index()];
    (wrist + index_mcp + pinky_mcp) / 3.0
}

/// Unit normal to the palm plane, from the wrist→index-MCP and wrist→pinky-MCP
/// edges. Points out of the palm. Chirality flips the sign so both hands' normals
/// agree with the Leap convention (away from the back of the hand).
#[must_use]
pub fn palm_normal(lm: &[Vec3; LANDMARK_COUNT], chirality: Chirality) -> Vec3 {
    let wrist = lm[LandmarkIndex::Wrist.as_index()];
    let index_mcp = lm[LandmarkIndex::IndexMcp.as_index()];
    let pinky_mcp = lm[LandmarkIndex::PinkyMcp.as_index()];
    let a = index_mcp - wrist;
    let b = pinky_mcp - wrist;
    let n = a.cross(b);
    let n = n.normalize_or_zero();
    match chirality {
        Chirality::Right => n,
        Chirality::Left => -n,
    }
}

/// Smoothed palm velocity (NDC-or-mm units per second), a finite difference of
/// successive palm positions over `dt`.
#[must_use]
pub fn palm_velocity(prev: Vec3, cur: Vec3, dt: Duration) -> Vec3 {
    let secs = dt.as_secs_f32();
    if secs <= 0.0 {
        Vec3::ZERO
    } else {
        (cur - prev) / secs
    }
}

/// Assigns stable per-hand IDs across frames.
///
/// `MediaPipe`'s landmark stage has no notion of track identity, so the provider
/// keeps its own: a detection inherits the ID of the previous frame's hand of
/// the same [`Chirality`] when their palm positions are within
/// [`Self::gate`]; otherwise it gets a fresh ID. Tracks not seen in a frame age
/// out, so IDs are reused only after a hand leaves.
#[derive(Debug)]
pub struct HandTracker {
    tracks: Vec<Track>,
    next_id: u32,
    /// Max palm-distance (same units as the positions) for two frames to count
    /// as the same hand.
    gate: f32,
}

#[derive(Debug, Clone, Copy)]
struct Track {
    id: u32,
    chirality: Chirality,
    pos: Vec3,
    seen_this_frame: bool,
}

impl Default for HandTracker {
    fn default() -> Self {
        // 60 mm in the Leap-device-mm convention: a hand won't jump that far
        // between consecutive frames, but two distinct hands are farther apart.
        Self {
            tracks: Vec::new(),
            next_id: 0,
            gate: 60.0,
        }
    }
}

impl HandTracker {
    /// Construct a tracker with a custom association gate.
    #[must_use]
    pub fn with_gate(gate: f32) -> Self {
        Self {
            gate,
            ..Self::default()
        }
    }

    /// Assign (or reuse) an ID for a hand of `chirality` at palm position `pos`.
    pub fn assign(&mut self, chirality: Chirality, pos: Vec3) -> u32 {
        // Nearest unclaimed track of the same chirality within the gate.
        let mut best: Option<(usize, f32)> = None;
        for (i, t) in self.tracks.iter().enumerate() {
            if t.seen_this_frame || t.chirality != chirality {
                continue;
            }
            let d = t.pos.distance(pos);
            if d <= self.gate && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
        if let Some((i, _)) = best {
            self.tracks[i].pos = pos;
            self.tracks[i].seen_this_frame = true;
            return self.tracks[i].id;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.tracks.push(Track {
            id,
            chirality,
            pos,
            seen_this_frame: true,
        });
        id
    }

    /// Call once per frame after all `assign` calls: drop tracks not seen this
    /// frame and reset the per-frame flags.
    pub fn end_frame(&mut self) {
        self.tracks.retain(|t| t.seen_this_frame);
        for t in &mut self.tracks {
            t.seen_this_frame = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An open right hand roughly in the XY plane: wrist at origin, fingers
    /// extended along +Y, thumb out along +X. Units are arbitrary but
    /// self-consistent.
    fn open_hand() -> [Vec3; LANDMARK_COUNT] {
        let mut lm = [Vec3::ZERO; LANDMARK_COUNT];
        lm[LandmarkIndex::Wrist.as_index()] = Vec3::new(0.0, 0.0, 0.0);
        // MCP knuckles across the palm.
        lm[LandmarkIndex::IndexMcp.as_index()] = Vec3::new(-0.3, 1.0, 0.0);
        lm[LandmarkIndex::MiddleMcp.as_index()] = Vec3::new(0.0, 1.0, 0.0);
        lm[LandmarkIndex::RingMcp.as_index()] = Vec3::new(0.3, 1.0, 0.0);
        lm[LandmarkIndex::PinkyMcp.as_index()] = Vec3::new(0.6, 1.0, 0.0);
        // Fingertips extended out to ~2.0 (about one hand-scale beyond the palm).
        lm[LandmarkIndex::IndexTip.as_index()] = Vec3::new(-0.3, 2.0, 0.0);
        lm[LandmarkIndex::MiddleTip.as_index()] = Vec3::new(0.0, 2.0, 0.0);
        lm[LandmarkIndex::RingTip.as_index()] = Vec3::new(0.3, 2.0, 0.0);
        lm[LandmarkIndex::PinkyTip.as_index()] = Vec3::new(0.6, 2.0, 0.0);
        // Thumb out to the side, tip far from the index tip.
        lm[LandmarkIndex::ThumbTip.as_index()] = Vec3::new(-1.2, 0.6, 0.0);
        lm
    }

    #[test]
    fn open_hand_has_low_pinch_and_grab() {
        let lm = open_hand();
        assert!(pinch_strength(&lm) < 0.2, "pinch={}", pinch_strength(&lm));
        assert!(grab_strength(&lm) < 0.2, "grab={}", grab_strength(&lm));
    }

    #[test]
    fn touching_thumb_and_index_reads_full_pinch() {
        let mut lm = open_hand();
        let p = Vec3::new(0.0, 1.5, 0.0);
        lm[LandmarkIndex::ThumbTip.as_index()] = p;
        lm[LandmarkIndex::IndexTip.as_index()] = p;
        assert!(pinch_strength(&lm) > 0.9, "pinch={}", pinch_strength(&lm));
    }

    #[test]
    fn curled_fingers_read_high_grab() {
        let mut lm = open_hand();
        // Curl the four fingertips back toward the palm centre.
        let palm = palm_center(&lm);
        for t in [
            LandmarkIndex::IndexTip,
            LandmarkIndex::MiddleTip,
            LandmarkIndex::RingTip,
            LandmarkIndex::PinkyTip,
        ] {
            lm[t.as_index()] = palm + Vec3::new(0.0, 0.1, 0.0);
        }
        assert!(grab_strength(&lm) > 0.8, "grab={}", grab_strength(&lm));
    }

    #[test]
    fn palm_normal_is_perpendicular_to_a_planar_hand() {
        let lm = open_hand();
        let n = palm_normal(&lm, Chirality::Right);
        // Hand lies in the XY plane → normal along ±Z.
        assert!(n.x.abs() < 1e-5 && n.y.abs() < 1e-5, "n={n:?}");
        assert!((n.z.abs() - 1.0).abs() < 1e-4, "n={n:?}");
    }

    #[test]
    fn left_and_right_palm_normals_oppose() {
        let lm = open_hand();
        let r = palm_normal(&lm, Chirality::Right);
        let l = palm_normal(&lm, Chirality::Left);
        assert!((r + l).length() < 1e-5, "r={r:?} l={l:?}");
    }

    #[test]
    fn velocity_is_finite_difference() {
        let v = palm_velocity(
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(10.0, 0.0, 0.0),
            Duration::from_millis(100),
        );
        assert!((v.x - 100.0).abs() < 1e-3, "v={v:?}");
        // Zero dt is safe.
        assert_eq!(
            palm_velocity(Vec3::ZERO, Vec3::ONE, Duration::ZERO),
            Vec3::ZERO
        );
    }

    #[test]
    fn tracker_keeps_id_for_nearby_same_chirality_hand() {
        let mut t = HandTracker::default();
        let id1 = t.assign(Chirality::Right, Vec3::new(0.0, 200.0, 0.0));
        t.end_frame();
        let id2 = t.assign(Chirality::Right, Vec3::new(5.0, 205.0, 0.0));
        assert_eq!(id1, id2);
    }

    #[test]
    fn tracker_gives_new_id_for_far_hand() {
        let mut t = HandTracker::default();
        let id1 = t.assign(Chirality::Right, Vec3::new(-200.0, 100.0, 0.0));
        t.end_frame();
        let id2 = t.assign(Chirality::Right, Vec3::new(200.0, 300.0, 0.0));
        assert_ne!(id1, id2);
    }

    #[test]
    fn tracker_separates_left_and_right_hands() {
        let mut t = HandTracker::default();
        let r = t.assign(Chirality::Right, Vec3::new(0.0, 0.0, 0.0));
        // Same position but opposite chirality → distinct ID.
        let l = t.assign(Chirality::Left, Vec3::new(0.0, 0.0, 0.0));
        assert_ne!(r, l);
    }
}
