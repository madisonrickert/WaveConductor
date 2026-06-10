//! Hand-tracking resource + message types.
//!
//! Mirrors the shape of Bevy's `Res<Touches>` for continuous data and
//! `Messages<TouchInput>` for raw events.

use std::time::Duration;

use bevy::prelude::*;
use smallvec::SmallVec;
use thiserror::Error;

use super::hand::Hand;

/// Maximum number of hands a provider is expected to report. Both Leap Motion
/// and `MediaPipe` Hands track at most two hands; this matches the hardware
/// reality and keeps the `SmallVec` inline.
pub const MAX_HANDS: usize = 2;

/// Current snapshot of all active hands.
///
/// Updated each `PreUpdate` by [`crate::input::systems::mirror_state_resource`]
/// from the current [`TrackedHand`] entity query.
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
    /// Production write path is [`crate::input::systems::mirror_state_resource`]
    /// (derives state from the [`crate::input::entity::TrackedHand`] entity query).
    /// Promoted to `pub` in Plan 11 so integration tests can synthesize hand
    /// frames without a fake provider — see
    /// `crates/wc-sketches/tests/line_input.rs::hand_pinch_activates_mouse_attractor`.
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
/// [`crate::input::systems::poll_all_providers`]. Most systems consume the
/// derived [`HandTrackingState`] resource instead; raw frames are useful for
/// analytics, recording, and the lifecycle interaction-reset system.
#[derive(Message, Debug, Clone)]
pub struct HandTrackingFrame {
    /// Source provider for this frame. Stamped by `poll_all_providers`,
    /// not by the provider itself, so providers don't need to know their
    /// own ID.
    pub provider: super::provider::ProviderId,
    /// Hands tracked in this frame, in provider order. Empty when no hands
    /// are present in the tracking volume.
    pub hands: SmallVec<[Hand; MAX_HANDS]>,
    /// Time the frame was captured by the provider (provider-relative clock).
    pub timestamp: Duration,
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

/// Coarse-grained state for the status LED dot. Derived from the multi-axis
/// [`ProviderStatus`]; the dev panel reads the axes directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrimaryState {
    /// Provider has not started.
    #[default]
    NotStarted,
    /// Ultraleap service (or WS server, on web) not running.
    ServiceMissing,
    /// Connecting / handshake / dropped. Surface as one user-facing state.
    Disconnected,
    /// Service reachable, no Leap device attached.
    ServiceOnly,
    /// Device attached but not currently streaming frames.
    DeviceAttached,
    /// Device attached and expected to stream, but the frame stream is
    /// sustained-dead — the service is wedged (alive-but-frozen).
    DeviceWedged,
    /// Streaming and the device reports no degraded-health flags.
    Streaming,
    /// Streaming, but `health` contains a degradation flag (smudged, robust,
    /// low-resource) or `service_health` contains `LOW_FPS_DETECTED` /
    /// `POOR_PERFORMANCE_PAUSE`.
    DeviceDegraded,
    /// Device reported a failure (`BAD_TRANSPORT` / `BAD_FIRMWARE` /
    /// `BAD_CALIBRATION` / `BAD_CONTROL` / `UNKNOWN_FAILURE`) or
    /// [`DevicePresence::Failed`].
    DeviceFailed,
}

/// Multi-axis snapshot of a provider's lifecycle and health, updated each
/// `poll()`. The status LED reads [`Self::primary`]; the dev panel reads
/// every field.
#[derive(Debug, Clone, Default)]
pub struct ProviderStatus {
    /// Reachability of the underlying transport.
    pub service: ServiceConnection,
    /// Whether a tracking device is currently attached.
    pub device: DevicePresence,
    /// Device-side health conditions. Multiple flags possible simultaneously.
    pub health: DeviceHealth,
    /// Whether tracking frames are currently flowing.
    pub streaming: TrackingFlow,
    /// Service-side health conditions.
    pub service_health: ServiceHealth,
    /// `true` when the service is wedged: attached + expected-to-stream but the
    /// frame stream is sustained-dead. Set only by the native provider's
    /// `LeapWedgeDetector`; `primary()` maps it to `PrimaryState::DeviceWedged`.
    pub wedged: bool,
}

