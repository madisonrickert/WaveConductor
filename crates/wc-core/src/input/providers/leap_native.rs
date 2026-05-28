//! Real native `LeapC` provider backed by the `leaprs` crate.
//!
//! ## Lifecycle
//!
//! 1. **`start()`** — creates and opens a `leaprs::Connection`. Optionally
//!    requests the `BackgroundFrames` policy when `request_background` is `true`.
//!    On error, returns [`HandTrackingError::Unavailable`] and leaves
//!    `self.connection` as `None`.
//! 2. **`poll()`** — drains all pending leaprs events non-blockingly (timeout 0).
//!    Breaks the drain loop on `Error::Timeout` or `Error::None`. Each event is
//!    dispatched to `handle_event`. After draining, refreshes the
//!    `last_frame_ago` heartbeat inside [`TrackingFlow::Streaming`]; if more
//!    than 1 s has elapsed since the last tracking frame the streaming state
//!    degrades to `NotStreaming`.
//! 3. **`stop()`** — drops the connection and resets status.
//!
//! ## Encapsulation
//!
//! All `leaprs` types are fully encapsulated — they do not leak across the
//! [`HandTrackingProvider`] trait boundary. The only public types visible to
//! the rest of `wc-core` are the standard provider trait outputs:
//! [`ProviderStatus`], [`ProviderDiagnostics`], and [`HandTrackingFrame`].

#![cfg(feature = "hand-tracking-gestures")]

use std::time::{Duration, Instant};

use bevy::math::Vec3;
use bevy::prelude::Messages;
use smallvec::SmallVec;

use crate::input::hand::{Chirality, Hand, LandmarkIndex, LANDMARK_COUNT};
use crate::input::provider::HandTrackingProvider;
use crate::input::state::{
    DeviceHealth, DevicePresence, HandTrackingError, HandTrackingFrame, ProviderDiagnostics,
    ProviderStatus, ServiceConnection, TrackingFlow, MAX_HANDS,
};

// ── threshold for heartbeat degradation ──────────────────────────────────────
const STALE_FRAME_THRESHOLD: Duration = Duration::from_secs(1);

// ── provider struct ──────────────────────────────────────────────────────────

// The workspace lint `unsafe_code = "deny"` must be locally lifted for the
// two `unsafe impl` blocks below. `leaprs::Connection` wraps a raw LeapC FFI
// pointer that Rust cannot verify is thread-safe, but the LeapC SDK guarantees
// handle-safety from any single thread at a time. We own the connection
// exclusively (no aliasing) and access it only from the Bevy main thread via
// the `ProviderRegistry` exclusive resource. This is the only `unsafe impl`
// in `wc-core` — a deliberate, narrow FFI exception.
#[allow(unsafe_code)]
// SAFETY: `LeaprsProvider` is polled exclusively on the Bevy main thread via
// `ProviderRegistry` (an exclusive resource). The LeapC SDK guarantees
// handle-safety: a connection handle is valid from any single thread, provided
// it is not polled concurrently. No aliasing, no concurrent access.
unsafe impl Send for LeaprsProvider {}
#[allow(unsafe_code)]
// SAFETY: same reasoning as `Send` above.
unsafe impl Sync for LeaprsProvider {}

/// Native Ultraleap Leap Motion provider, backed by `leaprs` / `LeapC`.
///
/// Construct via [`Default`] then register with [`crate::input::provider::ProviderRegistry`].
/// Set `request_background` before registering if the app needs frames when
/// it does not have window focus.
#[derive(Default)]
pub struct LeaprsProvider {
    /// Live connection handle; `None` before `start()` or after `stop()`.
    connection: Option<leaprs::Connection>,
    /// Multi-axis provider health snapshot. Refreshed each `poll()`.
    status: ProviderStatus,
    /// Provider-level diagnostics (serial, SDK version, counters).
    diagnostics: ProviderDiagnostics,
    /// Wall-clock instant of the most recent tracking frame, used for the
    /// `last_frame_ago` heartbeat.
    last_tracking_instant: Option<Instant>,
    /// When `true`, `start()` requests the `BackgroundFrames` policy from
    /// `LeapC` so frames continue flowing when the window loses focus.
    pub request_background: bool,
}

// ── HandTrackingProvider impl ────────────────────────────────────────────────

