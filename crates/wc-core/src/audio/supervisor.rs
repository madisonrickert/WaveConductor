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
//! **This file is pure.** Every function is arithmetic over a clock passed in as
//! `f64` seconds; there is no cpal call, no thread, and no Bevy `Time` here,
//! which is what makes the whole state machine unit-testable with no audio
//! device (CI has none). Task 5's `supervise_audio` Bevy system is the only
//! place a clock is read and the actual stream rebuild is performed — and it
//! lives on the **main thread**, never the audio callback and never the render
//! thread. The rebuild itself (which can block on WASAPI enumeration) is
//! event-driven (error path / backoff tick), not a per-frame cost.
//!
//! ## The clock is required to be monotonic
//!
//! Every `now: f64` here **must** come from a monotonic source — in Bevy, that
//! is `Time<Real>::elapsed_secs_f64` (backed by `Instant`), *not* a wall clock.
//! This is a requirement, not a description. Deadlines are stored as absolute
//! times on the caller's clock, so a wall clock stepping **backward** (an NTP
//! correction overnight — entirely plausible in an unattended 8-hour run)
//! postpones the next reconnect attempt by the size of the jump. The failure
//! mode of getting this wrong is a kiosk that stays silent for however far the
//! clock stepped back.
//!
//! ## The contract Task 5 must honour
//!
//! [`AudioSupervisor::poll`] takes `&mut self` and **arms the next backoff step
//! as it hands back [`SupervisorAction::Rebuild`]** — it assumes the attempt it
//! is authorising will fail, and [`AudioSupervisor::record_success`] is what
//! walks that back. That is deliberate: a caller that acts on `Rebuild` and then
//! forgets to report the outcome degrades to "retried on the normal backoff
//! schedule", never to a cpal teardown-and-blocking-reopen every frame. Two
//! polls in the same frame cannot double-fire either.

use std::time::Duration;

use bevy::prelude::Resource;

/// Ceiling on the reconnect backoff. The delay doubles from 1 s and is clamped
/// here so an install left with a permanently-absent device retries at a steady
/// once-every-30-s rather than drifting toward hours.
pub const BACKOFF_CAP: Duration = Duration::from_secs(30);

/// How long a rebuilt stream must survive before it counts as a real recovery
/// and earns a reset of the failure count.
///
/// A half-awake HDMI endpoint (a TV mid-wake, an AVR renegotiating) will happily
/// *enumerate and open*, then drop the stream half a second later. If every such
/// open reset the backoff, the kiosk would churn cpal streams at ~1 Hz for the
/// rest of the night — each open being blocking and allocating, on the main
/// thread — which is precisely what the backoff exists to bound. So the reset is
/// earned by *liveness*, not by the open call returning `Ok`: a stream that
/// lived less than one full backoff period never really came up.
///
/// [`BACKOFF_CAP`] is the natural window: it is the longest the schedule ever
/// waits between attempts, so "survived at least one cap-width" is the same
/// statement as "outlived the retry loop that would have replaced it".
pub const STREAM_SETTLE_WINDOW: Duration = BACKOFF_CAP;

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
    /// A (re)build attempt is due now. The next backoff step is *already armed*
    /// — see [`AudioSupervisor::poll`].
    Rebuild,
}

/// Reconnect bookkeeping: the consecutive-failure counter, the scheduled time of
/// the next attempt, and when the last stream last came up — all driven by the
/// caller's monotonic clock.
///
/// A Bevy `Resource` so the main-thread supervisor system owns exactly one.
/// Times are `f64` seconds on whatever monotonic clock the caller uses (Task 5
/// uses `Time<Real>::elapsed_secs_f64`; see the module header — a wall clock is
/// **not** acceptable). This type never reads a clock itself.
#[derive(Resource, Debug, Default, Clone)]
pub struct AudioSupervisor {
    /// Rebuild attempts started since the last *settled* stream. Grows the
    /// backoff; reset by [`Self::begin`] when the stream that just died had
    /// survived [`STREAM_SETTLE_WINDOW`].
    attempts: u32,
    /// Monotonic time of the next scheduled attempt, or `None` when no
    /// reconnect cycle is in progress.
    next_attempt_at: Option<f64>,
    /// Monotonic time the stream last came up ([`Self::record_success`]), or
    /// `None` if it never has this session. The liveness half of the
    /// [`STREAM_SETTLE_WINDOW`] test.
    last_success_at: Option<f64>,
}

