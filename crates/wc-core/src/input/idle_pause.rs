//! Duty-cycle state machine for the deep-idle Leap throttle (roadmap
//! `leap-idle-pause`, CF #84).
//!
//! Pure logic — no Bevy systems, no `leaprs`, always compiled — so it unit-tests
//! without hardware or the `hand-tracking-gestures` feature. The Bevy wiring that
//! applies its decisions to the live provider lives in
//! [`crate::input::providers::leap_native`].
//!
//! ## Why a duty cycle, not a flat pause
//!
//! Flat-pausing the Leap service during the screensaver sheds the most CPU but
//! emits no frames, so a hand can never wake the install — fatal on the touchless
//! projector kiosk. Instead we keep the service paused for a gap `D`, then
//! un-pause for a short sample window `W` to look for a hand; period `P = W + D`.
//! Worst-case wake latency ≈ `P`; Leap-service CPU saved ≈ `1 − W/P`. See the
//! plan doc for the latency↔saving trade and how `W` is tuned to the measured
//! resume latency.

use std::time::Duration;

use bevy::prelude::Resource;

/// Worst-case wake latency / required hand-hold time. Madison-directed (0.5 s).
pub const IDLE_PAUSE_PERIOD: Duration = Duration::from_millis(500);

/// Un-paused sample window. Tuned against the Phase 1 hardware spike (2026-06-03):
/// measured Leap service resume latency `L` was median 32 ms / max 79 ms over 20
/// cycles. With [`SAMPLE_POLL_INTERVAL`] polling, worst-case wake-registration is
/// ~`max L` + one poll tick + the `reset_on_interaction` pass ≈ 95 ms, so 150 ms
/// leaves a comfortable ~55 ms margin while still parking the service ~70% of each
/// period. Must stay `< IDLE_PAUSE_PERIOD`.
pub const IDLE_PAUSE_SAMPLE_WINDOW: Duration = Duration::from_millis(150);

/// Wake interval requested while sampling, so the app ticks fast enough to catch
/// the resume frame (the present-rate throttle floors its reactive `wait` to this).
pub const SAMPLE_POLL_INTERVAL: Duration = Duration::from_millis(16);

/// Which half of the duty cycle we are in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DutyPhase {
    /// Service paused, shedding CPU; waiting out the gap `D`.
    Paused,
    /// Service un-paused, sampling for a hand for the window `W`.
    Sampling,
}

/// What the caller should do to the Leap service this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseAction {
    /// No change since last tick.
    Hold,
    /// Pause the service now (entered the gap).
    Pause,
    /// Un-pause the service now (entered a sample window).
    Resume,
}

/// Duty-cycle clock for the deep-idle Leap throttle. Advanced once per tick while
/// the screensaver is showing; reset on screensaver entry.
#[derive(Resource, Debug, Clone, Copy)]
pub struct LeapIdlePause {
    period: Duration,
    sample_window: Duration,
    phase: DutyPhase,
    /// Monotonic time (`Time::elapsed`) when the current phase began.
    phase_since: Duration,
}

impl Default for LeapIdlePause {
    fn default() -> Self {
        Self {
            period: IDLE_PAUSE_PERIOD,
            sample_window: IDLE_PAUSE_SAMPLE_WINDOW,
            phase: DutyPhase::Paused,
            phase_since: Duration::ZERO,
        }
    }
}

impl LeapIdlePause {
    /// The current duty phase.
    #[must_use]
    pub fn phase(&self) -> DutyPhase {
        self.phase
    }

    /// Paused gap `D = P − W`.
    #[must_use]
    fn gap(&self) -> Duration {
        self.period.saturating_sub(self.sample_window)
    }

    /// Begin the cycle paused (no visitor at deep-idle entry). Returns the action
    /// to apply now — always [`PauseAction::Pause`].
    pub fn reset_paused(&mut self, now: Duration) -> PauseAction {
        self.phase = DutyPhase::Paused;
        self.phase_since = now;
        PauseAction::Pause
    }