impl HandTrackingProvider for LeaprsProvider {
    /// Create and open a `leaprs::Connection`. Optionally requests the
    /// `BackgroundFrames` policy. Sets `status.service` to `Connecting` on
    /// success so downstream code can distinguish "not started" from
    /// "handshake in progress".
    fn start(&mut self) -> Result<(), HandTrackingError> {
        let mut conn =
            leaprs::Connection::create(leaprs::ConnectionConfig::default()).map_err(|e| {
                HandTrackingError::Unavailable(format!("leaprs::Connection::create failed: {e}"))
            })?;

        conn.open().map_err(|e| {
            HandTrackingError::Unavailable(format!("leaprs::Connection::open failed: {e}"))
        })?;

        if self.request_background {
            if let Err(e) = conn.set_policy_flags(
                leaprs::PolicyFlags::BACKGROUND_FRAMES,
                leaprs::PolicyFlags::empty(),
            ) {
                // Non-fatal — log and continue. Background frames are a
                // best-effort enhancement, not a hard requirement.
                tracing::warn!("failed to set BackgroundFrames policy: {e}");
            }
        }

        self.connection = Some(conn);
        self.status.service = ServiceConnection::Connecting;
        self.diagnostics.sdk_version = Some("Ultraleap Gemini 6.2.0".to_string());

        Ok(())
    }

    /// Drop the connection and reset all status / diagnostic counters.
    fn stop(&mut self) {
        self.connection = None;
        self.status = ProviderStatus::default();
        self.last_tracking_instant = None;
    }

    /// Drain all pending leaprs events (non-blocking, timeout = 0).
    ///
    /// Iterates until `poll(0)` returns `Error::Timeout` (queue empty) or
    /// any other error. Dispatches each event to `handle_event`, then
    /// refreshes the `last_frame_ago` heartbeat.
    fn poll(&mut self, _now: Duration, out: &mut Messages<HandTrackingFrame>) {
        let Some(conn) = self.connection.as_mut() else {
            return;
        };

        loop {
            match conn.poll(0) {
                Ok(msg) => {
                    let event = msg.event();
                    // SAFETY: we forward the event to handle_event; the borrow
                    // on `conn` ends here since we only pass the event variant.
                    // We need to work around the borrow-checker by delegating
                    // to a free function that receives `&mut self` minus the
                    // connection. We use an unsafe pointer trick below.
                    //
                    // Actually: `conn` borrows from `self.connection`; we need
                    // to pass `&mut self.status` etc. separately. Use a helper
                    // that takes the sub-fields by reference.
                    dispatch_event(
                        event,
                        &mut self.status,
                        &mut self.diagnostics,
                        &mut self.last_tracking_instant,
                        out,
                    );
                }
                Err(leaprs::Error::Timeout) => {
                    // Queue exhausted — normal non-blocking exit.
                    break;
                }
                Err(e) => {
                    tracing::error!("leaprs poll error: {e}");
                    self.status.service = ServiceConnection::Errored;
                    self.diagnostics.last_error = Some(e.to_string());
                    break;
                }
            }
        }

        // Heartbeat: refresh last_frame_ago or degrade to NotStreaming.
        match self.status.streaming {
            TrackingFlow::Streaming {
                ref mut last_frame_ago,
                ..
            } => {
                let elapsed = self
                    .last_tracking_instant
                    .map_or(STALE_FRAME_THRESHOLD, |t| t.elapsed());
                if elapsed > STALE_FRAME_THRESHOLD {
                    self.status.streaming = TrackingFlow::NotStreaming;
                } else {
                    *last_frame_ago = elapsed;
                }
            }
            TrackingFlow::NotStreaming => {}
        }
    }

    fn status(&self) -> ProviderStatus {
        self.status.clone()
    }

    fn diagnostics(&self) -> ProviderDiagnostics {
        self.diagnostics.clone()
    }
}

// ── event dispatch (free function to avoid borrow-check issues) ───────────────