impl AudioSupervisor {
    /// Start (or restart) a reconnect cycle: schedule the next attempt one
    /// backoff step out, resetting the failure count first **if the stream that
    /// just died had settled** (it lived at least [`STREAM_SETTLE_WINDOW`]).
    ///
    /// That conditional reset is the whole flap defence, and `now` is the only
    /// moment at which it can be evaluated correctly: the elapsed time from
    /// the last recorded success to this death *is* the dead stream's lifetime.
    /// A stream that lived hours and then died is a fresh outage and starts the
    /// schedule over at 1 s. A stream that opened and dropped within the window
    /// never really came up, so the backoff keeps growing toward
    /// [`BACKOFF_CAP`] instead of pinning at a 1 s rebuild loop.
    ///
    /// # When to call this
    ///
    /// **Two triggers, not one.** Both are load-bearing for a kiosk:
    ///
    /// 1. The edge into [`crate::audio::state::AudioStatus::Reconnecting`] —
    ///    a *live* stream's cpal error callback fired (see
    ///    `state::mark_reconnecting_from_callback`).
    /// 2. **The engine failing to start at all**, i.e. a startup
    ///    `EngineBuildError::NoDefaultDevice` / `AudioStatus::Errored` with no
    ///    stream. There is no live stream here, so there is no error callback
    ///    and trigger 1 can *never* fire — yet this is the routine kiosk case:
    ///    the machine powers on (or reboots on a power blip) while the TV is
    ///    still asleep or before anyone selects the HDMI input, and the host
    ///    enumerates no output device at all. If `begin` is not called here, no
    ///    reconnect cycle is ever started and the installation is silent for the
    ///    night — even though the device-watcher will see the TV appear seconds
    ///    later.
    ///
    /// Do **not** "helpfully" narrow this to trigger 1. A boot with no device is
    /// recoverable, not terminal.
    ///
    /// Calling this while a cycle is already running simply reschedules it; it
    /// is not intended as a per-frame call.
    pub fn begin(&mut self, now: f64) {
        if self.last_stream_settled(now) {
            self.attempts = 0;
        }
        self.next_attempt_at = Some(now + backoff_delay(self.attempts).as_secs_f64());
    }

    /// Bring the next attempt forward to `now` without touching the failure
    /// count. Used when the saved endpoint reappears in the device list, so the
    /// stream migrates back immediately instead of waiting out the backoff.
    pub fn request_now(&mut self, now: f64) {
        self.next_attempt_at = Some(now);
    }

    /// Whether a (re)build attempt is due at `now` — and, when it is, **arm the
    /// next backoff step before returning**.
    ///
    /// Returning [`SupervisorAction::Rebuild`] consumes the due-ness: the
    /// failure counter is incremented and the deadline is pushed out by the new
    /// backoff, *as if the attempt this call authorises had already failed*.
    /// [`Self::record_success`] is what undoes that.
    ///
    /// This is why it takes `&mut self`. A `&self` version reports "a rebuild is
    /// due" without consuming it, so a caller that acts on `Rebuild` and forgets
    /// to record an outcome gets `Rebuild` again on the very next frame: a cpal
    /// stream teardown and blocking re-open at 60 Hz on the main thread, which
    /// is a worse failure than the silence this module exists to fix. Assuming
    /// failure makes that misuse impossible rather than merely loud — the worst
    /// a forgetful caller gets is "retried on the normal backoff schedule" — and
    /// it makes a double-poll within one frame a no-op.
    #[must_use]
    pub fn poll(&mut self, now: f64) -> SupervisorAction {
        match self.next_attempt_at {
            Some(at) if now >= at => {
                // Assume this attempt fails and schedule accordingly.
                // `saturating_add` means an install that fails for days pins at
                // `u32::MAX` rather than wrapping back to a 1 s hot loop; the
                // delay itself is already clamped by `backoff_delay`.
                self.attempts = self.attempts.saturating_add(1);
                self.next_attempt_at = Some(now + backoff_delay(self.attempts).as_secs_f64());
                SupervisorAction::Rebuild
            }
            _ => SupervisorAction::Idle,
        }
    }

    /// Record that a (re)build succeeded at `now`: clear the scheduled attempt
    /// [`poll`](Self::poll) armed, so nothing is due until the next stream death
    /// calls [`Self::begin`], and remember when the stream came up.
    ///
    /// This deliberately does **not** reset the failure count. A successful
    /// `open` is not evidence of a working endpoint — a half-awake HDMI device
    /// opens fine and dies moments later. The reset is earned by surviving
    /// [`STREAM_SETTLE_WINDOW`] and is therefore applied at the *next* death, in
    /// [`Self::begin`], where the stream's lifetime is actually known.
    pub fn record_success(&mut self, now: f64) {
        self.next_attempt_at = None;
        self.last_success_at = Some(now);
    }