impl ProviderStatus {
    /// Coarse-grained derived state for UI status indicators.
    ///
    /// Precedence (first matching rule wins):
    /// 1. `service == NotStarted` → `NotStarted`
    /// 2. Device failure conditions → `DeviceFailed`
    /// 3. Service-level reachability problems → `ServiceMissing` / `Disconnected`
    /// 4. Streaming with any health/service-health degradation → `DeviceDegraded`
    /// 5. Streaming clean → [`PrimaryState::Streaming`]
    ///    5.5. Attached + wedged (sustained-dead stream) → [`PrimaryState::DeviceWedged`]
    /// 6. Device attached but no streaming → `DeviceAttached`
    /// 7. Service connected, no device → `ServiceOnly`
    /// 8. Anything else → `Disconnected` (catch-all)
    #[must_use]
    pub fn primary(&self) -> PrimaryState {
        // Rule 1
        if matches!(self.service, ServiceConnection::NotStarted) {
            return PrimaryState::NotStarted;
        }

        // Rule 2 — device failure or hard-failure health flags
        let hard_failure = DeviceHealth::UNKNOWN_FAILURE
            | DeviceHealth::BAD_CALIBRATION
            | DeviceHealth::BAD_FIRMWARE
            | DeviceHealth::BAD_TRANSPORT
            | DeviceHealth::BAD_CONTROL;
        if matches!(self.device, DevicePresence::Failed) || self.health.intersects(hard_failure) {
            return PrimaryState::DeviceFailed;
        }

        // Rule 3 — service-level reachability
        match self.service {
            ServiceConnection::ServiceMissing => return PrimaryState::ServiceMissing,
            ServiceConnection::Errored
            | ServiceConnection::Disconnected
            | ServiceConnection::Connecting => return PrimaryState::Disconnected,
            ServiceConnection::Connected | ServiceConnection::NotStarted => {}
        }

        // From here `service == Connected`.

        // Rules 4-5: streaming branch
        if matches!(self.streaming, TrackingFlow::Streaming { .. }) {
            let soft_degrade =
                DeviceHealth::SMUDGED | DeviceHealth::ROBUST | DeviceHealth::LOW_RESOURCE;
            let service_degrade =
                ServiceHealth::LOW_FPS_DETECTED | ServiceHealth::POOR_PERFORMANCE_PAUSE;
            if self.health.intersects(soft_degrade)
                || self.service_health.intersects(service_degrade)
            {
                return PrimaryState::DeviceDegraded;
            }
            return PrimaryState::Streaming;
        }

        // Rule 5.5 — attached + expected-to-stream but stream sustained-dead.
        // Below DeviceFailed/Disconnected (a real failure is more actionable),
        // above benign DeviceAttached (the whole point: distinguish a wedge).
        if self.wedged && matches!(self.device, DevicePresence::Attached) {
            return PrimaryState::DeviceWedged;
        }

        // Rule 6
        if matches!(self.device, DevicePresence::Attached) {
            return PrimaryState::DeviceAttached;
        }

        // Rule 7
        if matches!(self.device, DevicePresence::NoDevice) {
            return PrimaryState::ServiceOnly;
        }

        // Rule 8 — catch-all (e.g., DevicePresence::Lost with no streaming)
        PrimaryState::Disconnected
    }
}

