//! Per-slot presence debounce, admission dwell, and graceful fade envelope
//! (pure, main-thread).
//!
//! Three pure decisions, one per tracked-body slot:
//!
//! - [`presence_decision`]: the presence-hold debounce. A stationary,
//!   partially-framed body sits right at the landmark model's
//!   `presence_threshold`, so its per-frame presence flag chatters; the hold
//!   bridges brief dropouts so the published `present` does not flicker.
//! - [`admit_step`]: the admission dwell gate. A newly-detected body must
//!   persist for [`ADMIT_DWELL`] before its fade-in begins. This is the
//!   busy-road defence: a kiosk camera pointed anywhere near foot/road
//!   traffic sees a stream of half-second walk-through detections, and
//!   without the dwell every one of them would claim a slot, flare in
//!   (ignite boost and all), and fade out over seconds — constant visual
//!   churn from people who never intended to interact. With the dwell, a
//!   passer-by who crosses the frame in under [`ADMIT_DWELL`] never ignites
//!   and their slot frees again immediately (no fade tail, and the worker
//!   skips its reservation for never-admitted tracks — see
//!   `pipeline::RESERVE_MIN_ACTIVE`).
//! - [`fade_step`]: the appearance/disappearance envelope. `fade` eases toward
//!   1.0 while a body is present/held ([`FADE_ATTACK_TAU`]) and toward 0.0
//!   once it is gone ([`FADE_RELEASE_TAU`]), using the frame-rate-independent
//!   `1 − exp(−dt/τ)` idiom with `dt` capped at [`FADE_DT_CAP`]. A slot is
//!   freed (its `TrackedBody` removed and its worker-side channel eligible for
//!   reuse) only once the fade reaches 0 — this is what lets figures appear
//!   and disappear gracefully instead of popping.
//!
//! All are pure functions over plain values so the timing behaviour is
//! unit-tested without standing up a worker or a clock.
//!
//! Timing coupling: the worker reserves a lost slot for
//! `pipeline::SLOT_RESERVE` before letting a *new* person claim it. That
//! reservation must cover [`PRESENCE_HOLD`] plus the full release
//! ([`FADE_RELEASE_TAU`] · ln(1/[`FADE_DONE_EPSILON`]) ≈ 3.6 s) so a mask
//! channel is never handed to a newcomer while the previous occupant is still
//! fading out on screen. The reservation applies only to tracks that lived
//! long enough to have been admitted here — a track lost before
//! `pipeline::RESERVE_MIN_ACTIVE` (strictly less than [`ADMIT_DWELL`]) can
//! never have ignited a fade, so the worker frees it immediately instead;
//! otherwise road traffic would convert every slot into a multi-second
//! reserved zombie and starve a genuine new person of capacity.

use std::time::Duration;

/// How long to keep publishing a slot's last pose after the worker stops
/// reporting that person, debouncing brief detection dropouts. Long enough to
/// bridge presence-threshold thrash on a marginal body, short enough that a
/// real exit starts the fade-out promptly.
pub const PRESENCE_HOLD: Duration = Duration::from_millis(300);

/// How long a newly-detected body must persist before its fade-in begins (the
/// admission dwell — see the module doc's busy-road rationale). Sized against
/// the sibling taus: comfortably longer than a typical walk-through detection
/// blip at road distance (a few hundred ms) and longer than [`PRESENCE_HOLD`]
/// (so a mere hold cannot admit anyone), but shorter than the ~1 s a person
/// takes to stop and face the screen — an intentional visitor still sees
/// their flame catch essentially as they settle. The [`FADE_ATTACK_TAU`]
/// attack starting *after* the dwell adds ~0.6 s of ease-in on top, which
/// reads as intentional rather than laggy. A held dropout
/// ([`PresenceDecision::Held`]) does **not** re-arm the dwell.
pub const ADMIT_DWELL: Duration = Duration::from_millis(700);

/// Fade attack time constant, seconds: how fast a newly-present body eases
/// toward full `fade = 1.0`.
pub const FADE_ATTACK_TAU: f32 = 0.6;

/// Fade release time constant, seconds: how fast an absent body eases toward
/// `fade = 0.0` (the graceful disappearance).
pub const FADE_RELEASE_TAU: f32 = 1.2;

/// Per-step `dt` cap, seconds, so a frame hitch cannot snap an envelope.
pub const FADE_DT_CAP: f32 = 0.05;