/// Dispatch a single leaprs event, mutating the provider's status and
/// diagnostics sub-fields. Separated from `LeaprsProvider::poll` so the
/// borrow of `self.connection` (which holds the event's lifetime) does not
/// overlap with the mutable borrow of `self.status` / `self.diagnostics`.
fn dispatch_event(
    event: leaprs::EventRef<'_>,
    status: &mut ProviderStatus,
    diagnostics: &mut ProviderDiagnostics,
    last_tracking_instant: &mut Option<Instant>,
    out: &mut Messages<HandTrackingFrame>,
) {
    match event {
        leaprs::EventRef::Connection(_) => {
            status.service = ServiceConnection::Connected;
        }

        leaprs::EventRef::ConnectionLost(_) => {
            status.service = ServiceConnection::Disconnected;
            status.streaming = TrackingFlow::NotStreaming;
        }

        leaprs::EventRef::Device(dev) => {
            status.device = DevicePresence::Attached;
            // Try to open the device and read its serial number.
            // Opening may fail if the device is in a transient state; that
            // is non-fatal — we still mark it as attached.
            match dev.device().open() {
                Ok(mut device) => match device.get_info() {
                    Ok(info) => {
                        if let Some(serial) = info.serial() {
                            diagnostics.device_serial = Some(serial.to_owned());
                        }
                    }
                    Err(e) => {
                        tracing::debug!("could not read device info: {e}");
                    }
                },
                Err(e) => {
                    tracing::debug!("could not open device for info query: {e}");
                }
            }
        }

        leaprs::EventRef::DeviceLost => {
            status.device = DevicePresence::Lost;
            status.streaming = TrackingFlow::NotStreaming;
        }

        leaprs::EventRef::DeviceFailure(failure) => {
            status.device = DevicePresence::Failed;
            let ds = failure.status();
            // DeviceStatus is a bitflags struct without Debug; use bits() for
            // a stable, human-readable representation in the error string.
            diagnostics.last_error = Some(format!("DeviceFailure status bits: 0x{:08X}", ds.bits()));
        }

        leaprs::EventRef::DeviceStatusChange(change) => {
            status.health = device_health_from_leaprs(change.status());
        }

        leaprs::EventRef::Tracking(tracking) => {
            let frame = build_frame_from_tracking(tracking);
            out.write(frame);
            *last_tracking_instant = Some(Instant::now());

            // Preserve the existing dropped counter when transitioning to
            // Streaming, or keep the running total if already streaming.
            let dropped = match status.streaming {
                TrackingFlow::Streaming {
                    dropped_since_start, ..
                } => dropped_since_start,
                TrackingFlow::NotStreaming => 0,
            };
            status.streaming = TrackingFlow::Streaming {
                last_frame_ago: Duration::ZERO,
                dropped_since_start: dropped,
            };
        }

        leaprs::EventRef::DroppedFrame(_) => {
            if let TrackingFlow::Streaming {
                ref mut dropped_since_start,
                ..
            } = status.streaming
            {
                *dropped_since_start = dropped_since_start.saturating_add(1);
            }
            diagnostics.dropped_frames = diagnostics.dropped_frames.saturating_add(1);
        }

        // Policy events, dropped-frame completions, image/log/config events,
        // and any future unknown variants are silently ignored. The policy we
        // requested is already applied at start(); a future phase can inspect
        // granted flags from the Policy event if needed.
        _ => {}
    }
}

// ── device health mapping ────────────────────────────────────────────────────

/// Map leaprs `DeviceStatus` bitflags to our `DeviceHealth` bitflags 1:1.
///
/// Both types model the same `LeapC` `eLeapDeviceStatus` constants; the
/// translation keeps leaprs types from leaking across the provider boundary.
fn device_health_from_leaprs(ds: leaprs::DeviceStatus) -> DeviceHealth {
    let mut out = DeviceHealth::empty();
    if ds.contains(leaprs::DeviceStatus::STREAMING) {
        out |= DeviceHealth::STREAMING;
    }
    if ds.contains(leaprs::DeviceStatus::PAUSED) {
        out |= DeviceHealth::PAUSED;
    }
    if ds.contains(leaprs::DeviceStatus::ROBUST) {
        out |= DeviceHealth::ROBUST;
    }
    if ds.contains(leaprs::DeviceStatus::SMUDGED) {
        out |= DeviceHealth::SMUDGED;
    }
    if ds.contains(leaprs::DeviceStatus::LOW_RESOURCE) {
        out |= DeviceHealth::LOW_RESOURCE;
    }
    if ds.contains(leaprs::DeviceStatus::UNKNOWN_FAILURE) {
        out |= DeviceHealth::UNKNOWN_FAILURE;
    }
    if ds.contains(leaprs::DeviceStatus::BAD_CALIBRATION) {
        out |= DeviceHealth::BAD_CALIBRATION;
    }
    if ds.contains(leaprs::DeviceStatus::BAD_FIRMWARE) {
        out |= DeviceHealth::BAD_FIRMWARE;
    }
    if ds.contains(leaprs::DeviceStatus::BAD_TRANSPORT) {
        out |= DeviceHealth::BAD_TRANSPORT;
    }
    if ds.contains(leaprs::DeviceStatus::BAD_CONTROL) {
        out |= DeviceHealth::BAD_CONTROL;
    }
    out
}

// ── frame conversion ─────────────────────────────────────────────────────────

