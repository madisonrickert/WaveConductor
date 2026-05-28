//! Synthetic hand fixtures for exercising hand-driven visuals without hardware.
//!
//! [`synthetic_open_hand`] returns a stationary, anatomically-plausible open
//! right hand. It backs
//! [`crate::input::providers::mock::MockProvider::synthetic_hand`] (selected at
//! runtime with `WAVECONDUCTOR_HAND_PROVIDER=synthetic`) and is also useful
//! directly in tests that need a realistic [`Hand`] — e.g. verifying bone-mesh
//! rendering, the Line hand-attractor, or future per-hand gesture sketches when
//! no Leap device is attached.
//!
//! ## Coordinate convention
//!
//! Landmark positions are authored in the **Leap device millimetre** convention
//! consumed downstream by [`crate::input::projection::palm_to_world`] and the
//! bone-centre derivation in [`crate::input::systems`]: origin at the device
//! centre, +X right, +Y up (height above the sensor), +Z toward the user (Z is
//! unused for on-screen projection). Values sit inside the usable Leap range
//! (X ∈ [-200, 200] mm, Y ∈ [40, 350] mm) so the projected hand fills the
//! centre of the viewport rather than collapsing toward the origin.

use std::time::Duration;

use bevy::math::Vec3;
use smallvec::smallvec;

use super::hand::{Chirality, Hand, LandmarkIndex, LANDMARK_COUNT};
use super::provider::ProviderId;
use super::state::HandTrackingFrame;

/// Build a stationary open right hand: wrist low and centred, the five digits
/// fanned upward and outward in a relaxed open pose.
///
/// Positions are in Leap device millimetres (see the module-level coordinate
/// note). The mock/fusion path derives the 20 bone centres from these 21
/// landmarks via midpoints, so the pose is what determines where the
/// `hand_mesh` bone spheres land on screen.
#[must_use]
pub fn synthetic_open_hand() -> Hand {
    use LandmarkIndex as L;

    // (landmark, x_mm, y_mm). Z is 0 for every joint (ignored by projection).
    let pose: [(L, f32, f32); LANDMARK_COUNT] = [
        (L::Wrist, 0.0, 50.0),
        // Thumb — angled out to the left of a right hand.
        (L::ThumbCmc, -50.0, 80.0),
        (L::ThumbMcp, -85.0, 110.0),
        (L::ThumbIp, -110.0, 140.0),
        (L::ThumbTip, -128.0, 165.0),
        // Index.
        (L::IndexMcp, -48.0, 160.0),
        (L::IndexPip, -52.0, 210.0),
        (L::IndexDip, -54.0, 245.0),
        (L::IndexTip, -56.0, 275.0),
        // Middle.
        (L::MiddleMcp, -14.0, 172.0),
        (L::MiddlePip, -15.0, 226.0),
        (L::MiddleDip, -16.0, 264.0),
        (L::MiddleTip, -17.0, 298.0),
        // Ring.
        (L::RingMcp, 20.0, 168.0),
        (L::RingPip, 22.0, 220.0),
        (L::RingDip, 23.0, 256.0),
        (L::RingTip, 24.0, 288.0),
        // Pinky.
        (L::PinkyMcp, 52.0, 156.0),
        (L::PinkyPip, 55.0, 200.0),
        (L::PinkyDip, 57.0, 232.0),
        (L::PinkyTip, 58.0, 260.0),
    ];

    let mut landmarks = [Vec3::ZERO; LANDMARK_COUNT];
    for (idx, x, y) in pose {
        landmarks[idx.as_index()] = Vec3::new(x, y, 0.0);
    }

    Hand {
        id: 0,
        chirality: Chirality::Right,
        // Palm centroid roughly at the middle of the fanned digits.
        palm_position: Vec3::new(-10.0, 150.0, 0.0),
        // Palm faces the user (+Z).
        palm_normal: Vec3::Z,
        palm_velocity: Vec3::ZERO,
        pinch_strength: 0.0,
        grab_strength: 0.0,
        landmarks,
    }
}

/// Wrap [`synthetic_open_hand`] in a single-hand [`HandTrackingFrame`] stamped
/// at `timestamp`.
#[must_use]
pub fn synthetic_hand_frame(timestamp: Duration) -> HandTrackingFrame {
    HandTrackingFrame {
        provider: ProviderId::Mock,
        hands: smallvec![synthetic_open_hand()],
        timestamp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_hand_fills_all_landmarks_within_leap_range() {
        let hand = synthetic_open_hand();
        // Every landmark sits inside the usable Leap volume so it projects to
        // the centre of the viewport rather than clamping to an edge.
        for (i, lm) in hand.landmarks.iter().enumerate() {
            assert!(
                (-200.0..=200.0).contains(&lm.x),
                "landmark {i} x={} out of Leap X range",
                lm.x
            );
            assert!(
                (40.0..=350.0).contains(&lm.y),
                "landmark {i} y={} out of Leap Y range",
                lm.y
            );
        }
        assert_eq!(hand.chirality, Chirality::Right);
    }

    #[test]
    fn hand_frame_carries_one_hand_with_given_timestamp() {
        let frame = synthetic_hand_frame(Duration::from_millis(42));
        assert_eq!(frame.hands.len(), 1);
        assert_eq!(frame.timestamp, Duration::from_millis(42));
        assert_eq!(frame.provider, ProviderId::Mock);
    }
}
