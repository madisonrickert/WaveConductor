//! Scripted-frame provider used by tests and as the default fallback provider.
//!
//! Construct with a list of [`HandTrackingFrame`]s; each `poll` emits the next
//! frame in the script. When the script is exhausted, `poll` emits nothing.

use std::time::Duration;

use bevy::prelude::*;

use crate::input::provider::HandTrackingProvider;
use crate::input::state::{
    DeviceHealth, DevicePresence, HandTrackingError, HandTrackingFrame, ProviderDiagnostics,
    ProviderStatus, ServiceConnection, ServiceHealth, TrackingFlow,
};

/// Scripted-frame provider. Each `poll` emits the next frame from `queue`.
///
/// Used by integration tests and as a fallback simulator source so the app
/// boots cleanly without hardware.
pub struct MockProvider {
    /// Frames waiting to be emitted, in order. `poll` removes from the front.
    queue: std::collections::VecDeque<HandTrackingFrame>,
    /// When set, `poll` emits a freshly-stamped copy of this frame on every
    /// call once the scripted `queue` is drained, so a synthetic hand persists
    /// indefinitely instead of vanishing after one frame. Set by
    /// [`MockProvider::synthetic_hand`]; `None` for the default empty mock.
    looping_frame: Option<HandTrackingFrame>,
    /// Whether `start()` has been called successfully.
    started: bool,
    /// Allow tests to inject specific device-health flags to exercise the
    /// dev panel + LED color logic.
    pub injected_health: DeviceHealth,
}

impl Default for MockProvider {
    fn default() -> Self {
        Self {
            queue: std::collections::VecDeque::new(),
            looping_frame: None,
            started: false,
            injected_health: DeviceHealth::empty(),
        }
    }
}

impl MockProvider {
    /// Construct a new mock provider that emits the given frames on subsequent
    /// `poll` calls.
    #[must_use]
    pub fn with_frames(frames: impl IntoIterator<Item = HandTrackingFrame>) -> Self {
        Self {
            queue: frames.into_iter().collect(),
            looping_frame: None,
            started: false,
            injected_health: DeviceHealth::empty(),
        }
    }

    /// A mock that continuously emits a single stationary synthetic open hand
    /// (see [`crate::input::synthetic::synthetic_open_hand`]) on every `poll`,
    /// for exercising hand-driven visuals with no Leap hardware attached.
    /// Selected at runtime via `WAVECONDUCTOR_HAND_PROVIDER=synthetic`.
    ///
    /// Distinct from [`MockProvider::default`], which emits nothing — the empty
    /// default is the silent auto-fallback used when no Leap is present, so it
    /// must *not* conjure a phantom hand in production.
    #[must_use]
    pub fn synthetic_hand() -> Self {
        Self {
            queue: std::collections::VecDeque::new(),
            looping_frame: Some(crate::input::synthetic::synthetic_hand_frame(Duration::ZERO)),
            started: false,
            injected_health: DeviceHealth::empty(),
        }
    }

    /// Append more frames to the script. Useful for tests that build the
    /// script incrementally.
    pub fn push_frame(&mut self, frame: HandTrackingFrame) {
        self.queue.push_back(frame);
    }

    /// How many frames remain in the script.
    #[must_use]
    pub fn remaining_frames(&self) -> usize {
        self.queue.len()
    }
}

impl HandTrackingProvider for MockProvider {
    fn start(&mut self) -> Result<(), HandTrackingError> {
        self.started = true;
        Ok(())
    }

    fn stop(&mut self) {
        self.started = false;
    }

    fn poll(&mut self, now: Duration, out: &mut Messages<HandTrackingFrame>) {
        if let Some(frame) = self.queue.pop_front() {
            out.write(frame);
        } else if let Some(template) = &self.looping_frame {
            // Re-stamp the persistent synthetic frame with the current clock so
            // downstream consumers see a steady, present hand each tick.
            let mut frame = template.clone();
            frame.timestamp = now;
            out.write(frame);
        }
    }

