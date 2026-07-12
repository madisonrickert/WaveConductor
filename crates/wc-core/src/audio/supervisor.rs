//! Reconnect supervision for the audio engine.
//!
//! ## Why this exists
//!
//! A single output-endpoint blip — an HDMI TV going to sleep, an input switch,
//! a device re-enumeration — used to leave the app in a terminal
//! [`crate::audio::state::AudioStatus::Errored`] with a "restart the app" log.
//! This module replaces that dead end with a bounded exponential-backoff
//! rebuild loop.
//!
//! ## What runs where
//!
//! **This file is pure.** Every function is arithmetic over a monotonic clock
//! passed in as `f64` seconds; there is no cpal call, no thread, and no Bevy
//! `Time` here, which is what makes the whole state machine unit-testable with
//! no audio device (CI has none). Task 5's `supervise_audio` Bevy system is the
//! only place the wall clock is read and the actual stream rebuild is performed
//! — and it lives on the **main thread**, never the audio callback and never
//! the render thread. The rebuild itself (which can block on WASAPI
//! enumeration) is event-driven (error path / backoff tick), not a per-frame
//! cost.

use std::time::Duration;

use bevy::prelude::Resource;

/// Ceiling on the reconnect backoff. The delay doubles from 1 s and is clamped
/// here so an install left with a permanently-absent device retries at a steady
/// once-every-30-s rather than drifting toward hours.
pub const BACKOFF_CAP: Duration = Duration::from_secs(30);

/// Backoff before the `attempt`-th reconnect: `2^attempt` seconds (1, 2, 4, 8,
/// 16, …) clamped to [`BACKOFF_CAP`]. Attempt 0 is the first retry after a
/// failure, at 1 s.
///
/// The shift saturates: an implausibly large `attempt` yields the cap rather
/// than overflowing.
#[must_use]
pub fn backoff_delay(attempt: u32) -> Duration {
    // `checked_shl` returns None past 63 bits; treat that as "very large".
    let secs = 1_u64.checked_shl(attempt).unwrap_or(u64::MAX);
    Duration::from_secs(secs).min(BACKOFF_CAP)
}

/// What [`AudioSupervisor::poll`] tells the caller to do this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorAction {
    /// No attempt is due; do nothing this tick.
    Idle,
    /// A (re)build attempt is due now.
    Rebuild,
}

/// Reconnect bookkeeping: the consecutive-failure counter and the scheduled
/// time of the next attempt, both driven by the caller's monotonic clock.
///
/// A Bevy `Resource` so the main-thread supervisor system owns exactly one.
/// Times are `f64` seconds on whatever monotonic clock the caller uses
/// (Task 5 uses `Time<Real>::elapsed_secs_f64`); this type never reads a clock
/// itself.
#[derive(Resource, Debug, Default, Clone)]
pub struct AudioSupervisor {
    /// Consecutive failed (re)build attempts. Grows the backoff; reset on
    /// success.
    attempts: u32,
    /// Monotonic time of the next scheduled attempt, or `None` when no
    /// reconnect cycle is in progress.
    next_attempt_at: Option<f64>,
}

impl AudioSupervisor {
    /// Start (or restart) a reconnect cycle: reset the failure count and
    /// schedule the first attempt one backoff step out (`now + 1 s`).
    ///
    /// Resetting `attempts` here is what keeps a *later* outage in a long soak
    /// from inheriting the previous outage's 30 s backoff: every fresh stream
    /// death starts the schedule over at 1 s.
    ///
    /// Idempotent while a cycle is already running is *not* assumed — call this
    /// only on the edge into `Reconnecting`.
    pub fn begin(&mut self, now: f64) {
        self.attempts = 0;
        self.next_attempt_at = Some(now + backoff_delay(0).as_secs_f64());
    }

    /// Bring the next attempt forward to `now` without touching the failure
    /// count. Used when the saved endpoint reappears in the device list, so the
    /// stream migrates back immediately instead of waiting out the backoff.
    pub fn request_now(&mut self, now: f64) {
        self.next_attempt_at = Some(now);
    }

