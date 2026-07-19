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
//!   body. Score = [`primary_score`] (`size × crop_weight`), so the closest
//!   person wins **unless** they are substantially cropped off the frame edge
//!   ([`crop_weight`] strongly penalizes low [`visible_fraction`] — the fix
//!   for "camera stayed locked on someone cropped off screen"). Switches are
//!   hysteretic: a challenger must beat the incumbent by
//!   [`PRIMARY_SWITCH_RATIO`] for [`PRIMARY_SWITCH_HOLD`] (no flapping). The
//!   `KeyN` debug hotkey feeds [`PrimarySelect::cycle`], which manually pins a
//!   slot until that person leaves.
//!
//! Everything here is allocation-free (fixed arrays sized by
//! [`MAX_TRACKED_BODIES`]) so both the worker and the publisher can call it
//! per frame.

use std::time::Duration;

use bevy::math::Vec2;

use super::detector::Rect;
use super::MAX_TRACKED_BODIES;

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

/// Priority score for one body: normalized bbox area (`size`, the
/// closest-person proxy — nearer people subtend more of the frame) times the
/// crop penalty. The largest well-framed person wins primary.
#[must_use]
pub fn primary_score(size: f32, crop_fraction: f32) -> f32 {
    size.max(0.0) * crop_weight(crop_fraction)
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
    /// half off the frame loses primary-score to a smaller, fully-framed one.
    #[test]
    fn crop_penalty_demotes_cropped_person() {
        let big_cropped = primary_score(0.30, 0.25);
        let small_framed = primary_score(0.12, 1.0);
        assert!(
            small_framed > big_cropped,
            "framed {small_framed} must beat cropped {big_cropped}"
        );
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
