//! Per-body limb-oscillation scoring and the shared mote-pool assignment —
//! the pure decision half of the sparkle layer (see the module doc in
//! [`super`] for the full picture; [`super::update_radiance_sparkles`] is
//! the driver that consumes these each frame).
//!
//! [`LimbOscillator`] scores the four extremity candidates by how fast they
//! *oscillate* (a per-axis Schmitt trigger on smoothed landmark velocity
//! counts direction flips into a decaying flips-per-second score) and picks
//! each body's winning limb with switch hysteresis. [`assign_motes`] then
//! apportions the shared mote budget across the present bodies
//! (winner + contralateral partner per body, [`per_body_quota`] each).
//! Everything here is pure over `Copy` inputs — no ECS, no allocation —
//! which is what keeps the whole selection behavior unit-testable without an
//! app.

use bevy::prelude::*;
use wc_core::input::body::landmark_index::{
    LEFT_ANKLE, LEFT_HIP, LEFT_WRIST, RIGHT_ANKLE, RIGHT_HIP, RIGHT_WRIST,
};
use wc_core::input::body::{TrackedBody, MAX_TRACKED_BODIES};

use super::{MAX_MOTES_PER_BODY, MAX_SPARKLES};

/// The extremity candidates, ordered so [`PARTNER`] is a same-array index
/// map: the appendages farthest from the centre of mass, each with a
/// contralateral partner (which is why the nose is excluded — it has no
/// mirror limb to anchor the reflected motes to).
pub const CANDIDATE_LANDMARKS: [usize; 4] = [LEFT_WRIST, RIGHT_WRIST, LEFT_ANKLE, RIGHT_ANKLE];
/// Contralateral partner of each entry in [`CANDIDATE_LANDMARKS`]
/// (candidate-index → candidate-index).
pub const PARTNER: [usize; 4] = [1, 0, 3, 2];

/// Schmitt-trigger hysteresis on landmark velocity, mask-UV/s: a direction
/// flip only counts when the velocity actually crosses ±this, so One-Euro
/// residual jitter around zero never reads as oscillation. (Active limbs
/// sweep ~0.1..1.0 UV/s; see `IMPULSE_FULL_SPEED`'s world-px equivalent.)
pub const FLIP_HYSTERESIS_UV_S: f32 = 0.25;
/// Decay time constant of the flips-per-second score, seconds. Long enough
/// to hold priority through a beat, short enough to hand off within ~a bar.
pub const SCORE_TAU_S: f32 = 1.2;
/// A challenger must beat the incumbent by this ratio (plus the floor) to
/// steal the motes — priority-switch hysteresis.
pub const SWITCH_RATIO: f32 = 1.3;
/// Absolute score floor a challenger must clear (flips/s) so a still body
/// never hands priority to noise.
pub const SWITCH_FLOOR: f32 = 0.2;
/// Minimum mask-UV distance from the centre of mass (mid-hip) for a limb to
/// sparkle: a wrist resting on the hip is not "far from the centre of mass".
pub const MIN_COM_DIST_UV: f32 = 0.12;
/// Landmark visibility gate (matches the limb-impulse gate).
pub const VISIBILITY_GATE: f32 = 0.5;

/// Per-body Schmitt oscillation scorer + hysteretic winner selection (one per
/// tracked slot).
#[derive(Clone, Copy, Debug, Default)]
pub struct LimbOscillator {
    /// Schmitt sign state per candidate, x axis (`-1`, `0` = unarmed, `1`).
    sign_x: [i8; 4],
    /// Schmitt sign state per candidate, y axis.
    sign_y: [i8; 4],
    /// Decaying flips-per-second oscillation score per candidate.
    score: [f32; 4],
    /// Currently prioritized candidate index (into [`CANDIDATE_LANDMARKS`]).
    current: Option<usize>,
}

impl LimbOscillator {
    /// The current flips-per-second score of a candidate (also the driver's
    /// mote-activity input).
    #[must_use]
    pub fn score(&self, candidate: usize) -> f32 {
        self.score[candidate]
    }