/// Provider-level diagnostic metadata, separate from per-poll status.
///
/// Updated by the provider during `poll()` (or `start()` for static fields
/// like `sdk_version`). Surfaced through
/// `HandTrackingProvider::diagnostics()`. Read by the dev panel; not consumed
/// by the status LED.
#[derive(Debug, Clone, Default)]
pub struct ProviderDiagnostics {
    /// Device serial number (e.g., "LP00012345"). None on providers that
    /// don't expose it (mock; `WebSocket` before deviceEvent).
    pub device_serial: Option<String>,
    /// SDK / runtime version string. Example: "Ultraleap Gemini 6.2.0".
    pub sdk_version: Option<String>,
    /// Currently-active policy flags as human-readable strings (e.g.
    /// `"BackgroundFrames"`). Empty when no policies are set.
    pub active_policies: Vec<String>,
    /// Cumulative dropped-frames count since `start()`. Mirrors the value
    /// inside [`TrackingFlow::Streaming::dropped_since_start`] when streaming;
    /// kept here so the dev panel can render it across all states.
    pub dropped_frames: u64,
    /// Short reason string for the most recent
    /// [`ServiceConnection::Errored`] or [`DevicePresence::Failed`]. None when
    /// no error has occurred.
    pub last_error: Option<String>,
    /// Provider-specific numeric/text metrics for the dev panel. Labels are
    /// static so providers can update these without allocating strings in hot
    /// paths. The inline capacity (20) carries headroom over the densest
    /// current producer (`MediaPipe`, 17 metrics) so a few additions never
    /// silently spill the per-poll refill to the heap; that producer
    /// `debug_assert!`s `!metrics.spilled()` after its push block.
    pub metrics: SmallVec<[ProviderMetric; 20]>,
}

/// One provider-specific diagnostic metric.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProviderMetric {
    /// Human-readable label shown in the dev panel.
    pub label: &'static str,
    /// Metric value.
    pub value: ProviderMetricValue,
}

impl ProviderMetric {
    /// Construct a duration metric.
    #[must_use]
    pub const fn duration(label: &'static str, value: Duration) -> Self {
        Self {
            label,
            value: ProviderMetricValue::Duration(value),
        }
    }

    /// Construct a counter metric.
    #[must_use]
    pub const fn count(label: &'static str, value: u64) -> Self {
        Self {
            label,
            value: ProviderMetricValue::Count(value),
        }
    }

    /// Construct a static text metric.
    #[must_use]
    pub const fn text(label: &'static str, value: &'static str) -> Self {
        Self {
            label,
            value: ProviderMetricValue::Text(value),
        }
    }
}

/// Value for a provider-specific diagnostic metric.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProviderMetricValue {
    /// A duration.
    Duration(Duration),
    /// An unsigned counter.
    Count(u64),
    /// Static text.
    Text(&'static str),
}

/// One hand from a fused frame, tagged with the originating provider so
/// downstream systems can key their state by `(provider, raw_id)` rather
/// than relying on a global ID counter.
#[derive(Debug, Clone)]
pub struct FusedHand {
    /// Source provider for this hand.
    pub provider: super::provider::ProviderId,
    /// Provider-local hand identifier (stable across consecutive frames
    /// while the hand stays in the tracking volume).
    pub raw_id: u32,
    /// Mirrored from `Hand`.
    pub chirality: super::hand::Chirality,
    /// Palm centroid in Leap-device coordinates (millimeters).
    pub palm_position: Vec3,
    /// Palm velocity (mm/s).
    pub palm_velocity: Vec3,
    /// Pinch strength in `[0, 1]`.
    pub pinch_strength: f32,
    /// Grab strength in `[0, 1]`.
    pub grab_strength: f32,
    /// 21-landmark `MediaPipe` layout.
    pub landmarks: [Vec3; super::hand::LANDMARK_COUNT],
    /// 20 bone centers (5 fingers × 4 bones each) for `HandMesh` rendering.
    pub bone_centers: [Vec3; 20],
}

/// Fused frame emitted by `fuse_hand_frames` after combining all
/// provider-tagged [`HandTrackingFrame`]s for this tick.
#[derive(Message, Debug, Clone)]
pub struct FusedHandFrame {
    /// Hands present this tick, in deterministic order (left then right).
    pub hands: SmallVec<[FusedHand; MAX_HANDS]>,
    /// Time the frame was captured (provider-relative).
    pub timestamp: Duration,
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
    fn provider_status_primary_streaming_healthy() {
        let s = ProviderStatus {
            service: ServiceConnection::Connected,
            device: DevicePresence::Attached,
            health: DeviceHealth::STREAMING,
            streaming: TrackingFlow::Streaming {
                last_frame_ago: Duration::from_millis(10),
                dropped_since_start: 0,
            },
            service_health: ServiceHealth::empty(),
            wedged: false,
        };
        assert_eq!(s.primary(), PrimaryState::Streaming);
    }

