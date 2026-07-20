//! Multi-person slot association and primary-body selection (pure, tested).
//!
//! Two independent pure surfaces:
//!
//! - **Association** ([`assign_slots`]): match a detector pass's person
//!   candidates to the existing tracked-body slots by centroid distance
//!   (greedy nearest — exact enough at n ≤ 4), letting unmatched candidates
//!   claim free slots. A person keeps their slot for their whole visit; the
//!   worker calls this on every detector pass. Matching against a *reserved*
//!   (recently lost) slot is what re-acquires the same person after a brief
//!   occlusion — the multi-body successor of the old single-track
//!   "stickiness".
//! - **Primary selection** ([`PrimarySelect`]): which slot is the featured
//!   body. Score = [`primary_score`] (`size × crop_weight × motion_weight`),
//!   so the closest person wins **unless** they are substantially cropped off
//!   the frame edge ([`crop_weight`] strongly penalizes low
//!   [`visible_fraction`] — the fix for "camera stayed locked on someone
//!   cropped off screen") or standing still while someone else at similar
//!   size is dancing ([`motion_weight`] — the busy-road bias toward people
//!   who are actually interacting; a lone still person keeps at least
//!   [`MOTION_FLOOR`] weight so they still win over nobody). Switches are
//!   hysteretic: a challenger must beat the incumbent by
//!   [`PRIMARY_SWITCH_RATIO`] for [`PRIMARY_SWITCH_HOLD`] (no flapping). The
//!   `KeyN` debug hotkey feeds [`PrimarySelect::cycle`], which manually pins a
//!   slot until that person leaves.
//! - **Motion measure** ([`body_motion_measure`] + [`motion_ema_step`]): the
//!   per-body "how much are they moving" scalar shared by primary selection
//!   and the Radiance emission-budget subdue. Mean speed of a stable landmark
//!   subset (nose + hips — the torso, not the flailing extremities),
//!   normalized by `sqrt(size)` so a far-away walker's small on-screen speed
//!   and a close dancer's large one compare fairly, then smoothed by a ~1.5 s
//!   EMA (held per slot in the publisher) so a momentary pause mid-dance does
//!   not read as "standing still".
//!
//! Everything here is allocation-free (fixed arrays sized by
//! [`MAX_TRACKED_BODIES`]) so both the worker and the publisher can call it
//! per frame.

use std::time::Duration;

use bevy::math::{Vec2, Vec3};

use super::detector::Rect;
use super::landmark_index::{LEFT_HIP, NOSE, RIGHT_HIP};
use super::{BODY_LANDMARK_COUNT, MAX_TRACKED_BODIES};

/// Max centre distance (square-norm units) for a detection to associate with
/// an existing slot. Beyond this the candidate is too far to plausibly be the
/// same person. (Carried over from the old single-track stickiness distance.)
pub const ASSOC_MAX_DIST: f32 = 0.25;

/// [`crop_weight`] smoothstep floor: at or below this visible fraction a body
/// is effectively fully penalized (score ≈ 0) — someone this cropped should
/// never hold primary.
pub const CROP_WEIGHT_FLOOR: f32 = 0.15;

/// [`crop_weight`] smoothstep ceiling: at or above this visible fraction a
/// body carries full weight. Between floor and ceiling the penalty eases in.
pub const CROP_WEIGHT_FULL: f32 = 0.75;

/// [`motion_weight`] floor for primary selection: the weight a completely
/// still body keeps. Deliberately well above zero — a lone still person must
/// still score (and win primary over nobody), and the crowded-venue goal is
/// only a *bias* toward movers, not a hard gate. At 0.55, a mover at full
/// motion out-scores an equally-sized still person by 1/0.55 ≈ 1.8× — past
/// the 1.3 [`PRIMARY_SWITCH_RATIO`], so a dancer can take primary from a
/// similarly-sized bystander, while a clearly-closer still person (≳ 2.4×
/// the size) still holds it.
pub const MOTION_FLOOR: f32 = 0.55;