    /// The currently prioritized candidate index (test/diagnostic).
    #[must_use]
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// Advance the oscillation scores by one frame: decay every score toward
    /// zero, then count hysteretic velocity-direction flips on both axes
    /// into the flipping candidate's score. A limb oscillating at `f` Hz on
    /// one axis converges to a score of `2f` (two flips per cycle); the
    /// ranking only needs relative order.
    pub fn step_scores(&mut self, body: &TrackedBody, dt: f32) {
        let decay = (-dt / SCORE_TAU_S).exp();
        for (i, &landmark) in CANDIDATE_LANDMARKS.iter().enumerate() {
            self.score[i] *= decay;
            let v = body.velocities[landmark];
            let nx = schmitt_step(self.sign_x[i], v.x);
            let ny = schmitt_step(self.sign_y[i], v.y);
            let mut flips = 0.0_f32;
            if self.sign_x[i] != 0 && nx != self.sign_x[i] {
                flips += 1.0;
            }
            if self.sign_y[i] != 0 && ny != self.sign_y[i] {
                flips += 1.0;
            }
            self.sign_x[i] = nx;
            self.sign_y[i] = ny;
            // Impulse-train EMA: converges to the flip rate in flips/s.
            self.score[i] += flips / SCORE_TAU_S;
        }
    }

    /// Re-select the prioritized candidate with switch hysteresis: the
    /// incumbent keeps the motes unless it becomes ineligible or a
    /// challenger beats it by [`SWITCH_RATIO`] (plus [`SWITCH_FLOOR`]).
    pub fn select(&mut self, body: &TrackedBody) {
        let com = body_com_uv(body);
        let incumbent = self.current.filter(|&c| candidate_eligible(body, c, com));
        let mut best: Option<usize> = None;
        for i in 0..CANDIDATE_LANDMARKS.len() {
            if !candidate_eligible(body, i, com) {
                continue;
            }
            if best.is_none_or(|b| self.score[i] > self.score[b]) {
                best = Some(i);
            }
        }
        self.current = match (incumbent, best) {
            (None, b) => b.filter(|&b| self.score[b] > SWITCH_FLOOR),
            (Some(inc), Some(b)) if b != inc => {
                if self.score[b] > self.score[inc] * SWITCH_RATIO + SWITCH_FLOOR {
                    Some(b)
                } else {
                    Some(inc)
                }
            }
            (Some(inc), _) => Some(inc),
        };
    }

