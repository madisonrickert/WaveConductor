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

    /// Replace the state with the contents of a frame.
    ///
    /// Production write path is [`crate::input::systems::update_hand_tracking_state`]
    /// (driven from `Messages<HandTrackingFrame>`). Promoted to `pub` in Plan 11
    /// so integration tests can synthesize hand frames without a fake provider —
    /// see `crates/wc-sketches/tests/line_input.rs::hand_pinch_activates_mouse_attractor`.
    pub fn ingest(&mut self, frame: &HandTrackingFrame) {
        self.hands.clear();
        for hand in &frame.hands {
            self.hands.push(hand.clone());
        }
        self.timestamp = frame.timestamp;
    }
}

/// One raw frame from a [`super::provider::HandTrackingProvider`].
///
/// Emitted as `Messages<HandTrackingFrame>` by
/// [`crate::input::systems::poll_active_provider`]. Most systems consume the
/// derived [`HandTrackingState`] resource instead; raw frames are useful for
/// analytics, recording, and the lifecycle interaction-reset system.
#[derive(Message, Debug, Clone)]
pub struct HandTrackingFrame {
    /// Hands tracked in this frame, in provider order. Empty when no hands
    /// are present in the tracking volume.
    pub hands: SmallVec<[Hand; MAX_HANDS]>,
    /// Time the frame was captured by the provider (provider-relative clock).
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

use bitflags::bitflags;

bitflags! {
    /// Device-side health conditions reported by the underlying transport.
    /// Multiple flags can be set simultaneously (e.g., `STREAMING | SMUDGED`
    /// when the sensor is producing degraded frames). Mirrors leaprs'
    /// `DeviceStatus` bitflags 1:1, exposed in our own crate so the leaprs
    /// type doesn't leak across the provider trait boundary.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct DeviceHealth: u32 {
        /// Device is actively producing tracking frames.
        const STREAMING       = 1 << 0;
        /// Device streaming has been paused.
        const PAUSED          = 1 << 1;
        /// Known IR interference present; device has switched to robust mode.
        const ROBUST          = 1 << 2;
        /// Sensor window is smudged; tracking may be degraded.
        const SMUDGED         = 1 << 3;
        /// Device has entered low-resource mode.
        const LOW_RESOURCE    = 1 << 4;
        /// Unknown device failure.
        const UNKNOWN_FAILURE = 1 << 5;
        /// Device has a bad calibration record; cannot send frames.
        const BAD_CALIBRATION = 1 << 6;
        /// Corrupt firmware, or required firmware update cannot install.
        const BAD_FIRMWARE    = 1 << 7;
        /// USB transport is faulty.
        const BAD_TRANSPORT   = 1 << 8;
        /// USB control interface failed to initialize.
        const BAD_CONTROL     = 1 << 9;
    }
}

bitflags! {
    /// Service-side health conditions reported by the LeapC service.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct ServiceHealth: u32 {
        /// Service can't receive frames fast enough from the hardware.
        const LOW_FPS_DETECTED       = 1 << 0;
        /// Service paused itself due to insufficient hardware framerate.
        const POOR_PERFORMANCE_PAUSE = 1 << 1;
        /// Service failed to start tracking; reason unknown.
        const TRACKING_ERROR_UNKNOWN = 1 << 2;
    }
}

/// Reachability of the underlying transport (`LeapC` service for native;
/// `WebSocket` endpoint for web).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ServiceConnection {
    /// Provider has not started yet.
    #[default]
    NotStarted,
    /// Connection handshake is in progress. Maps to leaprs
    /// `ConnectionStatus::HandshakeIncomplete`.
    Connecting,
    /// Service reached. Maps to `ConnectionStatus::Connected`.
    Connected,
    /// The Ultraleap tracking service is not installed or not running
    /// on this machine. Maps to `ConnectionStatus::NotRunning`.
    ServiceMissing,
    /// Was connected, then dropped.
    Disconnected,
    /// Unrecoverable provider-level error. Error reason is held in
    /// `ProviderDiagnostics::last_error`.
    Errored,
}

/// Whether a tracking device is currently attached.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DevicePresence {
    /// No device attached to the service.
    #[default]
    NoDevice,
    /// A device is attached. Device serial + SDK version live in
    /// [`ProviderDiagnostics`].
    Attached,
    /// A previously-attached device was unplugged.
    Lost,
    /// Device reported a failure condition. Failure reason is held in
    /// [`ProviderDiagnostics::last_error`].
    Failed,
}

/// Whether tracking frames are currently flowing, plus heartbeat metrics.
#[derive(Debug, Clone, Copy, Default)]
pub enum TrackingFlow {
    /// No tracking frames are currently arriving.
    #[default]
    NotStreaming,
    /// Tracking frames are arriving.
    Streaming {
        /// Time elapsed since the most recent tracking frame.
        last_frame_ago: Duration,
        /// Cumulative count of dropped frames since `start()`.
        dropped_since_start: u64,
    },
}

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