/// [`body_motion_measure`] value (sqrt-size-normalized screen units/s) at or
/// below which a body reads as fully still ([`motion_weight`] = floor).
/// Sized above landmark-jitter noise on a stationary subject. **Venue-tune
/// candidate:** verified against synthetic fixtures only; check against live
/// bodies on the deployment camera (see docs/runbooks/kiosk.md).
pub const MOTION_SPEED_LO: f32 = 0.2;

/// [`body_motion_measure`] value at or above which a body reads as fully in
/// motion ([`motion_weight`] = 1). Roughly a torso sweeping its own height in
/// a second. Venue-tune candidate, same caveat as [`MOTION_SPEED_LO`].
pub const MOTION_SPEED_HI: f32 = 1.0;

/// Time constant (seconds) of the per-slot motion EMA ([`motion_ema_step`]).
/// ~1.5 s: long enough that a dancer pausing for a beat keeps their motion
/// standing, short enough that someone who genuinely stops decays to the
/// floor within a few seconds.
pub const MOTION_EMA_TAU: f32 = 1.5;

/// A challenger's [`primary_score`] must exceed the incumbent's by this ratio
/// to start (and sustain) a takeover.
pub const PRIMARY_SWITCH_RATIO: f32 = 1.3;

/// How long a challenger must sustain the ratio before actually taking
/// primary. Together with [`PRIMARY_SWITCH_RATIO`] this is the anti-flapping
/// hysteresis: a briefly-larger person does not steal the title.
pub const PRIMARY_SWITCH_HOLD: Duration = Duration::from_secs(1);

/// Fraction of `rect`'s area that lies inside `bounds` (`1.0` = fully
/// visible, `≈ 0.5` = half off-edge, `0.0` = fully outside or degenerate).
/// This is the `crop_fraction` published on each `TrackedBody`, computed from
/// the person's bbox clamped to the camera frame bounds.
#[must_use]
pub fn visible_fraction(rect: Rect, bounds: Rect) -> f32 {
    let area = (rect.xmax - rect.xmin).max(0.0) * (rect.ymax - rect.ymin).max(0.0);
    if area <= 0.0 {
        return 0.0;
    }
    let ix = (rect.xmax.min(bounds.xmax) - rect.xmin.max(bounds.xmin)).max(0.0);
    let iy = (rect.ymax.min(bounds.ymax) - rect.ymin.max(bounds.ymin)).max(0.0);
    (ix * iy / area).clamp(0.0, 1.0)
}