    /// Reset to unarmed (called when the body's slot empties so a returning
    /// dancer starts from a clean tracker).
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// A mote's assignment: `(body slot, candidate index)`.
pub type MoteTarget = Option<(usize, usize)>;

/// Motes each present body receives: an even split of the budget, capped at
/// [`MAX_MOTES_PER_BODY`] so a solo dancer gets a rich-but-not-blinding
/// constellation. `budget` is the `sparkle_count` setting clamped to
/// [`MAX_SPARKLES`].
#[must_use]
pub fn per_body_quota(present_bodies: usize, budget: usize) -> usize {
    if present_bodies == 0 {
        return 0;
    }
    (budget.min(MAX_SPARKLES) / present_bodies).min(MAX_MOTES_PER_BODY)
}

/// Build the desired mote → (slot, candidate) assignment for this frame.
/// Deterministic and stable: bodies fill the pool in slot order, each body's
/// winner extremity takes the larger half of its quota, the partner the
/// rest (or the winner takes all when the partner is ineligible). Stability
/// plus the winner-selection hysteresis means the assignment only shifts
/// when bodies actually come/go or hand off priority — and the per-mote
/// envelope cross-fades even those shifts.
#[must_use]
pub fn assign_motes(
    winners: [Option<usize>; MAX_TRACKED_BODIES],
    partner_eligible: [bool; MAX_TRACKED_BODIES],
    budget: usize,
) -> [MoteTarget; MAX_SPARKLES] {
    let mut out = [None; MAX_SPARKLES];
    let present = winners.iter().filter(|w| w.is_some()).count();
    let quota = per_body_quota(present, budget);
    if quota == 0 {
        return out;
    }
    let mut next = 0usize;
    for (slot, winner) in winners.iter().enumerate() {
        let Some(winner) = *winner else {
            continue;
        };
        let winner_share = if partner_eligible[slot] {
            quota.div_ceil(2)
        } else {
            quota
        };
        for i in 0..quota {
            if next >= MAX_SPARKLES {
                return out;
            }
            let candidate = if i < winner_share {
                winner
            } else {
                PARTNER[winner]
            };
            out[next] = Some((slot, candidate));
            next += 1;
        }
    }
    out
}

/// The centre of mass in mask UV: the mean of the visible hips, or `None`
/// when both are occluded (the distance gate then passes — with no COM
/// reference we cannot judge "far from it").
#[must_use]
pub fn body_com_uv(body: &TrackedBody) -> Option<Vec2> {
    let left = body.landmarks[LEFT_HIP];
    let right = body.landmarks[RIGHT_HIP];
    match (
        left.visibility >= VISIBILITY_GATE,
        right.visibility >= VISIBILITY_GATE,
    ) {
        (true, true) => Some((left.pos.truncate() + right.pos.truncate()) / 2.0),
        (true, false) => Some(left.pos.truncate()),
        (false, true) => Some(right.pos.truncate()),
        (false, false) => None,
    }
}

/// Whether a candidate may carry motes this frame: visible, and far enough
/// from the centre of mass.
#[must_use]
pub fn candidate_eligible(body: &TrackedBody, candidate: usize, com: Option<Vec2>) -> bool {
    let landmark = body.landmarks[CANDIDATE_LANDMARKS[candidate]];
    if landmark.visibility < VISIBILITY_GATE {
        return false;
    }
    com.is_none_or(|c| landmark.pos.truncate().distance(c) >= MIN_COM_DIST_UV)
}

/// One Schmitt-trigger step: the sign only changes when `v` crosses the
/// hysteresis band, and `0` (unarmed, the initial state) arms without
/// counting a flip.
fn schmitt_step(prev: i8, v: f32) -> i8 {
    if v > FLIP_HYSTERESIS_UV_S {
        1
    } else if v < -FLIP_HYSTERESIS_UV_S {
        -1
    } else {
        prev
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "tests derive small positive step counts / times from literals, exact in f32"
)]
mod tests {
    use super::*;
    use wc_core::input::body::{BodyLandmark, BODY_LANDMARK_COUNT};

    /// A visible present body (fully faded in) with all landmarks at
    /// UV (0.5, 0.5) and no motion.
    fn fixture_body() -> TrackedBody {
        let mut landmarks = [BodyLandmark::default(); BODY_LANDMARK_COUNT];
        for lm in &mut landmarks {
            lm.visibility = 1.0;
            lm.pos = Vec3::new(0.5, 0.5, 0.0);
        }
        // Spread the extremities away from the mid-hip COM so the distance
        // gate passes.
        landmarks[LEFT_WRIST].pos = Vec3::new(0.2, 0.3, 0.0);
        landmarks[RIGHT_WRIST].pos = Vec3::new(0.8, 0.3, 0.0);
        landmarks[LEFT_ANKLE].pos = Vec3::new(0.4, 0.9, 0.0);
        landmarks[RIGHT_ANKLE].pos = Vec3::new(0.6, 0.9, 0.0);
        TrackedBody {
            slot: 0,
            present: true,
            fade: 1.0,
            confidence: 0.9,
            landmarks,
            timestamp: std::time::Duration::from_millis(33),
            crop_fraction: 1.0,
            size: 0.2,
            ..TrackedBody::default()
        }
    }

    /// Drive `body`'s landmark `idx` with a square-wave x velocity at `hz`
    /// for `seconds`, stepping the tracker at 60 fps.
    fn oscillate(
        state: &mut LimbOscillator,
        body: &mut TrackedBody,
        idx: usize,
        hz: f32,
        seconds: f32,
    ) {
        let dt = 1.0 / 60.0;
        let steps = (seconds * 60.0) as usize;
        for step in 0..steps {
            let t = step as f32 * dt;
            let sign = if (t * hz).fract() < 0.5 { 1.0 } else { -1.0 };
            body.velocities[idx] = Vec3::new(sign * 0.6, 0.0, 0.0);
            state.step_scores(body, dt);
            body.velocities[idx] = Vec3::ZERO;
        }
    }