    /// Whether the stream that came up at [`Self::last_success_at`] had been
    /// alive for at least [`STREAM_SETTLE_WINDOW`] by `now`.
    ///
    /// `false` when the stream never came up at all this session (nothing has
    /// been observed to settle, so nothing has earned a reset — the failure
    /// count carries on growing).
    fn last_stream_settled(&self, now: f64) -> bool {
        self.last_success_at
            .is_some_and(|up_at| now - up_at >= STREAM_SETTLE_WINDOW.as_secs_f64())
    }

    /// Consecutive-failure count. Test-only accessor: not part of the shipped
    /// API. Production reads this state only through [`Self::poll`]'s
    /// scheduling.
    #[cfg(test)]
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Whether a reconnect cycle is in progress. Test-only accessor; not part of
    /// the shipped API.
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
        // Each `Rebuild` arms the next step, so a caller that never reports an
        // outcome still walks the 1, 2, 4 s schedule.
        assert_eq!(sup.poll(1.0), SupervisorAction::Rebuild); // attempt 1 -> next at 3.0
        assert_eq!(sup.attempts(), 1);
        assert_eq!(sup.poll(2.9), SupervisorAction::Idle);
        assert_eq!(sup.poll(3.0), SupervisorAction::Rebuild); // attempt 2 -> next at 7.0
        assert_eq!(sup.attempts(), 2);
        assert_eq!(sup.poll(6.9), SupervisorAction::Idle);
        assert_eq!(sup.poll(7.0), SupervisorAction::Rebuild);
        assert_eq!(sup.attempts(), 3);
    }

    /// The Fix-2 guarantee, stated as a test: a caller that acts on `Rebuild`
    /// and reports *nothing* back must not be handed `Rebuild` again next frame.
    /// The `&self` version of `poll` did exactly that — a cpal teardown and
    /// blocking re-open at 60 Hz on the main thread.
    #[test]
    fn a_rebuild_is_not_handed_out_again_on_the_next_frame() {
        let mut sup = AudioSupervisor::default();
        sup.begin(0.0);
        assert_eq!(sup.poll(1.0), SupervisorAction::Rebuild);
        // Same frame (a double-poll) and the next few frames: nothing.
        assert_eq!(sup.poll(1.0), SupervisorAction::Idle);
        assert_eq!(sup.poll(1.016), SupervisorAction::Idle);
        assert_eq!(sup.poll(1.033), SupervisorAction::Idle);
        // The retry lands on the normal backoff schedule instead (2 s out).
        assert_eq!(sup.poll(2.9), SupervisorAction::Idle);
        assert_eq!(sup.poll(3.0), SupervisorAction::Rebuild);
    }

    #[test]
    fn success_clears_the_reconnect_cycle() {
        let mut sup = AudioSupervisor::default();
        sup.begin(0.0);
        assert_eq!(sup.poll(1.0), SupervisorAction::Rebuild);
        sup.record_success(1.0);
        assert!(!sup.is_reconnecting());
        // Nothing is ever due once cleared.
        assert_eq!(sup.poll(1_000.0), SupervisorAction::Idle);
    }

    #[test]
    fn request_now_forces_an_immediate_attempt_without_resetting_attempts() {
        let mut sup = AudioSupervisor::default();
        sup.begin(0.0);
        assert_eq!(sup.poll(1.0), SupervisorAction::Rebuild); // attempt 1, next at 3.0
        assert_eq!(sup.poll(1.5), SupervisorAction::Idle);
        // A device reappearing short-circuits the wait.
        sup.request_now(1.5);
        assert_eq!(sup.poll(1.5), SupervisorAction::Rebuild);
        // The failure count is preserved (and grown by the poll) so backoff
        // keeps climbing if this attempt fails too.
        assert_eq!(sup.attempts(), 2);
    }

    /// A long soak sees more than one outage. The second one must not inherit
    /// the first one's 30 s backoff — a stream that reconnected and then *stayed
    /// up* resets the schedule, so the next blip is retried 1 s later, not 30 s
    /// later.
    #[test]
    fn a_second_outage_after_a_healthy_stream_restarts_the_backoff_at_one_second() {
        let mut sup = AudioSupervisor::default();
        // First outage: fail long enough to sit at the cap.
        sup.begin(0.0);
        let mut now = backoff_delay(0).as_secs_f64();
        for _ in 0..8 {
            assert_eq!(sup.poll(now), SupervisorAction::Rebuild);
            now += backoff_delay(sup.attempts()).as_secs_f64();
        }
        assert_eq!(sup.attempts(), 8);
        // Then it comes back and stays up for hours.
        sup.record_success(now);

        sup.begin(now + 10_000.0);
        assert_eq!(sup.attempts(), 0);
        assert_eq!(sup.poll(now + 10_000.9), SupervisorAction::Idle);
        assert_eq!(sup.poll(now + 10_001.0), SupervisorAction::Rebuild);
    }

    /// The flapping endpoint: a half-awake HDMI device that *opens successfully*
    /// and then drops the stream half a second later, over and over. Every open
    /// looks like a success; none of them is one. The backoff must grow to the
    /// cap anyway — otherwise the kiosk rebuilds a cpal stream once a second for
    /// eight hours.
    #[test]
    fn a_flapping_endpoint_grows_the_backoff_to_the_cap_instead_of_looping_at_one_second() {
        let mut sup = AudioSupervisor::default();
        let mut now = 0.0_f64;
        // The stream comes up for the first time.
        sup.record_success(now);

        // Each cycle: it dies 0.5 s in, we wait out the backoff, rebuild, and
        // "succeed" — briefly. The delay must double, not stay at 1 s.
        for delay in [1.0_f64, 2.0, 4.0, 8.0, 16.0, 30.0, 30.0, 30.0] {
            now += 0.5;
            sup.begin(now);
            // Nothing is due until the (growing) backoff elapses …
            assert_eq!(sup.poll(now + delay - 0.001), SupervisorAction::Idle);
            // … and the attempt lands exactly one backoff step out.
            now += delay;
            assert_eq!(sup.poll(now), SupervisorAction::Rebuild);
            sup.record_success(now);
        }

        // Having settled at the cap, a stream that *does* survive the window
        // still earns the reset: the next genuine outage retries at 1 s.
        now += STREAM_SETTLE_WINDOW.as_secs_f64();
        sup.begin(now);
        assert_eq!(sup.attempts(), 0);
        assert_eq!(sup.poll(now + 0.9), SupervisorAction::Idle);
        assert_eq!(sup.poll(now + 1.0), SupervisorAction::Rebuild);
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
            // The poll armed the next step; nothing is due until it elapses.
            let delay = backoff_delay(sup.attempts()).as_secs_f64();
            assert_eq!(sup.poll(now + delay - 0.001), SupervisorAction::Idle);
            now += delay;
        }
        assert_eq!(sup.attempts(), 64);

        // The counter saturates rather than wrapping, so the cap holds forever.
        sup.attempts = u32::MAX - 1;
        assert_eq!(sup.poll(now), SupervisorAction::Rebuild);
        assert_eq!(sup.attempts(), u32::MAX);
        assert_eq!(sup.poll(now + 29.9), SupervisorAction::Idle);
        assert_eq!(sup.poll(now + 30.0), SupervisorAction::Rebuild);
        assert_eq!(sup.attempts(), u32::MAX);
    }

    /// A fresh supervisor has no cycle in progress: `poll` never fires until
    /// something calls [`AudioSupervisor::begin`] (or `request_now`). Guards
    /// against a default `next_attempt_at` of `0.0` rebuilding on frame one.
    #[test]
    fn a_default_supervisor_never_rebuilds() {
        let mut sup = AudioSupervisor::default();
        assert!(!sup.is_reconnecting());
        assert_eq!(sup.attempts(), 0);
        assert_eq!(sup.poll(0.0), SupervisorAction::Idle);
        assert_eq!(sup.poll(86_400.0), SupervisorAction::Idle);
    }

    /// Boot with no output device at all (the TV is still asleep): there is no
    /// stream, so no error callback will ever fire. `begin` must be callable
    /// straight off the failed startup build — see its docs — and it schedules
    /// the usual 1 s first retry even though nothing ever succeeded.
    #[test]
    fn begin_from_a_failed_startup_build_starts_the_cycle() {
        let mut sup = AudioSupervisor::default();
        // No `record_success` has ever happened: `last_success_at` is None.
        sup.begin(0.0);
        assert!(sup.is_reconnecting());
        assert_eq!(sup.poll(0.9), SupervisorAction::Idle);
        assert_eq!(sup.poll(1.0), SupervisorAction::Rebuild);
        // The TV finishes waking a few retries later and the stream sticks.
        sup.record_success(5.0);
        assert!(!sup.is_reconnecting());
        assert_eq!(sup.poll(10_000.0), SupervisorAction::Idle);
    }
}
