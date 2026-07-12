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

    /// Whether a reconnect cycle is in progress — i.e. an attempt is scheduled,
    /// whether by [`Self::begin`] (a stream death or a failed startup build) or
    /// by [`Self::request_now`] (the saved endpoint reappeared).
    ///
    /// [`supervise_audio`] reads this to decide whether it has anything to do at
    /// all: on a quiet frame (the overwhelmingly common case — stream healthy,
    /// no cycle) this is `false` and the system returns without touching cpal.
    #[must_use]
    pub fn is_reconnecting(&self) -> bool {
        self.next_attempt_at.is_some()
    }
}

/// Set when a stream rebuild succeeds; cleared when the sketch has been made to
/// re-enter its own state (which is what re-adds the synth graph), or when there
/// is no graph to restore.
///
/// ## Why a flag and not a direct call
///
/// A rebuild constructs a fresh `DspHost` with **no synth voice**, and only a
/// sketch's `OnEnter` produces the `Add*Synth` command that installs one. The
/// repair is therefore a `sketch → Home → sketch` round-trip through
/// [`crate::lifecycle::reload`] — but that machine can only be started when it is
/// **idle**. If a settings-restart reload is already in flight when the device
/// reconnects, clobbering its `SketchReloadState` would interleave two reloads
/// (wrong reason, wrong return state, a fade that never restores its volume). So
/// the intent is *recorded* here and fired on the first frame the reload machine
/// is idle — never dropped, which would leave a voiceless graph and a kiosk that
/// reports `Running` while playing nothing.
///
/// Read once per frame by [`supervise_audio`] (a `bool`; no allocation, no work on
/// a quiet frame).
#[cfg(not(target_arch = "wasm32"))]
#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct SynthGraphReloadPending(pub bool);

/// What [`supervise_audio`] should do about a pending synth-graph reload this
/// frame.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SynthGraphReload {
    /// Nothing is pending; do nothing.
    Nothing,
    /// Begin the `sketch → Home → sketch` reload and clear the flag.
    Begin,
    /// A reload is already in flight. Keep the flag and try again next frame —
    /// dropping it here is what would leave the sketch running with no voice.
    Defer,
    /// We are at `Home` (or otherwise not in a sketch): there is no synth graph to
    /// restore, and the round-trip would be a pointless flicker. Clear the flag;
    /// the next sketch entered runs `OnEnter` against the fresh `DspHost` anyway.
    Discard,
}

/// The pure gate behind [`SynthGraphReloadPending`]. Extracted so the decision is
/// unit-testable with no app, no cpal, and no reload machine.
///
/// Ordering matters: `Defer` outranks `Discard`, because a reload in flight is
/// *itself* on its way back into a sketch (its `return_state` is always a sketch),
/// and the frames it spends in `Home` must not be mistaken for "there is nothing
/// to restore".
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn synth_graph_reload_action(
    pending: bool,
    in_sketch: bool,
    reload_idle: bool,
) -> SynthGraphReload {
    if !pending {
        return SynthGraphReload::Nothing;
    }
    if !reload_idle {
        return SynthGraphReload::Defer;
    }
    if in_sketch {
        SynthGraphReload::Begin
    } else {
        SynthGraphReload::Discard
    }
}