    /// A fast-waving wrist out-scores a slowly-waving ankle, and selection
    /// picks it.
    #[test]
    fn fastest_oscillator_wins_priority() {
        let mut state = LimbOscillator::default();
        let mut body = fixture_body();
        let dt = 1.0 / 60.0;
        let steps = (3.0 * 60.0) as usize;
        // Drive both limbs simultaneously: left wrist at 3 Hz, left ankle at 1 Hz.
        for step in 0..steps {
            let t = step as f32 * dt;
            let wrist_sign = if (t * 3.0).fract() < 0.5 { 1.0 } else { -1.0 };
            let ankle_sign = if (t * 1.0).fract() < 0.5 { 1.0 } else { -1.0 };
            body.velocities[LEFT_WRIST] = Vec3::new(wrist_sign * 0.6, 0.0, 0.0);
            body.velocities[LEFT_ANKLE] = Vec3::new(ankle_sign * 0.6, 0.0, 0.0);
            state.step_scores(&body, dt);
        }
        assert!(
            state.score(0) > state.score(2) * 2.0,
            "3 Hz wrist ({}) must far out-score 1 Hz ankle ({})",
            state.score(0),
            state.score(2)
        );
        state.select(&body);
        assert_eq!(state.current(), Some(0), "left wrist takes priority");
    }

    /// Sub-hysteresis jitter around zero never accumulates score.
    #[test]
    fn jitter_below_hysteresis_scores_nothing() {
        let mut state = LimbOscillator::default();
        let mut body = fixture_body();
        let dt = 1.0 / 60.0;
        for step in 0..600 {
            let sign = if step % 2 == 0 { 1.0 } else { -1.0 };
            body.velocities[LEFT_WRIST] = Vec3::new(sign * 0.1, sign * -0.1, 0.0);
            state.step_scores(&body, dt);
        }
        assert!(
            state.score(0).abs() < f32::EPSILON,
            "jitter must not read as oscillation: {}",
            state.score(0)
        );
    }

    /// The incumbent keeps priority against a marginally-better challenger
    /// (switch hysteresis), but loses to a decisively better one.
    #[test]
    fn priority_switch_is_hysteretic() {
        let mut state = LimbOscillator::default();
        let mut body = fixture_body();
        oscillate(&mut state, &mut body, LEFT_WRIST, 2.0, 2.0);
        state.select(&body);
        assert_eq!(state.current(), Some(0));
        // Nudge the right wrist just above the left's decayed score: within
        // the ratio band, the incumbent holds.
        state.score[1] = state.score[0] * 1.1;
        state.select(&body);
        assert_eq!(
            state.current(),
            Some(0),
            "marginal challenger must not steal"
        );
        state.score[1] = state.score[0] * SWITCH_RATIO + SWITCH_FLOOR + 0.1;
        state.select(&body);
        assert_eq!(state.current(), Some(1), "decisive challenger takes over");
    }

    /// An occluded incumbent hands off; with every candidate occluded the
    /// motes go away entirely.
    #[test]
    fn occlusion_releases_priority() {
        let mut state = LimbOscillator::default();
        let mut body = fixture_body();
        oscillate(&mut state, &mut body, LEFT_WRIST, 2.0, 2.0);
        state.select(&body);
        assert_eq!(state.current(), Some(0));
        body.landmarks[LEFT_WRIST].visibility = 0.0;
        state.select(&body);
        assert_ne!(
            state.current(),
            Some(0),
            "occluded limb cannot hold priority"
        );
        for &lm in &CANDIDATE_LANDMARKS {
            body.landmarks[lm].visibility = 0.0;
        }
        state.select(&body);
        assert_eq!(state.current(), None, "no visible extremity, no motes");
    }