    #[test]
    fn provider_status_primary_streaming_smudged_is_degraded() {
        let s = ProviderStatus {
            service: ServiceConnection::Connected,
            device: DevicePresence::Attached,
            health: DeviceHealth::STREAMING | DeviceHealth::SMUDGED,
            streaming: TrackingFlow::Streaming {
                last_frame_ago: Duration::from_millis(10),
                dropped_since_start: 0,
            },
            service_health: ServiceHealth::empty(),
            wedged: false,
        };
        assert_eq!(s.primary(), PrimaryState::DeviceDegraded);
    }

    #[test]
    fn provider_status_primary_service_missing() {
        let s = ProviderStatus {
            service: ServiceConnection::ServiceMissing,
            ..ProviderStatus::default()
        };
        assert_eq!(s.primary(), PrimaryState::ServiceMissing);
    }

    #[test]
    fn provider_status_primary_device_failed() {
        let s = ProviderStatus {
            service: ServiceConnection::Connected,
            device: DevicePresence::Failed,
            ..ProviderStatus::default()
        };
        assert_eq!(s.primary(), PrimaryState::DeviceFailed);
    }

    #[test]
    fn provider_status_primary_service_health_low_fps_is_degraded() {
        let s = ProviderStatus {
            service: ServiceConnection::Connected,
            device: DevicePresence::Attached,
            health: DeviceHealth::STREAMING,
            streaming: TrackingFlow::Streaming {
                last_frame_ago: Duration::from_millis(10),
                dropped_since_start: 0,
            },
            service_health: ServiceHealth::LOW_FPS_DETECTED,
            wedged: false,
        };
        assert_eq!(s.primary(), PrimaryState::DeviceDegraded);
    }

    #[test]
    fn provider_status_primary_not_started_default() {
        assert_eq!(
            ProviderStatus::default().primary(),
            PrimaryState::NotStarted
        );
    }

    #[test]
    fn provider_status_primary_wedged_attached_is_device_wedged() {
        let s = ProviderStatus {
            service: ServiceConnection::Connected,
            device: DevicePresence::Attached,
            streaming: TrackingFlow::NotStreaming,
            wedged: true,
            ..ProviderStatus::default()
        };
        assert_eq!(s.primary(), PrimaryState::DeviceWedged);
    }

    #[test]
    fn provider_status_primary_device_failed_outranks_wedged() {
        let s = ProviderStatus {
            service: ServiceConnection::Connected,
            device: DevicePresence::Failed,
            wedged: true,
            ..ProviderStatus::default()
        };
        assert_eq!(s.primary(), PrimaryState::DeviceFailed);
    }

    #[test]
    fn provider_status_primary_wedged_requires_attached() {
        // DevicePresence::Lost is NOT caught by Rule 2 (only Failed is) and
        // passes through Rule 3 (service == Connected). With no streaming it
        // skips Rules 4-5, then the Rule 5.5 guard rejects it (not Attached),
        // falling through to the Rule 8 catch-all → Disconnected.
        let s = ProviderStatus {
            service: ServiceConnection::Connected,
            device: DevicePresence::Lost, // not Attached → Rule 5.5 guard must reject
            streaming: TrackingFlow::NotStreaming,
            wedged: true,
            ..ProviderStatus::default()
        };
        assert_ne!(s.primary(), PrimaryState::DeviceWedged);
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
            provider: crate::input::provider::ProviderId::Mock,
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
            provider: crate::input::provider::ProviderId::Mock,
            hands: smallvec::smallvec![fake_hand(2, Chirality::Right)],
            timestamp: Duration::from_millis(800),
        };
        state.ingest(&frame2);
        assert_eq!(state.active_hand_count(), 1);
        assert!(state.left().is_none());
        assert!(state.right().is_some());
    }
}