    fn status(&self) -> ProviderStatus {
        if !self.started {
            return ProviderStatus::default();
        }
        ProviderStatus {
            service: ServiceConnection::Connected,
            device: DevicePresence::Attached,
            health: DeviceHealth::STREAMING | self.injected_health,
            streaming: TrackingFlow::Streaming {
                last_frame_ago: Duration::from_millis(10),
                dropped_since_start: 0,
            },
            service_health: ServiceHealth::empty(),
        }
    }

    fn diagnostics(&self) -> ProviderDiagnostics {
        ProviderDiagnostics {
            device_serial: Some("MOCK00000000".to_string()),
            sdk_version: Some("MockProvider (scripted frames)".to_string()),
            active_policies: Vec::new(),
            dropped_frames: 0,
            last_error: None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;
    use crate::input::provider::ProviderId;
    use crate::input::state::PrimaryState;
    use smallvec::smallvec;

    fn empty_frame(at_ms: u64) -> HandTrackingFrame {
        HandTrackingFrame {
            provider: ProviderId::Mock,
            hands: smallvec![],
            timestamp: Duration::from_millis(at_ms),
        }
    }

    #[test]
    fn newly_constructed_provider_is_not_started() {
        let provider = MockProvider::with_frames([]);
        assert_eq!(provider.status().primary(), PrimaryState::NotStarted);
    }

    #[test]
    fn start_transitions_to_streaming() {
        let mut provider = MockProvider::with_frames([]);
        provider.start().expect("mock provider start cannot fail");
        assert_eq!(provider.status().primary(), PrimaryState::Streaming);
    }

    #[test]
    fn poll_emits_frames_in_order() {
        let mut provider = MockProvider::with_frames([empty_frame(10), empty_frame(20)]);
        provider.start().expect("mock provider start cannot fail");

        // Drive the provider through a Bevy `Messages` resource to exercise the
        // real consumer pipeline.
        let mut world = World::new();
        world.init_resource::<Messages<HandTrackingFrame>>();

        {
            let mut msgs = world.resource_mut::<Messages<HandTrackingFrame>>();
            provider.poll(Duration::ZERO, msgs.as_mut());
        }
        assert_eq!(provider.remaining_frames(), 1);

        {
            let mut msgs = world.resource_mut::<Messages<HandTrackingFrame>>();
            provider.poll(Duration::ZERO, msgs.as_mut());
        }
        assert_eq!(provider.remaining_frames(), 0);

        // A third poll on an exhausted script emits nothing and does not panic.
        {
            let mut msgs = world.resource_mut::<Messages<HandTrackingFrame>>();
            provider.poll(Duration::ZERO, msgs.as_mut());
        }
        assert_eq!(provider.remaining_frames(), 0);
    }

    #[test]
    fn push_frame_extends_the_script() {
        let mut provider = MockProvider::with_frames([]);
        assert_eq!(provider.remaining_frames(), 0);
        provider.push_frame(empty_frame(10));
        provider.push_frame(empty_frame(20));
        assert_eq!(provider.remaining_frames(), 2);
    }

    #[test]
    fn synthetic_hand_emits_a_hand_every_poll() {
        let mut provider = MockProvider::synthetic_hand();
        provider.start().expect("mock provider start cannot fail");

        // The synthetic hand is not a finite script — it persists across polls,
        // re-stamped with the current clock each time.
        for tick in 1..=3 {
            let now = Duration::from_millis(tick * 10);
            let mut msgs = Messages::<HandTrackingFrame>::default();
            provider.poll(now, &mut msgs);
            let frames: Vec<HandTrackingFrame> = msgs.drain().collect();
            assert_eq!(frames.len(), 1, "synthetic mock emits one frame per poll");
            assert_eq!(frames[0].hands.len(), 1, "exactly one synthetic hand");
            assert_eq!(frames[0].timestamp, now, "frame re-stamped with current clock");
        }
    }
}