    /// A wrist resting on the hip (inside the COM ring) is ineligible.
    #[test]
    fn extremity_near_com_is_ineligible() {
        let body = {
            let mut b = fixture_body();
            b.landmarks[LEFT_WRIST].pos = b.landmarks[LEFT_HIP].pos;
            b
        };
        let com = body_com_uv(&body);
        assert!(com.is_some(), "hips visible -> COM exists");
        assert!(
            !candidate_eligible(&body, 0, com),
            "wrist on the hip is not far from the centre of mass"
        );
        assert!(candidate_eligible(&body, 1, com), "raised wrist is");
    }

    /// The partner map is a left↔right involution over the candidate set.
    #[test]
    fn partner_map_is_contralateral_involution() {
        for (i, &p) in PARTNER.iter().enumerate() {
            assert_eq!(PARTNER[p], i, "partner of partner is self");
            assert_ne!(p, i);
        }
        assert_eq!(CANDIDATE_LANDMARKS[PARTNER[0]], RIGHT_WRIST);
        assert_eq!(CANDIDATE_LANDMARKS[PARTNER[2]], RIGHT_ANKLE);
    }

    /// The shared budget splits evenly, capped per body: a solo dancer gets
    /// the full constellation, a crowd spreads thinner, nobody exceeds 12.
    #[test]
    fn quota_shares_the_budget() {
        assert_eq!(per_body_quota(0, 10), 0);
        assert_eq!(
            per_body_quota(1, 10),
            MAX_MOTES_PER_BODY,
            "solo capped at 6"
        );
        assert_eq!(per_body_quota(2, 10), 5, "duo: 5 each = 10 total");
        assert_eq!(per_body_quota(3, 12), 4, "trio: 4 each = 12 total");
        assert_eq!(per_body_quota(4, 12), 3, "quad: 3 each = 12 total");
        assert_eq!(per_body_quota(2, 4), 2, "small budget honored");
    }

    /// Assignment: winner gets the larger half, partner the rest; an
    /// ineligible partner hands its share to the winner; two bodies fill in
    /// slot order and the pool never overflows.
    #[test]
    fn assignment_splits_winner_and_partner() {
        let mut winners = [None; MAX_TRACKED_BODIES];
        winners[0] = Some(0); // left wrist
        let mut partner_ok = [false; MAX_TRACKED_BODIES];
        partner_ok[0] = true;
        let assigned = assign_motes(winners, partner_ok, 10);
        let winner_motes = assigned.iter().filter(|m| **m == Some((0, 0))).count();
        let partner_motes = assigned.iter().filter(|m| **m == Some((0, 1))).count();
        assert_eq!(winner_motes, 3, "winner takes ceil(6/2)");
        assert_eq!(partner_motes, 3, "partner takes the rest");
        // Partner ineligible: the winner takes the whole quota.
        let assigned = assign_motes(winners, [false; MAX_TRACKED_BODIES], 10);
        assert_eq!(
            assigned.iter().filter(|m| **m == Some((0, 0))).count(),
            MAX_MOTES_PER_BODY
        );
        // Duo: both bodies represented, in slot order, within the pool.
        winners[2] = Some(3);
        let mut duo_ok = [false; MAX_TRACKED_BODIES];
        duo_ok[0] = true;
        duo_ok[2] = true;
        let assigned = assign_motes(winners, duo_ok, 12);
        let body0 = assigned.iter().flatten().filter(|(s, _)| *s == 0).count();
        let body2 = assigned.iter().flatten().filter(|(s, _)| *s == 2).count();
        assert_eq!(body0, 6, "duo split");
        assert_eq!(body2, 6, "duo split");
        assert!(assigned.iter().flatten().count() <= MAX_SPARKLES);
    }

    /// No winners → an empty assignment (all motes release).
    #[test]
    fn assignment_empty_without_bodies() {
        let assigned = assign_motes([None; MAX_TRACKED_BODIES], [false; MAX_TRACKED_BODIES], 12);
        assert!(assigned.iter().all(Option::is_none));
    }
}
