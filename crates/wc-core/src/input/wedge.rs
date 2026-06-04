//! Wedge detector for the Ultraleap tracking service (roadmap `leap-deep-idle-state`).
//!
//! Pure logic — no Bevy, no `leaprs`, always compiled — so it unit-tests without
//! hardware or the `hand-tracking-gestures` feature. The Bevy wiring that feeds it
//! and surfaces its verdict lives in [`crate::input::providers::leap_native`] and
//! [`crate::input::systems`].
//!
//! A "wedge" is the Ultraleap service alive-but-frozen: its control path still
//! responds (`set_pause` keeps ack'ing) but the frame stream is dead. The provider
//! heartbeat already degrades `Streaming → NotStreaming` after ~1 s of silence; this
//! is the next debounce stage that distinguishes a real wedge from a benign pause:
//! sustained not-streaming **while we expect streaming** for [`WEDGE_THRESHOLD`].

use std::time::Duration;

/// Sustained not-streaming-while-expecting, beyond the provider's ~1 s
/// `STALE_FRAME_THRESHOLD`, before we call it a wedge. The heartbeat already
/// spends ~1 s declaring `NotStreaming`, so this adds ~2 s of confirmed silence —
/// long enough to ride out a GPU-contention hitch or a duty-cycle gap, short
/// enough to surface promptly (total LED latency ≈ 1 s + `WEDGE_THRESHOLD` ≈ 4 s).
pub const WEDGE_THRESHOLD: Duration = Duration::from_secs(3);

/// Edge produced by [`LeapWedgeDetector::poll`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WedgeTransition {
    /// No edge this tick.
    None,
    /// Just entered a wedge (silence crossed [`WEDGE_THRESHOLD`]).
    Entered,
    /// Just recovered (frames resumed, or we stopped expecting them).
    Cleared,
}

/// Debounced wedge detector. Owned by the native provider; advanced once per
/// `poll()`. Pure: all state is the timer stamp + the latched verdict.
#[derive(Clone, Copy, Debug, Default)]
pub struct LeapWedgeDetector {
    /// Monotonic time the current not-streaming-while-expecting run began.
    not_streaming_since: Option<Duration>,
    /// Latched verdict: are we currently wedged?
    wedged: bool,
}

impl LeapWedgeDetector {
    /// Advance against the monotonic clock.
    ///
    /// - `expecting_streaming`: the service *should* be producing frames now
    ///   (connected, device attached, has streamed before, not intentionally paused).
    /// - `is_streaming`: frames are currently flowing.
    ///
    /// Returns the edge for this tick; [`Self::is_wedged`] exposes the latched state.
    pub fn poll(
        &mut self,
        now: Duration,
        expecting_streaming: bool,
        is_streaming: bool,
    ) -> WedgeTransition {
        // Silence isn't suspicious (paused / never-streamed / device gone) or
        // frames are flowing → clear the timer; report recovery if we were wedged.
        if !expecting_streaming || is_streaming {
            self.not_streaming_since = None;
            return if std::mem::take(&mut self.wedged) {
                WedgeTransition::Cleared
            } else {
                WedgeTransition::None
            };
        }
        // expecting_streaming && !is_streaming: arm on the first such tick, latch
        // once silence exceeds the threshold.
        let since = *self.not_streaming_since.get_or_insert(now);
        if !self.wedged && now.saturating_sub(since) >= WEDGE_THRESHOLD {
            self.wedged = true;
            WedgeTransition::Entered
        } else {
            WedgeTransition::None
        }
    }

    /// The latched verdict. Mirrored onto `ProviderStatus::wedged` by the provider.
    #[must_use]
    pub fn is_wedged(&self) -> bool {
        self.wedged
    }

    /// Reset to the default (not wedged, timer disarmed). Called on provider
    /// `start()`/`stop()` so a reconnect doesn't inherit stale state.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn threshold_exceeds_heartbeat() {
        // Self-contained floor: this always-compiled module can't see the
        // provider's private STALE_FRAME_THRESHOLD. The authoritative
        // WEDGE_THRESHOLD > STALE_FRAME_THRESHOLD binding is a compile-time
        // assertion in leap_native.rs.
        assert!(WEDGE_THRESHOLD >= Duration::from_secs(1));
    }

    #[test]
    fn benign_not_streaming_below_threshold() {
        let mut d = LeapWedgeDetector::default();
        assert_eq!(d.poll(Duration::ZERO, true, false), WedgeTransition::None);
        // WEDGE_THRESHOLD is 3 s; 2_999 ms is 1 ms below the threshold.
        assert_eq!(d.poll(at(2_999), true, false), WedgeTransition::None);
        assert!(!d.is_wedged());
    }

    #[test]
    fn intentional_pause_never_wedges() {
        let mut d = LeapWedgeDetector::default();
        for ms in [0, 5_000, 10_000] {
            assert_eq!(d.poll(at(ms), false, false), WedgeTransition::None);
        }
        assert!(!d.is_wedged());
    }

    #[test]
    fn real_wedge_after_threshold_is_edge_once() {
        let mut d = LeapWedgeDetector::default();
        d.poll(Duration::ZERO, true, false); // arm
        assert_eq!(
            d.poll(WEDGE_THRESHOLD, true, false),
            WedgeTransition::Entered
        );
        assert!(d.is_wedged());
        assert_eq!(
            d.poll(WEDGE_THRESHOLD + at(500), true, false),
            WedgeTransition::None
        );
        assert!(d.is_wedged());
    }

    #[test]
    fn recovery_clears() {
        let mut d = LeapWedgeDetector::default();
        d.poll(Duration::ZERO, true, false);
        d.poll(WEDGE_THRESHOLD, true, false); // Entered
        assert_eq!(
            d.poll(WEDGE_THRESHOLD + at(100), true, true),
            WedgeTransition::Cleared
        );
        assert!(!d.is_wedged());
    }

    #[test]
    fn pause_during_wedge_clears() {
        let mut d = LeapWedgeDetector::default();
        d.poll(Duration::ZERO, true, false);
        d.poll(WEDGE_THRESHOLD, true, false); // Entered
        assert_eq!(
            d.poll(WEDGE_THRESHOLD + at(100), false, false),
            WedgeTransition::Cleared
        );
        assert!(!d.is_wedged());
    }

    #[test]
    fn re_wedge_after_recovery() {
        let mut d = LeapWedgeDetector::default();
        d.poll(Duration::ZERO, true, false);
        d.poll(WEDGE_THRESHOLD, true, false); // Entered
        d.poll(WEDGE_THRESHOLD + at(100), true, true); // Cleared
        d.poll(WEDGE_THRESHOLD + at(200), true, false); // re-arm
        assert_eq!(
            d.poll(WEDGE_THRESHOLD + at(200) + WEDGE_THRESHOLD, true, false),
            WedgeTransition::Entered
        );
        assert!(d.is_wedged());
    }

    #[test]
    fn reset_disarms() {
        let mut d = LeapWedgeDetector::default();
        d.poll(Duration::ZERO, true, false);
        d.poll(WEDGE_THRESHOLD, true, false); // Entered
        d.reset();
        assert!(!d.is_wedged());
        assert_eq!(d.poll(Duration::ZERO, true, false), WedgeTransition::None);
    }
}