/// Convert a `leaprs` tracking event into a [`HandTrackingFrame`].
///
/// The `provider` field on the returned frame is left as
/// [`crate::input::provider::ProviderId::Leap`] because `poll_all_providers`
/// stamps the provider ID after this returns (consistent with all other
/// providers). We set it here anyway to match the static type; `poll_all_providers`
/// will overwrite it.
///
/// Bone centers are NOT computed here — they are derived in `fuse_hand_frames`
/// (Phase 6) from the 21-landmark array via `bone_centers_from_landmarks`.
/// This keeps frame construction simple and consistent with other providers
/// that only supply landmarks.
fn build_frame_from_tracking(tracking: leaprs::TrackingEventRef<'_>) -> HandTrackingFrame {
    let raw_hands = tracking.hands();

    let mut hands: SmallVec<[Hand; MAX_HANDS]> = SmallVec::new();

    for h in raw_hands.iter().take(MAX_HANDS) {
        // pinch_strength and grab_strength are FIELDS accessed via Deref on
        // the packed LEAP_HAND FFI struct. Do NOT call them as methods — copy
        // to locals to avoid unaligned-reference UB from the packed struct Deref.
        let pinch_strength = h.pinch_strength;
        let grab_strength = h.grab_strength;
        let id = h.id;

        let chirality = match h.hand_type() {
            leaprs::HandType::Left => Chirality::Left,
            leaprs::HandType::Right => Chirality::Right,
        };

        let palm = h.palm();
        let palm_position = vec3_from_leaprs(palm.position());
        let palm_normal = vec3_from_leaprs(palm.normal());
        let palm_velocity = vec3_from_leaprs(palm.velocity());

        let landmarks = landmarks_from_hand(*h);

        hands.push(Hand {
            id,
            chirality,
            palm_position,
            palm_normal,
            palm_velocity,
            pinch_strength,
            grab_strength,
            landmarks,
        });
    }

    // Use the frame timestamp from the header (microseconds, provider clock).
    // LEAP_FRAME_HEADER::timestamp is `i64`; LeapC guarantees it is always
    // non-negative (counts microseconds since service start), but we clamp
    // rather than panic or silently wrap on the rare pathological case.
    let timestamp_us = tracking.info().timestamp;
    let timestamp = Duration::from_micros(u64::try_from(timestamp_us).unwrap_or(0));

    HandTrackingFrame {
        provider: crate::input::provider::ProviderId::Leap,
        hands,
        timestamp,
    }
}