    /// Whether a (re)build attempt is due at `now`.
    #[must_use]
    pub fn poll(&self, now: f64) -> SupervisorAction {
        match self.next_attempt_at {
            Some(at) if now >= at => SupervisorAction::Rebuild,
            _ => SupervisorAction::Idle,
        }
    }

    /// Record a failed attempt: grow the backoff and reschedule.
    ///
    /// `saturating_add` on the counter means an install that fails for days
    /// pins at `u32::MAX` rather than wrapping back to a 1 s hot loop; the
    /// delay itself is already clamped by [`backoff_delay`].
    pub fn record_failure(&mut self, now: f64) {
        self.attempts = self.attempts.saturating_add(1);
        self.next_attempt_at = Some(now + backoff_delay(self.attempts).as_secs_f64());
    }

    /// Record a successful (re)build: clear the cycle so nothing is due until
    /// the next stream death calls [`Self::begin`].
    pub fn record_success(&mut self) {
        self.attempts = 0;
        self.next_attempt_at = None;
    }

    /// Consecutive-failure count. Test-only: production reads it only through
    /// [`Self::poll`]'s scheduling. Gated so the lib target does not flag it as
    /// dead code under `-D warnings`.
    #[cfg(test)]
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Whether a reconnect cycle is in progress. Test-only; gated as above.
    #[cfg(test)]
    pub fn is_reconnecting(&self) -> bool {
        self.next_attempt_at.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_then_caps_at_thirty_seconds() {
        assert_eq!(backoff_delay(0), Duration::from_secs(1));
        assert_eq!(backoff_delay(1), Duration::from_secs(2));
        assert_eq!(backoff_delay(2), Duration::from_secs(4));
        assert_eq!(backoff_delay(3), Duration::from_secs(8));
        assert_eq!(backoff_delay(4), Duration::from_secs(16));
        // 2^5 = 32 > cap.
        assert_eq!(backoff_delay(5), BACKOFF_CAP);
        assert_eq!(backoff_delay(6), BACKOFF_CAP);
        // Absurd attempt counts saturate rather than overflow the shift.
        assert_eq!(backoff_delay(1_000), BACKOFF_CAP);
        // Including the boundaries of the shift itself (63 is the last width
        // `checked_shl` accepts on a u64; 64 and up return `None`).
        assert_eq!(backoff_delay(63), BACKOFF_CAP);
        assert_eq!(backoff_delay(64), BACKOFF_CAP);
        assert_eq!(backoff_delay(u32::MAX), BACKOFF_CAP);
    }

    #[test]
    fn begin_schedules_the_first_attempt_one_second_out() {
        let mut sup = AudioSupervisor::default();
        assert!(!sup.is_reconnecting());
        sup.begin(100.0);
        assert!(sup.is_reconnecting());
        assert_eq!(sup.attempts(), 0);
        // Nothing due before the 1 s backoff elapses.
        assert_eq!(sup.poll(100.5), SupervisorAction::Idle);
        // Due exactly at the deadline.
        assert_eq!(sup.poll(101.0), SupervisorAction::Rebuild);
    }

    #[test]
    fn repeated_failures_grow_the_backoff() {
        let mut sup = AudioSupervisor::default();
        sup.begin(0.0);
        assert_eq!(sup.poll(1.0), SupervisorAction::Rebuild);
        sup.record_failure(1.0); // attempt 1 -> next at 1.0 + 2 s
        assert_eq!(sup.attempts(), 1);
        assert_eq!(sup.poll(2.9), SupervisorAction::Idle);
        assert_eq!(sup.poll(3.0), SupervisorAction::Rebuild);
        sup.record_failure(3.0); // attempt 2 -> next at 3.0 + 4 s
        assert_eq!(sup.poll(6.9), SupervisorAction::Idle);
        assert_eq!(sup.poll(7.0), SupervisorAction::Rebuild);
    }

    #[test]
    fn success_clears_the_reconnect_cycle() {
        let mut sup = AudioSupervisor::default();
        sup.begin(0.0);
        sup.record_failure(1.0);
        sup.record_success();
        assert!(!sup.is_reconnecting());
        assert_eq!(sup.attempts(), 0);
        // Nothing is ever due once cleared.
        assert_eq!(sup.poll(1_000.0), SupervisorAction::Idle);
    }

    #[test]
    fn request_now_forces_an_immediate_attempt_without_resetting_attempts() {
        let mut sup = AudioSupervisor::default();
        sup.begin(0.0);
        sup.record_failure(0.0); // attempt 1, next at 2 s
        assert_eq!(sup.poll(0.5), SupervisorAction::Idle);
        // A device reappearing short-circuits the wait.
        sup.request_now(0.5);
        assert_eq!(sup.poll(0.5), SupervisorAction::Rebuild);
        // The failure count is preserved so backoff keeps growing if it fails again.
        assert_eq!(sup.attempts(), 1);
    }

    /// A long soak sees more than one outage. The second one must not inherit
    /// the first one's 30 s backoff — a reconnected stream resets the schedule
    /// to its beginning, so the next blip is retried 1 s later, not 30 s later.
    #[test]
    fn a_second_outage_after_a_success_restarts_the_backoff_at_one_second() {
        let mut sup = AudioSupervisor::default();
        // First outage: fail long enough to sit at the cap.
        sup.begin(0.0);
        for attempt in 0..8 {
            sup.record_failure(f64::from(attempt));
        }
        assert_eq!(sup.attempts(), 8);
        sup.record_success();

        // Second outage, hours later.
        sup.begin(10_000.0);
        assert_eq!(sup.attempts(), 0);
        assert_eq!(sup.poll(10_000.9), SupervisorAction::Idle);
        assert_eq!(sup.poll(10_001.0), SupervisorAction::Rebuild);
    }

    /// A device that never comes back must settle into a steady 30 s retry: the
    /// delay stops growing, the attempt counter saturates instead of wrapping
    /// (a wrap to 0 would turn the cap back into a 1 s hot loop), and the
    /// deadline stays exactly one cap-width out.
    #[test]
    fn a_permanently_absent_device_settles_into_a_steady_thirty_second_retry() {
        let mut sup = AudioSupervisor::default();
        sup.begin(0.0);

        // `begin` schedules the first attempt one backoff step out, so the
        // first thing due is at t = 1 s, not t = 0.
        let mut now = backoff_delay(0).as_secs_f64();
        for _ in 0..64 {
            assert_eq!(sup.poll(now), SupervisorAction::Rebuild);
            sup.record_failure(now);
            // Past attempt 5 the schedule is pinned at the cap.
            if sup.attempts() >= 5 {
                let cap = BACKOFF_CAP.as_secs_f64();
                assert_eq!(sup.poll(now + cap - 0.001), SupervisorAction::Idle);
                assert_eq!(sup.poll(now + cap), SupervisorAction::Rebuild);
                now += cap;
            } else {
                now += backoff_delay(sup.attempts()).as_secs_f64();
            }
        }
        assert_eq!(sup.attempts(), 64);

        // The counter saturates rather than wrapping, so the cap holds forever.
        sup.attempts = u32::MAX;
        sup.record_failure(now);
        assert_eq!(sup.attempts(), u32::MAX);
        assert_eq!(sup.poll(now + 29.9), SupervisorAction::Idle);
        assert_eq!(sup.poll(now + 30.0), SupervisorAction::Rebuild);
    }

    /// A fresh supervisor has no cycle in progress: `poll` never fires until
    /// something calls [`AudioSupervisor::begin`] (or `request_now`). Guards
    /// against a default `next_attempt_at` of `0.0` rebuilding on frame one.
    #[test]
    fn a_default_supervisor_never_rebuilds() {
        let sup = AudioSupervisor::default();
        assert!(!sup.is_reconnecting());
        assert_eq!(sup.attempts(), 0);
        assert_eq!(sup.poll(0.0), SupervisorAction::Idle);
        assert_eq!(sup.poll(86_400.0), SupervisorAction::Idle);
    }
}
