//! Hand and landmark data types.
//!
//! The canonical landmark layout is `MediaPipe`'s 21-landmark scheme. When the
//! [`crate::input::providers::leap_native`] provider lands, it converts Leap
//! Motion's richer bone data into this 21-landmark form. This unification means
//! sketches written against the [`Hand`] type work against any provider.

use bevy::prelude::*;
use bevy::reflect::Reflect;

/// Number of landmarks per hand. Matches `MediaPipe` Hands' canonical layout.
pub const LANDMARK_COUNT: usize = 21;

/// Indices into the [`Hand::landmarks`] array, by anatomical role.
///
/// The naming and ordering match `MediaPipe` Hands (CMC = carpometacarpal, MCP =
/// metacarpophalangeal, PIP = proximal interphalangeal, DIP = distal
/// interphalangeal, IP = interphalangeal, TIP = fingertip).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Reflect)]
#[repr(usize)]
pub enum LandmarkIndex {
    /// Wrist joint.
    Wrist = 0,
    /// Thumb carpometacarpal joint.
    ThumbCmc = 1,
    /// Thumb metacarpophalangeal joint.
    ThumbMcp = 2,
    /// Thumb interphalangeal joint.
    ThumbIp = 3,
    /// Thumb tip.
    ThumbTip = 4,
    /// Index finger metacarpophalangeal joint.
    IndexMcp = 5,
    /// Index finger proximal interphalangeal joint.
    IndexPip = 6,
    /// Index finger distal interphalangeal joint.
    IndexDip = 7,
    /// Index finger tip.
    IndexTip = 8,
    /// Middle finger metacarpophalangeal joint.
    MiddleMcp = 9,
    /// Middle finger proximal interphalangeal joint.
    MiddlePip = 10,
    /// Middle finger distal interphalangeal joint.
    MiddleDip = 11,
    /// Middle finger tip.
    MiddleTip = 12,
    /// Ring finger metacarpophalangeal joint.
    RingMcp = 13,
    /// Ring finger proximal interphalangeal joint.
    RingPip = 14,
    /// Ring finger distal interphalangeal joint.
    RingDip = 15,
    /// Ring finger tip.
    RingTip = 16,
    /// Pinky finger metacarpophalangeal joint.
    PinkyMcp = 17,
    /// Pinky finger proximal interphalangeal joint.
    PinkyPip = 18,
    /// Pinky finger distal interphalangeal joint.
    PinkyDip = 19,
    /// Pinky finger tip.
    PinkyTip = 20,
}

impl LandmarkIndex {
    /// The raw `usize` index into [`Hand::landmarks`].
    #[must_use]
    #[allow(clippy::as_conversions)]
    // Safety: #[repr(usize)] guarantees the cast is exact for all variants.
    pub const fn as_index(self) -> usize {
        self as usize
    }
}

/// Which hand the [`Hand`] represents. Provider-reported; not inferred from
/// data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Reflect, Component)]
pub enum Chirality {
    /// Left hand.
    Left,
    /// Right hand.
    Right,
}

/// A single tracked hand at one point in time.
///
/// Positions (`palm_position`, `landmarks`) are in **Leap-device millimetres**,
/// the convention every consumer expects (see
/// [`crate::input::projection::palm_to_world`], which maps it to world space):
/// x in `[-200, +200]`, y as height-above-device in `[40, 350]`, z signed depth.
/// Providers project their native spaces into this layout — the Leap provider
/// passes LeapC millimetres through; the `MediaPipe` provider maps normalized
/// image coordinates into the same convention via its `coords` module.
#[derive(Debug, Clone, PartialEq, Reflect)]
pub struct Hand {
    /// Provider-stable identifier across frames. Two consecutive frames with
    /// the same `id` mean "same physical hand". Identifiers may be reused
    /// after a hand leaves the tracking volume.
    pub id: u32,
    /// Which hand this is.
    pub chirality: Chirality,
    /// Palm centroid in normalized device coordinates.
    pub palm_position: Vec3,
    /// Unit vector normal to the palm, pointing away from the back of the hand.
    pub palm_normal: Vec3,
    /// Palm velocity, in NDC units per second. Provider-smoothed.
    pub palm_velocity: Vec3,
    /// `[0.0, 1.0]`. Provider-reported pinch (thumb–index proximity).
    pub pinch_strength: f32,
    /// `[0.0, 1.0]`. Provider-reported grab (fist closure).
    pub grab_strength: f32,
    /// 21 landmarks in `MediaPipe` Hands layout. See [`LandmarkIndex`].
    pub landmarks: [Vec3; LANDMARK_COUNT],
}

impl Hand {
    /// Convenience accessor for a landmark by anatomical role.
    #[must_use]
    pub fn landmark(&self, idx: LandmarkIndex) -> Vec3 {
        self.landmarks[idx.as_index()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landmark_indices_cover_full_array() {
        // Every variant maps to a unique, in-range index.
        let all = [
            LandmarkIndex::Wrist,
            LandmarkIndex::ThumbCmc,
            LandmarkIndex::ThumbMcp,
            LandmarkIndex::ThumbIp,
            LandmarkIndex::ThumbTip,
            LandmarkIndex::IndexMcp,
            LandmarkIndex::IndexPip,
            LandmarkIndex::IndexDip,
            LandmarkIndex::IndexTip,
            LandmarkIndex::MiddleMcp,
            LandmarkIndex::MiddlePip,
            LandmarkIndex::MiddleDip,
            LandmarkIndex::MiddleTip,
            LandmarkIndex::RingMcp,
            LandmarkIndex::RingPip,
            LandmarkIndex::RingDip,
            LandmarkIndex::RingTip,
            LandmarkIndex::PinkyMcp,
            LandmarkIndex::PinkyPip,
            LandmarkIndex::PinkyDip,
            LandmarkIndex::PinkyTip,
        ];
        assert_eq!(all.len(), LANDMARK_COUNT);
        for (expected_index, role) in all.iter().enumerate() {
            assert_eq!(role.as_index(), expected_index);
        }
    }

    #[test]
    fn landmark_accessor_returns_correct_position() {
        let mut landmarks = [Vec3::ZERO; LANDMARK_COUNT];
        landmarks[LandmarkIndex::IndexTip.as_index()] = Vec3::new(0.5, 0.25, -0.1);
        let hand = Hand {
            id: 1,
            chirality: Chirality::Right,
            palm_position: Vec3::ZERO,
            palm_normal: Vec3::Y,
            palm_velocity: Vec3::ZERO,
            pinch_strength: 0.0,
            grab_strength: 0.0,
            landmarks,
        };
        assert_eq!(
            hand.landmark(LandmarkIndex::IndexTip),
            Vec3::new(0.5, 0.25, -0.1)
        );
    }
}
