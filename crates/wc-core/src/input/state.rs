//! Hand-tracking resource + message types.
//!
//! Mirrors the shape of Bevy's `Res<Touches>` for continuous data and
//! `Messages<TouchInput>` for raw events.

use std::time::Duration;

use bevy::prelude::*;
use bevy::reflect::Reflect;
use smallvec::SmallVec;
use thiserror::Error;

use super::hand::Hand;

/// Maximum number of hands a provider is expected to report. Both Leap Motion
/// and `MediaPipe` Hands track at most two hands; this matches the hardware
/// reality and keeps the `SmallVec` inline.
pub const MAX_HANDS: usize = 2;

/// Current snapshot of all active hands.
///
/// Updated each `PreUpdate` by [`crate::input::systems::update_hand_tracking_state`]
/// from the latest [`HandTrackingFrame`].
#[derive(Resource, Default, Debug, Clone)]
pub struct HandTrackingState {
    /// Active hands as of [`Self::timestamp`]. Provider order; do not assume
    /// left-first.
    hands: SmallVec<[Hand; MAX_HANDS]>,
    /// Time the frame was captured by the provider (provider-relative clock).
    timestamp: Duration,
}

impl HandTrackingState {
    /// How many hands are currently being tracked.
    #[must_use]
    pub fn active_hand_count(&self) -> usize {
        self.hands.len()
    }

    /// `true` if no hands are being tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hands.is_empty()
    }

    /// Iterate over all active hands.
    pub fn iter(&self) -> impl Iterator<Item = &Hand> + '_ {
        self.hands.iter()
    }

    /// First hand with [`super::hand::Chirality::Left`], if any.
    #[must_use]
    pub fn left(&self) -> Option<&Hand> {
        self.hands
            .iter()
            .find(|h| matches!(h.chirality, super::hand::Chirality::Left))
    }

    /// First hand with [`super::hand::Chirality::Right`], if any.
    #[must_use]
    pub fn right(&self) -> Option<&Hand> {
        self.hands
            .iter()
            .find(|h| matches!(h.chirality, super::hand::Chirality::Right))
    }

    /// Time the latest frame was captured (provider-relative).
    #[must_use]
    pub fn timestamp(&self) -> Duration {
        self.timestamp
    }

    /// Replace the state with the contents of a frame. Called only by the
    /// `update_hand_tracking_state` system; not part of the public API.
    pub(crate) fn ingest(&mut self, frame: &HandTrackingFrame) {
        self.hands.clear();
        for hand in &frame.hands {
            self.hands.push(hand.clone());
        }
        self.timestamp = frame.timestamp;
    }
}

/// One raw frame from a [`super::provider::HandTrackingProvider`].
///
/// Emitted as a `Messages<HandTrackingFrame>` event by
/// [`crate::input::systems::poll_active_provider`]. Most systems consume the
/// derived [`HandTrackingState`] resource instead; raw frames are useful for
/// analytics, recording, and the lifecycle interaction-reset system.
#[derive(Message, Debug, Clone)]
pub struct HandTrackingFrame {
    /// Active hands in this frame.
    pub hands: SmallVec<[Hand; MAX_HANDS]>,
    /// Timestamp of the frame (provider-relative clock).
    pub timestamp: Duration,
}

/// Lifecycle status of the active hand-tracking provider.
///
/// Read by the UI status indicator (added in Plan 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Reflect, Default)]
pub enum HandTrackingStatus {
    /// Provider has not yet been started.
    #[default]
    NotStarted,
    /// Provider is starting up (e.g., negotiating with hardware).
    Connecting,
    /// Provider is producing frames.
    Connected,
    /// Provider terminated cleanly (hardware unplugged, user-requested stop).
    Disconnected,
    /// Provider hit an error and stopped.
    Errored,
}

/// Error returned from provider lifecycle methods (`start`).
#[derive(Debug, Error)]
pub enum HandTrackingError {
    /// Provider could not access its hardware/transport.
    #[error("provider unavailable: {0}")]
    Unavailable(String),
    /// Provider was misconfigured.
    #[error("provider configuration error: {0}")]
    Misconfigured(String),
    /// Catch-all for other provider failures.
    #[error("provider error: {0}")]
    Other(String),
}

// Compile-time check: HandTrackingError must be Send + Sync + 'static so it
// can flow across thread boundaries (audio thread provider start, future
// async work). The trait bounds on `HandTrackingProvider` require this; the
// const here makes the requirement explicit and catches accidental
// non-`Send` field additions at compile time.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync + 'static>() {}
    assert_send_sync::<HandTrackingError>();
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::hand::{Chirality, LANDMARK_COUNT};

    fn fake_hand(id: u32, chirality: Chirality) -> Hand {
        Hand {
            id,
            chirality,
            palm_position: Vec3::ZERO,
            palm_normal: Vec3::Y,
            palm_velocity: Vec3::ZERO,
            pinch_strength: 0.0,
            grab_strength: 0.0,
            landmarks: [Vec3::ZERO; LANDMARK_COUNT],
        }
    }

    #[test]
    fn empty_state_is_empty() {
        let state = HandTrackingState::default();
        assert!(state.is_empty());
        assert_eq!(state.active_hand_count(), 0);
        assert_eq!(state.left(), None);
        assert_eq!(state.right(), None);
    }

    #[test]
    fn ingest_replaces_hands_and_timestamp() {
        let mut state = HandTrackingState::default();
        let frame = HandTrackingFrame {
            hands: smallvec::smallvec![fake_hand(1, Chirality::Left)],
            timestamp: Duration::from_millis(500),
        };
        state.ingest(&frame);
        assert_eq!(state.active_hand_count(), 1);
        assert_eq!(state.timestamp(), Duration::from_millis(500));
        assert!(state.left().is_some());
        assert!(state.right().is_none());

        // Ingest a frame with a different hand; previous one is dropped.
        let frame2 = HandTrackingFrame {
            hands: smallvec::smallvec![fake_hand(2, Chirality::Right)],
            timestamp: Duration::from_millis(800),
        };
        state.ingest(&frame2);
        assert_eq!(state.active_hand_count(), 1);
        assert!(state.left().is_none());
        assert!(state.right().is_some());
    }
}