/// Release fades are exponential and never mathematically reach zero; below
/// this the fade snaps to exactly 0.0 and the slot is freed. 0.05 puts the
/// free point at ≈ `ln(1/0.05) · FADE_RELEASE_TAU` ≈ 3.6 s after presence
/// ends — invisible on screen well before that.
pub const FADE_DONE_EPSILON: f32 = 0.05;

/// Outcome of the presence-hold debounce for one slot's frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresenceDecision {
    /// The worker reports the person: publish and (re)arm the hold.
    Present,
    /// No detection, but still within the hold window: keep the last pose.
    Held,
    /// No detection and the hold window has elapsed: the person really left
    /// (the fade-out begins).
    Absent,
}

/// Decide the debounced presence for one slot frame, and the (possibly
/// re-armed) hold deadline. A present frame arms `now + PRESENCE_HOLD`; an
/// absent frame is [`PresenceDecision::Held`] until that deadline passes,
/// then [`PresenceDecision::Absent`]. An absent frame never extends the
/// deadline.
#[must_use]
pub fn presence_decision(
    frame_present: bool,
    now: Duration,
    hold_until: Duration,
) -> (PresenceDecision, Duration) {
    if frame_present {
        (PresenceDecision::Present, now + PRESENCE_HOLD)
    } else if now < hold_until {
        (PresenceDecision::Held, hold_until)
    } else {
        (PresenceDecision::Absent, hold_until)
    }
}

/// Advance a slot's admission-dwell state by one frame (see [`ADMIT_DWELL`]
/// and the module doc). `admitted` / `present_since` are the slot's carried
/// dwell state; the return value is the updated pair.
///
/// - [`PresenceDecision::Present`]: the dwell clock runs from the first
///   present frame of the current visit (`present_since`); once
///   `now − present_since ≥ ADMIT_DWELL` the slot is admitted and the fade
///   may attack.
/// - [`PresenceDecision::Held`]: state carries unchanged — a held dropout
///   neither advances nor **re-arms** the dwell (the bridge contract shared
///   with [`presence_decision`]).
/// - [`PresenceDecision::Absent`]: the visit is over; `present_since` clears
///   so the next appearance dwells afresh. `admitted` is deliberately KEPT —
///   it lasts until the slot fully frees (fade reaches 0; the caller resets
///   it there), so a person who drops out and re-associates to their slot
///   mid-fade re-ignites immediately instead of standing through a second
///   dwell. Only a slot that has released to empty demands a fresh dwell.
#[must_use]
pub fn admit_step(
    admitted: bool,
    present_since: Option<Duration>,
    decision: PresenceDecision,
    now: Duration,
) -> (bool, Option<Duration>) {
    match decision {
        PresenceDecision::Present => {
            let since = present_since.unwrap_or(now);
            (
                admitted || now.saturating_sub(since) >= ADMIT_DWELL,
                Some(since),
            )
        }
        PresenceDecision::Held => (admitted, present_since),
        PresenceDecision::Absent => (admitted, None),
    }
}

