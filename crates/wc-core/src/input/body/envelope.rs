//! Per-slot presence debounce + graceful fade envelope (pure, main-thread).
//!
//! Two pure decisions, one per tracked-body slot:
//!
//! - [`presence_decision`]: the presence-hold debounce. A stationary,
//!   partially-framed body sits right at the landmark model's
//!   `presence_threshold`, so its per-frame presence flag chatters; the hold
//!   bridges brief dropouts so the published `present` does not flicker.
//! - [`fade_step`]: the appearance/disappearance envelope. `fade` eases toward
//!   1.0 while a body is present/held ([`FADE_ATTACK_TAU`]) and toward 0.0
//!   once it is gone ([`FADE_RELEASE_TAU`]), using the frame-rate-independent
//!   `1 − exp(−dt/τ)` idiom with `dt` capped at [`FADE_DT_CAP`]. A slot is
//!   freed (its `TrackedBody` removed and its worker-side channel eligible for
//!   reuse) only once the fade reaches 0 — this is what lets figures appear
//!   and disappear gracefully instead of popping.
//!
//! Both are pure functions over plain values so the timing behaviour is
//! unit-tested without standing up a worker or a clock.
//!
//! Timing coupling: the worker reserves a lost slot for
//! `pipeline::SLOT_RESERVE` before letting a *new* person claim it. That
//! reservation must cover [`PRESENCE_HOLD`] plus the full release
//! ([`FADE_RELEASE_TAU`] · ln(1/[`FADE_DONE_EPSILON`]) ≈ 3.6 s) so a mask
//! channel is never handed to a newcomer while the previous occupant is still
//! fading out on screen.

use std::time::Duration;

/// How long to keep publishing a slot's last pose after the worker stops
/// reporting that person, debouncing brief detection dropouts. Long enough to
/// bridge presence-threshold thrash on a marginal body, short enough that a
/// real exit starts the fade-out promptly.
pub const PRESENCE_HOLD: Duration = Duration::from_millis(300);

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
}
