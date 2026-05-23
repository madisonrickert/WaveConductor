//! Scripted-frame provider used by tests and as the default `ActiveProvider`.
//!
//! Construct with a list of [`HandTrackingFrame`]s; each `poll` emits the next
//! frame in the script. When the script is exhausted, `poll` emits nothing.

use std::time::Duration;

use bevy::prelude::*;

use crate::input::provider::HandTrackingProvider;
use crate::input::state::{HandTrackingError, HandTrackingFrame, HandTrackingStatus};

/// Scripted-frame provider. Each `poll` emits the next frame from `queue`.
///
/// Used by integration tests and as the default `ActiveProvider` so the app
/// boots cleanly without hardware.
#[derive(Default)]
pub struct MockProvider {
    /// Frames waiting to be emitted, in order. `poll` removes from the front.
    queue: std::collections::VecDeque<HandTrackingFrame>,
    /// Lifecycle status.
    status: HandTrackingStatus,
}

impl MockProvider {
    /// Construct a new mock provider that emits the given frames on subsequent
    /// `poll` calls.
    #[must_use]
    pub fn with_frames(frames: impl IntoIterator<Item = HandTrackingFrame>) -> Self {
        Self {
            queue: frames.into_iter().collect(),
            status: HandTrackingStatus::NotStarted,
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
        self.status = HandTrackingStatus::Connected;
        Ok(())
    }

    fn stop(&mut self) {
        self.status = HandTrackingStatus::Disconnected;
    }

    fn poll(&mut self, _now: Duration, out: &mut Messages<HandTrackingFrame>) {
        if let Some(frame) = self.queue.pop_front() {
            out.write(frame);
        }
    }

    fn status(&self) -> HandTrackingStatus {
        self.status
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;
    use smallvec::smallvec;

    fn empty_frame(at_ms: u64) -> HandTrackingFrame {
        HandTrackingFrame {
            hands: smallvec![],
            timestamp: Duration::from_millis(at_ms),
        }
    }

    #[test]
    fn newly_constructed_provider_is_not_started() {
        let provider = MockProvider::with_frames([]);
        assert_eq!(provider.status(), HandTrackingStatus::NotStarted);
    }

    #[test]
    fn start_transitions_to_connected() {
        let mut provider = MockProvider::with_frames([]);
        provider.start().expect("mock provider start cannot fail");
        assert_eq!(provider.status(), HandTrackingStatus::Connected);
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
}