    /// Advance against the monotonic clock and return the pause action to apply.
    ///
    /// On a phase transition the phase clock resets to `now` (not the exact
    /// deadline), so the period drifts by at most one tick's latency per cycle —
    /// acceptable for a thermal duty cycle.
    pub fn advance(&mut self, now: Duration) -> PauseAction {
        let elapsed = now.saturating_sub(self.phase_since);
        match self.phase {
            DutyPhase::Paused => {
                if elapsed >= self.gap() {
                    self.phase = DutyPhase::Sampling;
                    self.phase_since = now;
                    PauseAction::Resume
                } else {
                    PauseAction::Hold
                }
            }
            DutyPhase::Sampling => {
                if elapsed >= self.sample_window {
                    self.phase = DutyPhase::Paused;
                    self.phase_since = now;
                    PauseAction::Pause
                } else {
                    PauseAction::Hold
                }
            }
        }
    }

    /// How soon the app should next wake to service the cycle. The screensaver
    /// present-rate throttle floors its reactive `wait` to this value so the gap
    /// ends on time and sample windows are polled fast enough to catch frames.
    #[must_use]
    pub fn requested_wake(&self, now: Duration) -> Duration {
        match self.phase {
            DutyPhase::Sampling => SAMPLE_POLL_INTERVAL,
            DutyPhase::Paused => {
                let elapsed = now.saturating_sub(self.phase_since);
                self.gap()
                    .saturating_sub(elapsed)
                    .max(Duration::from_millis(1))
            }
        }
    }

    /// Test seam: force the Sampling phase so a wiring test can prove
    /// `reset_paused`/`enter_leap_idle_pause` returns it to Paused.
    #[cfg(test)]
    // The wiring test (Task 5) uses this; clippy sees it as dead until that
    // test module is added.
    #[allow(dead_code)]
    pub(crate) fn set_phase_sampling(&mut self) {
        self.phase = DutyPhase::Sampling;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn sample_window_shorter_than_period() {
        assert!(IDLE_PAUSE_SAMPLE_WINDOW < IDLE_PAUSE_PERIOD);
    }

    #[test]
    fn reset_paused_pauses_immediately() {
        let mut d = LeapIdlePause::default();
        assert_eq!(d.reset_paused(at(1000)), PauseAction::Pause);
        assert_eq!(d.phase(), DutyPhase::Paused);
    }

    #[test]
    fn holds_during_gap_then_resumes() {
        let mut d = LeapIdlePause::default(); // gap = 350 ms
        d.reset_paused(Duration::ZERO);
        assert_eq!(d.advance(at(100)), PauseAction::Hold);
        assert_eq!(d.advance(at(349)), PauseAction::Hold);
        assert_eq!(d.advance(at(350)), PauseAction::Resume);
        assert_eq!(d.phase(), DutyPhase::Sampling);
    }

    #[test]
    fn holds_during_window_then_pauses() {
        let mut d = LeapIdlePause::default(); // window = 150 ms
        d.reset_paused(Duration::ZERO);
        d.advance(at(350)); // -> Sampling, phase_since = 350
        assert_eq!(d.advance(at(400)), PauseAction::Hold);
        assert_eq!(d.advance(at(499)), PauseAction::Hold);
        assert_eq!(d.advance(at(500)), PauseAction::Pause);
        assert_eq!(d.phase(), DutyPhase::Paused);
    }

    #[test]
    fn requested_wake_short_while_sampling() {
        let mut d = LeapIdlePause::default();
        d.reset_paused(Duration::ZERO);
        d.advance(at(350)); // -> Sampling
        assert_eq!(d.requested_wake(at(360)), SAMPLE_POLL_INTERVAL);
    }

    #[test]
    fn requested_wake_counts_down_during_gap() {
        let mut d = LeapIdlePause::default();
        d.reset_paused(Duration::ZERO); // Paused, gap 350
        assert_eq!(d.requested_wake(at(0)), at(350));
        assert_eq!(d.requested_wake(at(100)), at(250));
    }

    #[test]
    fn full_cycle_repeats() {
        let mut d = LeapIdlePause::default();
        d.reset_paused(Duration::ZERO);
        assert_eq!(d.advance(at(350)), PauseAction::Resume);
        assert_eq!(d.advance(at(500)), PauseAction::Pause);
        assert_eq!(d.advance(at(850)), PauseAction::Resume);
        assert_eq!(d.advance(at(1000)), PauseAction::Pause);
    }
}