/// Main-thread exclusive system (`Update`) that drives audio reconnection. This
/// is where the pure state machine above meets the real cpal stream.
///
/// ## Per-frame cost
///
/// On a quiet frame — stream healthy, no cycle scheduled — it reads three
/// resources and returns. **No enumeration, no stream build, no allocation.**
/// Both of those cpal calls block, and they happen only on a backoff-gated
/// attempt.
///
/// ## The decision
///
/// 1. A cycle *should* be running when the status is
///    [`crate::audio::state::AudioStatus::Reconnecting`] (a live stream's cpal
///    error callback fired)
///    **or** when there is no stream at all and the status is
///    `NotStarted`/`Errored` (the startup build failed — the kiosk booted before
///    its TV woke). The second trigger is not optional: with no stream there is
///    no error callback, so nothing else can ever start the cycle, and the
///    installation would stay silent all night while the device-watcher happily
///    reported the TV appearing.
/// 2. If one should be running and none is, [`AudioSupervisor::begin`] starts it.
/// 3. A cycle may also be armed while the stream is *healthy*:
///    `drain_device_topology` calls [`AudioSupervisor::request_now`] when the
///    operator's saved endpoint reappears, to migrate back to it. So the poll is
///    gated on [`AudioSupervisor::is_reconnecting`], not on the status.
/// 4. When [`AudioSupervisor::poll`] returns [`SupervisorAction::Rebuild`],
///    `crate::audio::engine::rebuild_engine` (private) runs. On success —
///    and *only* on success — [`AudioSupervisor::record_success`] disarms the
///    schedule. On failure nothing is reported: `poll` already armed the next
///    step, so a failed attempt simply retries one backoff step later. There is
///    no `record_failure`, by design.
///
/// The outcome is judged by `rebuild_engine`'s return value, **not** by whether
/// an `AudioStream` resource exists: a failed rebuild leaves the old, dead stream
/// installed, so "a stream is present" is not evidence of anything.
///
/// ## Silencing the command ring for the duration of the outage
///
/// Whenever a reconnect is wanted this also **removes** the non-send
/// `crate::audio::ring::AudioCommandSender`. A dead stream's callback stops
/// draining the command ring, but the sketches keep pushing per-frame `Set*Param`
/// commands, so within ~1 s the (64-slot) ring is full and every subsequent push
/// emits an allocating `warn!` on the render thread — for the rest of the soak, if
/// the endpoint never comes back. Every consumer of the sender already takes it as
/// `Option<NonSendMut<…>>` and skips cleanly when it is absent, and
/// `rebuild_engine` re-installs a fresh one on the first successful rebuild.
///
/// ## Step 5: put the synth graph back (Task 5R)
///
/// A successful rebuild raises [`SynthGraphReloadPending`] and immediately tries
/// to spend it (see `apply_pending_synth_graph_reload`): the fresh `DspHost` has
/// no voice, and only a sketch's `OnEnter` can install one, so the sketch is made
/// to re-enter its own state through [`crate::lifecycle::reload`]. A **failed**
/// rebuild raises nothing (there is no new host to populate), and at `Home` the
/// flag is discarded rather than spent (no graph exists there; the round-trip
/// would be a pointless flicker).
///
/// **It fires exactly once per successful rebuild.** The flag is set only on
/// `rebuild_engine(world) == true`, and `rebuild_engine` runs only on a
/// [`SupervisorAction::Rebuild`], which [`AudioSupervisor::poll`] hands out at
/// most once per backoff deadline (it consumes the due-ness as it returns).
/// [`AudioSupervisor::record_success`] then disarms the schedule entirely, so the
/// next frames are quiet ones. The reload it begins cannot feed back: it changes
/// `AppState`, not `AudioState`, so nothing about it can raise
/// `Reconnecting` or start another cycle.
///
/// The pending flag is the only per-frame cost this adds on a quiet frame: one
/// `bool` read.
///
/// The clock is `Time<Real>` (`elapsed_secs_f64`), which is monotonic. A wall
/// clock would be a bug: an NTP correction stepping time backward overnight
/// postpones the next attempt by the size of the jump.
///
/// Runs on the main thread only — never the audio callback, never the render
/// thread.
#[cfg(not(target_arch = "wasm32"))]
pub fn supervise_audio(world: &mut bevy::prelude::World) {
    use bevy::prelude::{Real, Time};

    use crate::audio::engine::{rebuild_engine, AudioStream};
    use crate::audio::state::{AudioState, AudioStatus};

    // A synth-graph reload owed from an earlier rebuild that landed while another
    // reload was in flight. One `bool` read on a quiet frame; the flag is almost
    // always `false`.
    if world.resource::<SynthGraphReloadPending>().0 {
        apply_pending_synth_graph_reload(world);
    }

    let now = world.resource::<Time<Real>>().elapsed_secs_f64();
    let status = world.resource::<AudioState>().status;
    let has_stream = world.get_non_send::<AudioStream>().is_some();

    // Trigger 1: a live stream died. Trigger 2: no stream ever came up.
    let wants_reconnect = matches!(status, AudioStatus::Reconnecting)
        || (!has_stream && matches!(status, AudioStatus::NotStarted | AudioStatus::Errored));

    if wants_reconnect {
        // Take the command ring away for the duration of the outage. A dead stream
        // does not drain it, so it fills within ~1 s of per-frame `Set*Param`
        // pushes — and from then on *every* push logs an allocating, formatted
        // `warn!` ("audio command ring full; dropping … param update") on the
        // render thread, every frame, for as long as the outage lasts. On a kiosk
        // whose only endpoint was permanently removed the supervisor settles into a
        // 30 s retry that always fails, so that is ~10^5–10^6 log lines over an
        // 8-hour soak, on exactly the thread whose steady-state allocation
        // `AGENTS.md` forbids.
        //
        // Removing the resource is the honest fix rather than gating each push
        // helper on the status: there genuinely *is* nowhere to send a command, and
        // every consumer of the sender already takes it as `Option<NonSendMut<…>>`
        // and skips cleanly (that is the headless-test path they were written for).
        // `rebuild_engine` re-installs a fresh sender on the first successful
        // rebuild, so a recovered stream resumes receiving param updates with no
        // further wiring — and the sketch's own `OnEnter`, re-run by the
        // synth-graph reload below, re-seeds the params it missed.
        //
        // Cheap and idempotent: after the first frame of an outage there is nothing
        // left to remove, and this is a hash lookup that returns `None`.
        world.remove_non_send::<crate::audio::ring::AudioCommandSender>();
    }

    let action = {
        let mut supervisor = world.resource_mut::<AudioSupervisor>();
        if wants_reconnect && !supervisor.is_reconnecting() {
            supervisor.begin(now);
        }
        if !supervisor.is_reconnecting() {
            // The quiet frame: nothing scheduled, nothing to do.
            return;
        }
        supervisor.poll(now)
    };
    if action == SupervisorAction::Idle {
        return;
    }

    if rebuild_engine(world) {
        world.resource_mut::<AudioSupervisor>().record_success(now);
        // The rebuilt stream is playing a *voiceless* DspHost. Owe a sketch
        // re-entry, and spend it now if the reload machine is free.
        world.resource_mut::<SynthGraphReloadPending>().0 = true;
        apply_pending_synth_graph_reload(world);
    }
    // On failure: report nothing (and owe nothing — there is no new host to
    // populate). `poll` already armed the next backoff step.
}