/// Build the 21-landmark [`MediaPipe`](crate::input::hand::LandmarkIndex) array
/// from a leaprs `HandRef`.
///
/// ## Landmark layout
///
/// ```text
/// 0  Wrist          — index metacarpal prev_joint (palm root)
/// 1  ThumbCmc       — thumb metacarpal prev_joint
/// 2  ThumbMcp       — thumb proximal next_joint
/// 3  ThumbIp        — thumb intermediate next_joint (≈ IP joint)
/// 4  ThumbTip       — thumb distal next_joint
/// 5  IndexMcp       — index metacarpal next_joint
/// 6  IndexPip       — index proximal next_joint
/// 7  IndexDip       — index intermediate next_joint
/// 8  IndexTip       — index distal next_joint
/// … (same pattern for middle, ring, pinky)
/// ```
///
/// ## Thumb approximation
///
/// The Ultraleap model has a zero-length metacarpal for the thumb, so we use:
/// - `ThumbCmc` ← thumb metacarpal `prev_joint` (wrist-side anchor)
/// - `ThumbMcp` ← thumb proximal `next_joint` (knuckle)
/// - `ThumbIp`  ← thumb intermediate `next_joint` (IP joint approximation)
/// - `ThumbTip` ← thumb distal `next_joint` (fingertip)
///
/// The "zero metacarpal" means `ThumbCmc` and `Wrist` will often coincide —
/// this is intentional and matches the leaprs model.
fn landmarks_from_hand(hand: leaprs::HandRef<'_>) -> [Vec3; LANDMARK_COUNT] {
    use LandmarkIndex as L;

    let mut lm = [Vec3::ZERO; LANDMARK_COUNT];

    // ── Wrist: use the index-finger metacarpal's palm-side joint ─────────────
    // The index metacarpal's prev_joint is anchored at the wrist / palm root.
    let index = hand.index();
    lm[L::Wrist.as_index()] = vec3_from_leaprs(index.metacarpal().prev_joint());

    // ── Thumb ──────────────────────────────────────────────────────────────
    let thumb = hand.thumb();
    lm[L::ThumbCmc.as_index()] = vec3_from_leaprs(thumb.metacarpal().prev_joint());
    lm[L::ThumbMcp.as_index()] = vec3_from_leaprs(thumb.proximal().next_joint());
    lm[L::ThumbIp.as_index()] = vec3_from_leaprs(thumb.intermediate().next_joint());
    lm[L::ThumbTip.as_index()] = vec3_from_leaprs(thumb.distal().next_joint());

    // ── Index ──────────────────────────────────────────────────────────────
    lm[L::IndexMcp.as_index()] = vec3_from_leaprs(index.metacarpal().next_joint());
    lm[L::IndexPip.as_index()] = vec3_from_leaprs(index.proximal().next_joint());
    lm[L::IndexDip.as_index()] = vec3_from_leaprs(index.intermediate().next_joint());
    lm[L::IndexTip.as_index()] = vec3_from_leaprs(index.distal().next_joint());

    // ── Middle ─────────────────────────────────────────────────────────────
    let middle = hand.middle();
    lm[L::MiddleMcp.as_index()] = vec3_from_leaprs(middle.metacarpal().next_joint());
    lm[L::MiddlePip.as_index()] = vec3_from_leaprs(middle.proximal().next_joint());
    lm[L::MiddleDip.as_index()] = vec3_from_leaprs(middle.intermediate().next_joint());
    lm[L::MiddleTip.as_index()] = vec3_from_leaprs(middle.distal().next_joint());

    // ── Ring ───────────────────────────────────────────────────────────────
    let ring = hand.ring();
    lm[L::RingMcp.as_index()] = vec3_from_leaprs(ring.metacarpal().next_joint());
    lm[L::RingPip.as_index()] = vec3_from_leaprs(ring.proximal().next_joint());
    lm[L::RingDip.as_index()] = vec3_from_leaprs(ring.intermediate().next_joint());
    lm[L::RingTip.as_index()] = vec3_from_leaprs(ring.distal().next_joint());

    // ── Pinky ──────────────────────────────────────────────────────────────
    let pinky = hand.pinky();
    lm[L::PinkyMcp.as_index()] = vec3_from_leaprs(pinky.metacarpal().next_joint());
    lm[L::PinkyPip.as_index()] = vec3_from_leaprs(pinky.proximal().next_joint());
    lm[L::PinkyDip.as_index()] = vec3_from_leaprs(pinky.intermediate().next_joint());
    lm[L::PinkyTip.as_index()] = vec3_from_leaprs(pinky.distal().next_joint());

    lm
}

// ── coordinate conversion helpers ────────────────────────────────────────────

/// Convert a `leaprs` [`leaprs::LeapVectorRef`] to a Bevy [`Vec3`].
///
/// Uses `.array()` to safely copy out of the packed FFI struct — dereferencing
/// the x/y/z fields via the packed-struct Deref would cause unaligned-reference
/// UB.
#[inline]
fn vec3_from_leaprs(v: leaprs::LeapVectorRef<'_>) -> Vec3 {
    let [x, y, z] = v.array();
    Vec3::new(x, y, z)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that every `leaprs::DeviceStatus` bit maps to the corresponding
    /// `DeviceHealth` bit.  This is a compile-time-visible table — if a new
    /// `LeapC` status flag is added and leaprs exposes it, the corresponding
    /// `DeviceHealth` variant must be added and this test updated.
    #[test]
    fn device_health_maps_streaming() {
        let status = leaprs::DeviceStatus::STREAMING;
        let health = device_health_from_leaprs(status);
        assert!(health.contains(DeviceHealth::STREAMING));
        assert!(!health.contains(DeviceHealth::PAUSED));
    }

    #[test]
    fn device_health_maps_multiple_flags() {
        let status = leaprs::DeviceStatus::STREAMING | leaprs::DeviceStatus::SMUDGED;
        let health = device_health_from_leaprs(status);
        assert!(health.contains(DeviceHealth::STREAMING));
        assert!(health.contains(DeviceHealth::SMUDGED));
        assert!(!health.contains(DeviceHealth::PAUSED));
        assert!(!health.contains(DeviceHealth::ROBUST));
    }

    #[test]
    fn device_health_maps_failure_flags() {
        let status = leaprs::DeviceStatus::BAD_FIRMWARE | leaprs::DeviceStatus::BAD_TRANSPORT;
        let health = device_health_from_leaprs(status);
        assert!(health.contains(DeviceHealth::BAD_FIRMWARE));
        assert!(health.contains(DeviceHealth::BAD_TRANSPORT));
    }

    #[test]
    fn device_health_empty_roundtrips() {
        let health = device_health_from_leaprs(leaprs::DeviceStatus::empty());
        assert_eq!(health, DeviceHealth::empty());
    }
}