/// Advance a slot's fade envelope by `dt` seconds toward 1.0 (present/held)
/// or 0.0 (absent). Frame-rate independent (`1 − exp(−dt/τ)`), `dt` capped at
/// [`FADE_DT_CAP`]; an absent fade at or below [`FADE_DONE_EPSILON`] snaps to
/// exactly 0.0 (the "slot is now free" signal).
#[must_use]
pub fn fade_step(fade: f32, present: bool, dt: f32) -> f32 {
    let dt = dt.clamp(0.0, FADE_DT_CAP);
    let (target, tau) = if present {
        (1.0, FADE_ATTACK_TAU)
    } else {
        (0.0, FADE_RELEASE_TAU)
    };
    // k = fraction of the remaining distance covered this step.
    let k = 1.0 - (-dt / tau).exp();
    let next = (fade + (target - fade) * k).clamp(0.0, 1.0);
    if !present && next <= FADE_DONE_EPSILON {
        0.0
    } else {
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_decision_arms_hold_on_present() {
        let now = Duration::from_secs(1);
        let (d, hold_until) = presence_decision(true, now, Duration::ZERO);
        assert!(matches!(d, PresenceDecision::Present));
        assert_eq!(hold_until, now + PRESENCE_HOLD);
    }

    #[test]
    fn presence_decision_holds_through_brief_dropout() {
        // Armed at t = 1000 ms → deadline = 1000 + PRESENCE_HOLD.
        let hold_until = Duration::from_secs(1) + PRESENCE_HOLD;
        // A dropout just before the deadline is held, and does not extend it.
        let now = hold_until.saturating_sub(Duration::from_millis(1));
        let (d, hu) = presence_decision(false, now, hold_until);
        assert!(matches!(d, PresenceDecision::Held));
        assert_eq!(hu, hold_until, "an absent frame must not extend the hold");
    }

    #[test]
    fn presence_decision_drops_once_hold_elapses() {
        let hold_until = Duration::from_secs(1) + PRESENCE_HOLD;
        assert!(matches!(
            presence_decision(false, hold_until, hold_until).0,
            PresenceDecision::Absent
        ));
        assert!(matches!(
            presence_decision(false, hold_until + Duration::from_millis(50), hold_until).0,
            PresenceDecision::Absent
        ));
    }

    /// Attack reaches ~63% of the way to 1.0 after one time constant,
    /// regardless of step size (frame-rate independence).
    #[test]
    fn fade_attack_is_frame_rate_independent() {
        let steps_of = |dt: f32, total: f32| {
            let mut fade = 0.0;
            let mut t = 0.0;
            while t < total {
                fade = fade_step(fade, true, dt);
                t += dt;
            }
            fade
        };
        let fine = steps_of(1.0 / 240.0, FADE_ATTACK_TAU);
        let coarse = steps_of(1.0 / 30.0, FADE_ATTACK_TAU);
        assert!((fine - 0.632).abs() < 0.02, "fine={fine}");
        assert!(
            (fine - coarse).abs() < 0.02,
            "rate-independent: fine={fine} coarse={coarse}"
        );
    }

    /// Release decays toward zero and snaps to exactly 0.0 at the epsilon,
    /// around ln(1/ε)·τ ≈ 3.6 s after presence ends.
    #[test]
    fn fade_release_reaches_exact_zero() {
        let mut fade = 1.0_f32;
        let dt = 1.0 / 60.0;
        let mut t = 0.0_f32;
        while fade > 0.0 {
            fade = fade_step(fade, false, dt);
            t += dt;
            assert!(t < 10.0, "release must terminate");
        }
        #[allow(clippy::float_cmp, reason = "the snap-to-zero contract is exact")]
        {
            assert_eq!(fade, 0.0, "release snaps to exactly zero");
        }
        let expect = FADE_RELEASE_TAU * (1.0 / FADE_DONE_EPSILON).ln();
        assert!(
            (t - expect).abs() < 0.3,
            "free point ≈ {expect:.2} s, got {t:.2} s"
        );
    }

    /// A hitch (huge dt) is capped: one step can never snap the envelope.
    #[test]
    fn fade_dt_is_capped() {
        let hitch = fade_step(0.0, true, 5.0);
        let capped = fade_step(0.0, true, FADE_DT_CAP);
        assert!((hitch - capped).abs() < 1e-6, "dt caps at FADE_DT_CAP");
        assert!(hitch < 0.1, "one capped step is a small move");
    }

    /// Attack is faster than release (τ 0.6 vs 1.2).
    #[test]
    fn fade_attack_outpaces_release() {
        let up = fade_step(0.5, true, 1.0 / 60.0) - 0.5;
        let down = 0.5 - fade_step(0.5, false, 1.0 / 60.0);
        assert!(up > down, "attack {up} must outpace release {down}");
    }

    /// Drive one slot through the full presence-hold + dwell + fade stack the
    /// way `poll_body_worker` does, at a fixed frame rate. `present_at(t)`
    /// scripts the worker's raw per-frame presence flag. Returns the final
    /// `(admitted, fade)` and the peak fade seen.
    fn run_admission(
        frames: u32,
        frame_ms: u64,
        present_at: impl Fn(Duration) -> bool,
    ) -> (bool, f32, f32) {
        let dt = duration_secs(frame_ms);
        let mut hold_until = Duration::ZERO;
        let mut admitted = false;
        let mut present_since = None;
        let mut fade = 0.0_f32;
        let mut peak = 0.0_f32;
        // Mirrors SlotRuntime.target.present: true while Present/Held (the
        // held target keeps its last pose), false once Absent.
        let mut target_present = false;
        for i in 0..frames {
            let now = Duration::from_millis(u64::from(i) * frame_ms);
            let (decision, hu) = presence_decision(present_at(now), now, hold_until);
            hold_until = hu;
            (admitted, present_since) = admit_step(admitted, present_since, decision, now);
            match decision {
                PresenceDecision::Present => target_present = true,
                PresenceDecision::Held => {}
                PresenceDecision::Absent => target_present = false,
            }
            // The publisher's contract: the fade attacks only for an ADMITTED
            // present/held slot, and admission resets when the slot frees.
            fade = fade_step(fade, target_present && admitted, dt);
            peak = peak.max(fade);
            if fade <= 0.0 && !target_present {
                admitted = false;
            }
        }
        (admitted, fade, peak)
    }

    /// Lossless small-millisecond → f32 seconds for the fixture loop.
    fn duration_secs(ms: u64) -> f32 {
        Duration::from_millis(ms).as_secs_f32()
    }

    /// Busy-road scenario: a walker crosses the frame in 500 ms (< the 700 ms
    /// dwell). They must never ignite — fade stays exactly 0 the whole time —
    /// and once absent the slot state is immediately clean (no release tail).
    #[test]
    fn walker_crossing_never_ignites() {
        let (admitted, fade, peak) = run_admission(120, 33, |t| t < Duration::from_millis(500));
        assert!(!admitted, "a 500 ms visit must never be admitted");
        assert!(peak.abs() < f32::EPSILON, "fade must never leave 0: {peak}");
        assert!(fade.abs() < f32::EPSILON);
    }

    /// A person who arrives and stays ignites after the dwell with the normal
    /// attack: fade is still 0 at the dwell boundary, then rises.
    #[test]
    fn staying_person_ignites_after_dwell() {
        // 700 ms dwell at 33 ms frames ≈ frame 22.
        let (_, fade_at_dwell, peak_at_dwell) = run_admission(21, 33, |_| true);
        assert!(
            peak_at_dwell.abs() < f32::EPSILON,
            "no fade before the dwell elapses: {peak_at_dwell}"
        );
        let (admitted, fade, _) = run_admission(120, 33, |_| true);
        assert!(admitted, "a staying person must be admitted");
        assert!(
            fade > 0.9,
            "fade attacks normally after admission: {fade} (vs {fade_at_dwell} at dwell)"
        );
    }

    /// A brief 200 ms dropout mid-performance is bridged by PRESENCE_HOLD and
    /// must NOT re-arm the dwell: the fade keeps attacking straight through.
    #[test]
    fn held_dropout_does_not_rearm_dwell() {
        let dropout = |t: Duration| {
            // Present 0..2 s, a 200 ms dropout, present again.
            !(Duration::from_secs(2)..Duration::from_millis(2200)).contains(&t)
        };
        let (admitted, fade, _) = run_admission(90, 33, dropout);
        assert!(admitted, "the dropout must not revoke admission");
        // ~3 s total: an uninterrupted attack would sit near 1; a re-armed
        // dwell (700 ms of forced zero-target mid-run) could not recover this
        // high by frame 90.
        let (_, uninterrupted, _) = run_admission(90, 33, |_| true);
        assert!(
            (fade - uninterrupted).abs() < 0.05,
            "held dropout must not dent the envelope: {fade} vs {uninterrupted}"
        );
    }

    /// Pre-admission dwell survives a held dropout too: 400 ms present,
    /// 200 ms held dropout, then present again admits at the original
    /// `present_since` clock (700 ms total), not 700 ms after the resume.
    #[test]
    fn dwell_clock_carries_across_a_held_dropout() {
        let mut admitted = false;
        let mut present_since = None;
        let mut hold_until = Duration::ZERO;
        let script =
            |t: Duration| !(Duration::from_millis(400)..Duration::from_millis(600)).contains(&t);
        for i in 0..30_u64 {
            let now = Duration::from_millis(i * 33);
            let (decision, hu) = presence_decision(script(now), now, hold_until);
            hold_until = hu;
            (admitted, present_since) = admit_step(admitted, present_since, decision, now);
            if now < Duration::from_millis(700) {
                assert!(!admitted, "must not admit before 700 ms (t={now:?})");
            }
        }
        assert!(admitted, "carried dwell clock admits at ~700 ms total");
    }

    /// Absent clears the dwell clock: a walker who leaves and a NEW person
    /// arriving later must each dwell from scratch.
    #[test]
    fn absent_resets_the_dwell_clock() {
        let (admitted, present_since) = admit_step(
            false,
            Some(Duration::ZERO),
            PresenceDecision::Absent,
            Duration::from_secs(1),
        );
        assert!(!admitted);
        assert!(present_since.is_none(), "visit clock must clear on absence");
        // The next visit starts its own clock.
        let (admitted, since) = admit_step(
            false,
            None,
            PresenceDecision::Present,
            Duration::from_secs(5),
        );
        assert!(!admitted, "fresh visit is not instantly admitted");
        assert_eq!(since, Some(Duration::from_secs(5)));
    }
}