/// Spend a pending synth-graph reload if this frame allows it.
///
/// The fresh `DspHost` a rebuild installs has no synth voice, and only a sketch's
/// `OnEnter(AppState::…)` emits the `Add*Synth` command that installs one — which
/// a stream rebuild does not re-run. So the repair is to make the sketch re-enter
/// its own state: [`crate::lifecycle::reload`] already performs exactly that
/// `sketch → Home → sketch` round-trip, and
/// [`crate::lifecycle::reload::ReloadReason::AudioDeviceReconnect`] gives it a
/// zero-length fade and no master-volume command, so it is instant and silent.
///
/// The three outcomes are decided by the pure [`synth_graph_reload_action`]; see
/// its docs and [`SynthGraphReloadPending`] for why a reload already in flight
/// **defers** rather than being dropped or clobbered.
#[cfg(not(target_arch = "wasm32"))]
fn apply_pending_synth_graph_reload(world: &mut bevy::prelude::World) {
    use bevy::prelude::{State, Time};

    use crate::audio::state::AudioState;
    use crate::lifecycle::reload::{ReloadReason, SketchReloadState};
    use crate::lifecycle::state::AppState;

    let pending = world.resource::<SynthGraphReloadPending>().0;
    let current = *world.resource::<State<AppState>>().get();
    let reload_idle = world.resource::<SketchReloadState>().is_idle();

    match synth_graph_reload_action(pending, current.is_sketch(), reload_idle) {
        SynthGraphReload::Nothing | SynthGraphReload::Defer => {}
        SynthGraphReload::Discard => {
            world.resource_mut::<SynthGraphReloadPending>().0 = false;
        }
        SynthGraphReload::Begin => {
            // `Time` (not `Time<Real>`): the reload machine measures its fade legs
            // against the same clock `drive_reload_state` reads.
            let now = world.resource::<Time>().elapsed();
            // Carried for symmetry with the other reload paths; a reconnect never
            // dips the master volume, so it is never actually restored from here.
            let pre_fade_volume = world.resource::<AudioState>().volume;
            world.resource_mut::<SketchReloadState>().begin_fade_out(
                now,
                pre_fade_volume,
                current,
                ReloadReason::AudioDeviceReconnect,
            );
            world.resource_mut::<SynthGraphReloadPending>().0 = false;
            tracing::info!(
                sketch = ?current,
                "audio stream rebuilt mid-sketch — re-entering the sketch to restore its synth graph"
            );
        }
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

    /// The status→action wiring `supervise_audio` relies on, pinned as a test:
    /// the status has just become `Reconnecting` and no cycle is scheduled, so
    /// the system calls `begin` — and one backoff step later an attempt is due.
    /// If a later edit changes `begin`'s scheduling, this is what catches it.
    #[test]
    fn a_fresh_reconnecting_status_begins_a_cycle_then_becomes_due() {
        let mut sup = AudioSupervisor::default();
        assert!(!sup.is_reconnecting());
        sup.begin(10.0);
        assert!(sup.is_reconnecting());
        assert_eq!(sup.poll(10.0 + 0.9), SupervisorAction::Idle);
        assert_eq!(sup.poll(10.0 + 1.0), SupervisorAction::Rebuild);
    }

    /// `supervise_audio`'s quiet frame, stated as a test. A healthy stream has
    /// no cycle scheduled, so the system's `is_reconnecting` gate is `false` and
    /// it returns *before* polling — which is what keeps a blocking cpal
    /// enumeration off the 60 Hz path. `record_success` must therefore be called
    /// exactly once per stream-up (at startup and after a rebuild), **never**
    /// every healthy frame: doing the latter would keep resetting
    /// `last_success_at` to `now`, so no stream would ever be seen to have
    /// survived `STREAM_SETTLE_WINDOW` and the backoff would never reset.
    #[test]
    fn a_healthy_stream_leaves_no_cycle_armed_and_still_earns_its_settle() {
        let mut sup = AudioSupervisor::default();
        // Startup succeeded: the stream came up at t = 0.
        sup.record_success(0.0);
        assert!(!sup.is_reconnecting());

        // Hours of quiet frames later, the stream dies. Because success was
        // recorded once (not per frame), the stream is seen to have settled and
        // the backoff restarts at 1 s rather than inheriting an old attempt count.
        let death = 8.0 * 3600.0;
        sup.begin(death);
        assert_eq!(sup.attempts(), 0);
        assert_eq!(sup.poll(death + 1.0), SupervisorAction::Rebuild);
    }

    /// The command ring's fate during an outage. A dead stream stops draining it,
    /// but the sketches keep pushing per-frame `Set*Param` commands — so within a
    /// second the 64-slot ring is full and every further push emits an allocating,
    /// formatted `warn!` on the render thread. For the rest of the soak, if the
    /// endpoint never returns.
    #[cfg(not(target_arch = "wasm32"))]
    mod command_ring {
        use bevy::prelude::*;

        use super::*;
        use crate::audio::command::AudioCommand;
        use crate::audio::engine::SimulateNoOutputDevice;
        use crate::audio::ring::{AudioCommandSender, RING_CAPACITY};
        use crate::audio::state::{AudioState, AudioStatus};

        /// The supervisor with a command ring but no cpal stream. The
        /// `SimulateNoOutputDevice` seam is defensive: it guarantees that even if a
        /// backoff deadline somehow elapsed mid-test, no real audio device is opened.
        fn app_with_a_command_ring() -> App {
            let mut app = App::new();
            app.add_plugins(MinimalPlugins);
            app.init_resource::<AudioState>();
            app.init_resource::<AudioSupervisor>();
            app.init_resource::<SynthGraphReloadPending>();
            app.insert_resource(SimulateNoOutputDevice);
            let (producer, _consumer) = rtrb::RingBuffer::<AudioCommand>::new(RING_CAPACITY);
            app.insert_non_send(AudioCommandSender::new(producer));
            app.add_systems(Update, supervise_audio);
            app
        }

        /// A healthy stream keeps its ring — the sketches must go on sending param
        /// updates. This is the frame the app spends ~all of its life in, and the
        /// negative that makes the test below mean something.
        #[test]
        fn a_healthy_stream_keeps_its_command_ring() {
            let mut app = app_with_a_command_ring();
            app.world_mut().resource_mut::<AudioState>().status = AudioStatus::Running;

            for _ in 0..5 {
                app.update();
            }

            assert!(
                app.world().get_non_send::<AudioCommandSender>().is_some(),
                "a running stream must keep receiving param updates",
            );
            assert!(!app.world().resource::<AudioSupervisor>().is_reconnecting());
        }

        /// The outage: the sender is taken away, so the sketches' `Option<NonSendMut<…>>`
        /// param pushes skip instead of hammering a full ring and logging every frame.
        #[test]
        fn a_dead_stream_takes_the_command_ring_away_for_the_duration_of_the_outage() {
            let mut app = app_with_a_command_ring();
            app.world_mut().resource_mut::<AudioState>().status = AudioStatus::Reconnecting;

            app.update();

            assert!(
                app.world().get_non_send::<AudioCommandSender>().is_none(),
                "nothing drains the ring while the stream is dead; stop pushing to it",
            );
            assert!(
                app.world().resource::<AudioSupervisor>().is_reconnecting(),
                "and the reconnect cycle is armed — the removal is not a substitute \
                 for recovery, it is what keeps the recovery quiet",
            );

            // Idempotent: many more frames of outage remove nothing further and
            // cost a failed lookup each. `rebuild_engine` is the only thing that
            // puts a sender back, and only on a rebuild that actually succeeded.
            for _ in 0..20 {
                app.update();
            }
            assert!(app.world().get_non_send::<AudioCommandSender>().is_none());
        }

        /// A boot that never found a device (`NotStarted`, no stream) is the same
        /// outage seen from the other end, and must not leave a sender behind either
        /// — the sketches would push into a ring no callback will ever drain.
        #[test]
        fn a_stream_that_never_came_up_takes_the_command_ring_away_too() {
            let mut app = app_with_a_command_ring();
            // `NotStarted` + no `AudioStream`: `start_audio_engine` took its `Err`
            // arm. (It does not install a sender either; this test inserts one to
            // prove the removal is driven by the *status*, not by luck.)
            app.update();

            assert!(app.world().get_non_send::<AudioCommandSender>().is_none());
            assert!(app.world().resource::<AudioSupervisor>().is_reconnecting());
        }
    }

    /// Task 5R's gate. A rebuilt stream plays a `DspHost` with no synth voice, so
    /// a successful rebuild owes the sketch a re-entry — but only *in* a sketch,
    /// and only when the reload machine is free.
    #[cfg(not(target_arch = "wasm32"))]
    mod synth_graph {
        use super::*;

        /// The quiet frame: nothing owed, nothing done. This is the state the app
        /// is in for essentially all of an 8-hour soak.
        #[test]
        fn nothing_pending_is_nothing_to_do() {
            assert_eq!(
                synth_graph_reload_action(false, true, true),
                SynthGraphReload::Nothing,
            );
            assert_eq!(
                synth_graph_reload_action(false, false, false),
                SynthGraphReload::Nothing,
            );
        }

        /// The case the whole task exists for: the TV woke, the stream was
        /// rebuilt mid-sketch, and the fresh `DspHost` has no voice. Re-enter the
        /// sketch.
        #[test]
        fn a_rebuild_in_a_sketch_with_an_idle_reload_begins_the_round_trip() {
            assert_eq!(
                synth_graph_reload_action(true, true, true),
                SynthGraphReload::Begin,
            );
        }

        /// At `Home` there is no synth graph to restore and the `Home → Home` hop
        /// would be a pointless flicker. Drop the intent; the next sketch entered
        /// runs its `OnEnter` against the fresh host anyway.
        #[test]
        fn a_rebuild_at_home_discards_the_intent_rather_than_flickering() {
            assert_eq!(
                synth_graph_reload_action(true, false, true),
                SynthGraphReload::Discard,
            );
        }

        /// A settings-restart reload is already walking `sketch → Home → sketch`
        /// when the device reconnects. Starting a second one would clobber its
        /// `SketchReloadState` — wrong reason, wrong return state, a dipped volume
        /// that is never restored. Wait instead.
        ///
        /// `Defer` must outrank `Discard`: an in-flight reload spends a frame or
        /// two *at* `Home`, and mistaking those for "nothing to restore" would
        /// drop the intent and leave the sketch running silent for the rest of the
        /// night — which is exactly the bug Task 5R exists to close.
        #[test]
        fn a_reload_already_in_flight_defers_and_is_never_dropped() {
            assert_eq!(
                synth_graph_reload_action(true, true, false),
                SynthGraphReload::Defer,
            );
            assert_eq!(
                synth_graph_reload_action(true, false, false),
                SynthGraphReload::Defer,
                "mid-reload at the Home hop: still owed, not discarded",
            );
        }
    }
}