/// Crop penalty weight over a body's visible fraction: a smoothstep from
/// [`CROP_WEIGHT_FLOOR`] (≈ fully penalized) to [`CROP_WEIGHT_FULL`] (full
/// weight). Multiplied into [`primary_score`] so an edge-cropped body cannot
/// hold primary however large its bbox reads.
#[must_use]
pub fn crop_weight(crop_fraction: f32) -> f32 {
    let t = ((crop_fraction - CROP_WEIGHT_FLOOR) / (CROP_WEIGHT_FULL - CROP_WEIGHT_FLOOR))
        .clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Instantaneous body-motion measure: mean planar speed of the stable
/// landmark subset (nose + both hips — the torso reference points, chosen
/// over wrists/ankles so an idle arm swing or landmark jitter on an
/// extremity does not read as whole-body motion), normalized by
/// `sqrt(size)`. Velocities are screen-normalized units/s, so a distant
/// walker moves few screen units even at a brisk pace; dividing by the bbox
/// *side* (≈ `sqrt(area)`) converts to body-heights-per-second-ish units and
/// removes the distance bias. Raw per-frame — feed it through
/// [`motion_ema_step`] before scoring.
#[must_use]
pub fn body_motion_measure(velocities: &[Vec3; BODY_LANDMARK_COUNT], size: f32) -> f32 {
    let speed = (velocities[NOSE].truncate().length()
        + velocities[LEFT_HIP].truncate().length()
        + velocities[RIGHT_HIP].truncate().length())
        / 3.0;
    // Floor the size so a degenerate bbox cannot blow the measure up.
    speed / size.max(1.0e-4).sqrt()
}

/// One EMA step of the per-slot motion envelope: `ema` eased toward `sample`
/// with time constant [`MOTION_EMA_TAU`] (frame-rate-independent
/// `1 − exp(−dt/τ)`). The EMA state lives on existing per-slot publisher
/// state — no allocation on the poll path.
#[must_use]
pub fn motion_ema_step(ema: f32, sample: f32, dt: f32) -> f32 {
    let k = 1.0 - (-dt.max(0.0) / MOTION_EMA_TAU).exp();
    ema + (sample.max(0.0) - ema) * k
}

/// Map a (smoothed) motion measure to a multiplicative weight in
/// `[floor, 1]`: a smoothstep over [`MOTION_SPEED_LO`]..[`MOTION_SPEED_HI`]
/// scaled into the caller's floor. Primary selection passes
/// [`MOTION_FLOOR`]; Radiance's background-subdue passes its own (higher)
/// floor — one mapping, two consumers, so the two biases cannot drift in
/// shape.
#[must_use]
pub fn motion_weight(motion: f32, floor: f32) -> f32 {
    let t = ((motion - MOTION_SPEED_LO) / (MOTION_SPEED_HI - MOTION_SPEED_LO)).clamp(0.0, 1.0);
    let s = t * t * (3.0 - 2.0 * t);
    let floor = floor.clamp(0.0, 1.0);
    floor + (1.0 - floor) * s
}

/// Priority score for one body: normalized bbox area (`size`, the
/// closest-person proxy — nearer people subtend more of the frame) times the
/// crop penalty times the motion weight (`motion` is the slot's smoothed
/// [`body_motion_measure`]). The largest well-framed *moving* person wins
/// primary; stillness only discounts to [`MOTION_FLOOR`], never to zero.
#[must_use]
pub fn primary_score(size: f32, crop_fraction: f32, motion: f32) -> f32 {
    size.max(0.0) * crop_weight(crop_fraction) * motion_weight(motion, MOTION_FLOOR)
}

/// Match detector candidates to slots. Greedy globally-nearest matching:
///
/// 1. Repeatedly take the (candidate, occupied-slot) pair with the smallest
///    centre distance under [`ASSOC_MAX_DIST`] and bind it (each candidate
///    and each slot at most once). `anchors[i]` is `Some(last centre)` for
///    every occupied slot — active *and* reserved — so a returning person
///    re-binds to their old slot (and colour).
/// 2. Unmatched candidates then claim `claimable` slots (free, never
///    reserved) in ascending slot order.
///
/// Returns, per candidate index, `Some(slot)` or `None` (no slot available).
/// Fixed-size arrays throughout — no allocation (worker hot path).
#[must_use]
pub fn assign_slots(
    candidates: &[Vec2],
    anchors: &[Option<Vec2>; MAX_TRACKED_BODIES],
    claimable: &[bool; MAX_TRACKED_BODIES],
) -> [Option<usize>; MAX_TRACKED_BODIES] {
    let n = candidates.len().min(MAX_TRACKED_BODIES);
    let mut out: [Option<usize>; MAX_TRACKED_BODIES] = [None; MAX_TRACKED_BODIES];
    let mut slot_taken = [false; MAX_TRACKED_BODIES];

    // Pass 1: greedy globally-nearest candidate↔occupied-slot binding.
    let max_d2 = ASSOC_MAX_DIST * ASSOC_MAX_DIST;
    loop {
        let mut best: Option<(usize, usize, f32)> = None; // (candidate, slot, d²)
        for (c, centre) in candidates.iter().enumerate().take(n) {
            if out[c].is_some() {
                continue;
            }
            for (s, anchor) in anchors.iter().enumerate() {
                let Some(anchor) = anchor else { continue };
                if slot_taken[s] {
                    continue;
                }
                let d2 = centre.distance_squared(*anchor);
                if d2 <= max_d2 && best.is_none_or(|(_, _, b)| d2 < b) {
                    best = Some((c, s, d2));
                }
            }
        }
        let Some((c, s, _)) = best else { break };
        out[c] = Some(s);
        slot_taken[s] = true;
    }

    // Pass 2: unmatched candidates claim free slots in ascending order.
    for assigned in out.iter_mut().take(n) {
        if assigned.is_some() {
            continue;
        }
        if let Some(s) = (0..MAX_TRACKED_BODIES)
            .find(|&s| claimable[s] && !slot_taken[s] && anchors[s].is_none())
        {
            *assigned = Some(s);
            slot_taken[s] = true;
        }
    }
    out
}

/// Primary-slot selection state: the current primary, the pending challenger
/// (for the switch hysteresis), and the manual-pin flag driven by the `KeyN`
/// cycle. Lives in the publisher's runtime; pure `update`/`cycle` methods so
/// the policy is unit-tested without a worker.
#[derive(Debug, Default, Clone, Copy)]
pub struct PrimarySelect {
    /// The featured slot, if any body is present.
    current: Option<usize>,
    /// Manual pin: set by [`Self::cycle`], cleared when the pinned slot's
    /// body goes absent. While set, automatic score-based switching is
    /// suppressed (the operator picked a dancer on purpose).
    manual: bool,
    /// The slot currently out-scoring the incumbent past the ratio, if any.
    challenger: Option<usize>,
    /// When `challenger` first crossed the ratio (takeover at
    /// `challenger_since + PRIMARY_SWITCH_HOLD`).
    challenger_since: Duration,
}

impl PrimarySelect {
    /// The current primary slot.
    #[must_use]
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// Re-evaluate the primary. `scores[i]` is `Some(primary_score)` for every
    /// slot with a present body, `None` otherwise.
    ///
    /// - No incumbent (or incumbent absent): the best-scoring present slot
    ///   takes primary immediately; any manual pin dissolves (the pinned
    ///   person left).
    /// - Manual pin active: the incumbent keeps primary unconditionally.
    /// - Otherwise: a challenger whose score exceeds the incumbent's by
    ///   [`PRIMARY_SWITCH_RATIO`] must sustain it for
    ///   [`PRIMARY_SWITCH_HOLD`] before taking over.
    pub fn update(&mut self, scores: &[Option<f32>; MAX_TRACKED_BODIES], now: Duration) {
        let best = best_slot(scores);
        let incumbent = self.current.filter(|&c| scores[c].is_some());
        let Some(cur) = incumbent else {
            // No (present) incumbent: promote the best immediately.
            self.current = best;
            self.manual = false;
            self.challenger = None;
            return;
        };
        if self.manual {
            self.challenger = None;
            return;
        }
        let cur_score = scores[cur].unwrap_or(0.0);
        match best {
            Some(b) if b != cur && scores[b].unwrap_or(0.0) > cur_score * PRIMARY_SWITCH_RATIO => {
                if self.challenger == Some(b) {
                    if now.saturating_sub(self.challenger_since) >= PRIMARY_SWITCH_HOLD {
                        self.current = Some(b);
                        self.challenger = None;
                    }
                } else {
                    self.challenger = Some(b);
                    self.challenger_since = now;
                }
            }
            _ => self.challenger = None,
        }
    }

    /// Manually cycle primary to the next present slot (ascending slot order,
    /// wrapping), pinning it against automatic switching until that person
    /// leaves. The `KeyN` debug hotkey lands here. No-op when nobody is present.
    pub fn cycle(&mut self, present: &[bool; MAX_TRACKED_BODIES]) {
        let start = self.current.unwrap_or(MAX_TRACKED_BODIES - 1);
        for step in 1..=MAX_TRACKED_BODIES {
            let slot = (start + step) % MAX_TRACKED_BODIES;
            if present[slot] {
                self.current = Some(slot);
                self.manual = true;
                self.challenger = None;
                return;
            }
        }
    }
}

/// The present slot with the highest score, if any.
fn best_slot(scores: &[Option<f32>; MAX_TRACKED_BODIES]) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, s) in scores.iter().enumerate() {
        let Some(s) = *s else { continue };
        if best.is_none_or(|(_, b)| s > b) {
            best = Some((i, s));
        }
    }
    best.map(|(i, _)| i)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;

    fn full_rect() -> Rect {
        Rect {
            xmin: 0.0,
            ymin: 0.0,
            xmax: 1.0,
            ymax: 1.0,
        }
    }

    fn rect(xmin: f32, ymin: f32, xmax: f32, ymax: f32) -> Rect {
        Rect {
            xmin,
            ymin,
            xmax,
            ymax,
        }
    }

    #[test]
    fn visible_fraction_full_half_and_outside() {
        let b = full_rect();
        // Fully inside.
        assert!((visible_fraction(rect(0.2, 0.2, 0.6, 0.8), b) - 1.0).abs() < 1e-6);
        // Exactly half off the left edge.
        let half = visible_fraction(rect(-0.2, 0.2, 0.2, 0.8), b);
        assert!((half - 0.5).abs() < 1e-6, "half={half}");
        // Fully outside.
        assert!(visible_fraction(rect(1.1, 0.0, 1.5, 1.0), b).abs() < 1e-6);
        // Degenerate rect.
        assert!(visible_fraction(rect(0.5, 0.5, 0.5, 0.5), b).abs() < 1e-6);
    }

    #[test]
    fn crop_weight_penalizes_edge_cropped_bodies() {
        assert!((crop_weight(1.0) - 1.0).abs() < 1e-6, "fully visible = 1");
        assert!(crop_weight(CROP_WEIGHT_FLOOR).abs() < 1e-6, "floor = 0");
        assert!(crop_weight(0.05).abs() < 1e-6, "below floor stays 0");
        let half = crop_weight(0.5);
        assert!(
            half > 0.3 && half < 0.9,
            "half-visible partially penalized: {half}"
        );
        assert!(crop_weight(0.9) > 0.99, "well-framed ≈ full weight");
    }

    /// The crop penalty demotes an edge-cropped person: a big body hanging
    /// half off the frame loses primary-score to a smaller, fully-framed one
    /// (equal motion, so only the crop term differs).
    #[test]
    fn crop_penalty_demotes_cropped_person() {
        let big_cropped = primary_score(0.30, 0.25, MOTION_SPEED_HI);
        let small_framed = primary_score(0.12, 1.0, MOTION_SPEED_HI);
        assert!(
            small_framed > big_cropped,
            "framed {small_framed} must beat cropped {big_cropped}"
        );
    }

    /// The motion weight spans exactly [`MOTION_FLOOR`, 1]: a still body keeps
    /// the floor (never zero — a lone still person must still win over
    /// nobody), a fast one carries full weight, and the ramp is monotone.
    #[test]
    fn motion_weight_floors_and_saturates() {
        assert!((motion_weight(0.0, MOTION_FLOOR) - MOTION_FLOOR).abs() < 1e-6);
        assert!((motion_weight(MOTION_SPEED_LO, MOTION_FLOOR) - MOTION_FLOOR).abs() < 1e-6);
        assert!((motion_weight(MOTION_SPEED_HI, MOTION_FLOOR) - 1.0).abs() < 1e-6);
        assert!(
            (motion_weight(10.0, MOTION_FLOOR) - 1.0).abs() < 1e-6,
            "clamps above"
        );
        let mid = motion_weight((MOTION_SPEED_LO + MOTION_SPEED_HI) * 0.5, MOTION_FLOOR);
        assert!(
            mid > MOTION_FLOOR && mid < 1.0,
            "mid-speed partially weighted: {mid}"
        );
        // Alternate floors (the Radiance subdue path) rescale the same ramp.
        assert!((motion_weight(0.0, 0.6) - 0.6).abs() < 1e-6);
        assert!((motion_weight(MOTION_SPEED_HI, 0.6) - 1.0).abs() < 1e-6);
    }

    /// The still-close vs moving-far tradeoff the floor is tuned for: a
    /// mover beats an equally-sized still person past the switch ratio, but
    /// a clearly-closer (much larger) still person still wins.
    #[test]
    fn motion_biases_but_does_not_override_size() {
        let still = primary_score(0.15, 1.0, 0.0);
        let mover = primary_score(0.15, 1.0, MOTION_SPEED_HI);
        assert!(
            mover > still * PRIMARY_SWITCH_RATIO,
            "same-size mover must clear the switch ratio: {mover} vs {still}"
        );
        // A still person at 3x the mover's size (much closer to the camera)
        // still out-scores them: proximity remains the primary signal.
        let close_still = primary_score(0.45, 1.0, 0.0);
        let far_mover = primary_score(0.15, 1.0, MOTION_SPEED_HI);
        assert!(
            close_still > far_mover,
            "clearly-closer still person keeps the lead: {close_still} vs {far_mover}"
        );
    }

    /// The motion measure normalizes by sqrt(size): a small (distant) body
    /// and a large (near) one with proportionally-scaled screen velocities
    /// read the same.
    #[test]
    fn motion_measure_is_distance_normalized() {
        let mut vel_near = [Vec3::ZERO; BODY_LANDMARK_COUNT];
        let mut vel_far = [Vec3::ZERO; BODY_LANDMARK_COUNT];
        for &lm in &[NOSE, LEFT_HIP, RIGHT_HIP] {
            vel_near[lm] = Vec3::new(0.4, 0.0, 0.0); // big on-screen sweep
            vel_far[lm] = Vec3::new(0.1, 0.0, 0.0); // same body speed, 4x area
        }
        let near = body_motion_measure(&vel_near, 0.16);
        let far = body_motion_measure(&vel_far, 0.01);
        assert!(
            (near - far).abs() < 1e-5,
            "sqrt-size normalization must cancel distance: {near} vs {far}"
        );
        // z (model depth derivative) is excluded — only planar speed counts.
        let mut vel_z = [Vec3::ZERO; BODY_LANDMARK_COUNT];
        vel_z[NOSE] = Vec3::new(0.0, 0.0, 5.0);
        assert!(body_motion_measure(&vel_z, 0.16).abs() < 1e-6);
    }

    /// The EMA is frame-rate independent and stable: a constant sample
    /// converges without overshoot, and one noisy spike barely moves it.
    #[test]
    fn motion_ema_is_stable() {
        let steps = |dt: f32, total: f32| {
            let mut ema = 0.0;
            let mut t = 0.0;
            while t < total {
                ema = motion_ema_step(ema, 1.0, dt);
                t += dt;
            }
            ema
        };
        let fine = steps(1.0 / 240.0, MOTION_EMA_TAU);
        let coarse = steps(1.0 / 30.0, MOTION_EMA_TAU);
        assert!((fine - 0.632).abs() < 0.02, "one tau ≈ 63%: {fine}");
        assert!((fine - coarse).abs() < 0.02, "rate-independent");
        // A single spiky sample at 30 Hz moves the envelope ~2%, so
        // landmark-noise spikes cannot flip the motion weight.
        let spiked = motion_ema_step(0.0, 1.0, 1.0 / 30.0);
        assert!(spiked < 0.03, "one spike must barely register: {spiked}");
        // Negative dt / samples are clamped (defensive, never NaN).
        assert!(motion_ema_step(0.5, -1.0, -0.1) <= 0.5);
    }

    #[test]
    fn assign_matches_moving_person_to_their_slot() {
        // Slot 2 last seen at (0.5, 0.5); the person moved a little. The
        // candidate must re-bind to slot 2 (slot identity is stable across
        // motion), not claim a free slot.
        let anchors = [None, None, Some(Vec2::new(0.5, 0.5)), None];
        let claimable = [true, true, false, true];
        let out = assign_slots(&[Vec2::new(0.55, 0.48)], &anchors, &claimable);
        assert_eq!(out[0], Some(2));
    }

    #[test]
    fn assign_prefers_globally_nearest_pairs() {
        // Two people, two occupied slots; each candidate must bind to its own
        // nearest slot even when listed in the "wrong" order.
        let anchors = [
            Some(Vec2::new(0.2, 0.5)),
            Some(Vec2::new(0.8, 0.5)),
            None,
            None,
        ];
        let claimable = [false, false, true, true];
        let cands = [Vec2::new(0.78, 0.52), Vec2::new(0.22, 0.49)];
        let out = assign_slots(&cands, &anchors, &claimable);
        assert_eq!(out[0], Some(1), "right-side candidate → right slot");
        assert_eq!(out[1], Some(0), "left-side candidate → left slot");
    }

    #[test]
    fn assign_far_candidate_claims_a_free_slot_not_an_occupied_one() {
        // A candidate farther than ASSOC_MAX_DIST from every anchor is a new
        // person: they claim the lowest free slot instead of stealing slot 0.
        let anchors = [Some(Vec2::new(0.1, 0.1)), None, None, None];
        let claimable = [false, true, true, true];
        let out = assign_slots(&[Vec2::new(0.9, 0.9)], &anchors, &claimable);
        assert_eq!(out[0], Some(1), "new person claims the first free slot");
    }

    #[test]
    fn assign_reserved_slots_are_matchable_but_not_claimable() {
        // Slot 0 reserved (anchored, not claimable): a nearby candidate
        // re-binds to it (occlusion return), but a far-away new person must
        // not claim it while reserved.
        let anchors = [Some(Vec2::new(0.3, 0.3)), None, None, None];
        let claimable = [false, true, true, true];
        let near = assign_slots(&[Vec2::new(0.32, 0.31)], &anchors, &claimable);
        assert_eq!(near[0], Some(0), "returning person re-binds to slot 0");
        let far = assign_slots(&[Vec2::new(0.9, 0.9)], &anchors, &claimable);
        assert_eq!(far[0], Some(1), "newcomer must not take the reserved slot");
    }

    #[test]
    fn assign_overflow_candidates_get_no_slot() {
        let anchors = [None, None, None, None];
        let claimable = [true, false, false, false];
        let cands = [Vec2::new(0.1, 0.1), Vec2::new(0.9, 0.9)];
        let out = assign_slots(&cands, &anchors, &claimable);
        assert_eq!(out[0], Some(0));
        assert_eq!(out[1], None, "no capacity → no slot");
    }

    fn scores(v: [Option<f32>; MAX_TRACKED_BODIES]) -> [Option<f32>; MAX_TRACKED_BODIES] {
        v
    }

    #[test]
    fn primary_promotes_best_immediately_when_vacant() {
        let mut sel = PrimarySelect::default();
        sel.update(&scores([Some(0.1), Some(0.3), None, None]), Duration::ZERO);
        assert_eq!(sel.current(), Some(1));
    }

    /// A briefly-larger challenger does not steal primary; a sustained one
    /// does (the ratio+hold hysteresis).
    #[test]
    fn primary_switch_is_hysteretic() {
        let mut sel = PrimarySelect::default();
        let t0 = Duration::ZERO;
        sel.update(&scores([Some(0.2), None, None, None]), t0);
        assert_eq!(sel.current(), Some(0));

        // Challenger appears at 2x the incumbent (over the 1.3 ratio) but
        // only briefly: incumbent holds.
        let t1 = Duration::from_millis(100);
        sel.update(&scores([Some(0.2), Some(0.4), None, None]), t1);
        assert_eq!(sel.current(), Some(0), "no instant takeover");
        // Challenger dips back under the ratio: hysteresis timer resets.
        let t2 = Duration::from_millis(400);
        sel.update(&scores([Some(0.2), Some(0.22), None, None]), t2);
        assert_eq!(sel.current(), Some(0));
        // Challenger returns and SUSTAINS the ratio for the hold: takeover.
        let t3 = Duration::from_millis(500);
        sel.update(&scores([Some(0.2), Some(0.4), None, None]), t3);
        assert_eq!(sel.current(), Some(0), "hold not yet elapsed");
        let t4 = t3 + PRIMARY_SWITCH_HOLD;
        sel.update(&scores([Some(0.2), Some(0.4), None, None]), t4);
        assert_eq!(sel.current(), Some(1), "sustained challenger takes over");
    }

    #[test]
    fn primary_within_ratio_never_challenges() {
        let mut sel = PrimarySelect::default();
        sel.update(&scores([Some(0.2), None, None, None]), Duration::ZERO);
        for i in 1..300_u64 {
            // 25% better forever — under the 30% ratio, never a takeover.
            sel.update(
                &scores([Some(0.2), Some(0.25), None, None]),
                Duration::from_millis(i * 33),
            );
        }
        assert_eq!(sel.current(), Some(0), "sub-ratio challenger never wins");
    }

    #[test]
    fn primary_reassigns_when_incumbent_leaves() {
        let mut sel = PrimarySelect::default();
        sel.update(&scores([Some(0.5), Some(0.1), None, None]), Duration::ZERO);
        assert_eq!(sel.current(), Some(0));
        sel.update(
            &scores([None, Some(0.1), None, None]),
            Duration::from_secs(1),
        );
        assert_eq!(sel.current(), Some(1), "vacated primary promotes the rest");
        sel.update(&scores([None, None, None, None]), Duration::from_secs(2));
        assert_eq!(sel.current(), None, "empty room → no primary");
    }

    #[test]
    fn cycle_walks_present_slots_and_pins() {
        let mut sel = PrimarySelect::default();
        sel.update(
            &scores([Some(0.5), Some(0.1), Some(0.2), None]),
            Duration::ZERO,
        );
        assert_eq!(sel.current(), Some(0));
        sel.cycle(&[true, true, true, false]);
        assert_eq!(sel.current(), Some(1), "cycles ascending");
        sel.cycle(&[true, true, true, false]);
        assert_eq!(sel.current(), Some(2));
        sel.cycle(&[true, true, true, false]);
        assert_eq!(sel.current(), Some(0), "wraps");

        // Pinned: a huge sustained challenger must NOT steal.
        sel.cycle(&[true, true, true, false]); // pin slot 1
        assert_eq!(sel.current(), Some(1));
        for i in 0..200_u64 {
            sel.update(
                &scores([Some(5.0), Some(0.1), None, None]),
                Duration::from_millis(i * 33),
            );
        }
        assert_eq!(sel.current(), Some(1), "manual pin suppresses auto-switch");

        // The pinned person leaves: the pin dissolves and auto resumes.
        sel.update(
            &scores([Some(5.0), None, None, None]),
            Duration::from_mins(1),
        );
        assert_eq!(sel.current(), Some(0), "pin dissolves on absence");
    }

    #[test]
    fn cycle_with_nobody_present_is_a_no_op() {
        let mut sel = PrimarySelect::default();
        sel.cycle(&[false, false, false, false]);
        assert_eq!(sel.current(), None);
    }
}
