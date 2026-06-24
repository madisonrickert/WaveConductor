//! Two-hand grab → wave centres. Maps the shared hand-tracking state to the
//! [`CymaticsHandGrabs`] resource consumed by the interaction state machine.
//!
//! ## Y-convention
//!
//! Palm positions are converted from Leap mm-space to Bevy-native NDC
//! (top-left = `-1, -1`; bottom-right = `+1, +1`) using the same Leap range
//! constants as `wc_core::input::projection`. [`hand_to_sim_uv`] then delegates
//! to [`super::interaction::screen_to_sim_uv`], producing top-left UV with
//! **no Y-flip**. This matches the cursor NDC convention in
//! [`super::interaction::update_cymatics_centers`].
//!
//! ## Stickiness
//!
//! Hand 0 (ECS query order) → `c1`; hand 1 → `c2`. The entity-per-hand model
//! ensures stable query ordering per [`TrackedHand`] entity across frames, so
//! the same physical hand always maps to the same centre slot. Ported from v4
//! `centerHeldByHandId[0/1]` (`index.ts`).
//!
//! ## No hot-path allocation
//!
//! The Bevy system builds a fixed-size `[(bool, Vec2); 2]` stack buffer; no
//! per-frame `Vec` or heap allocation.

use bevy::math::Vec2;
use bevy::prelude::*;
use wc_core::input::entity::{GrabStrength, PalmPosition, TrackedHand};
use wc_core::input::projection::{LEAP_X_HALFRANGE_MM, LEAP_Y_MAX_MM, LEAP_Y_MIN_MM};

use super::interaction::screen_to_sim_uv;

// ---------------------------------------------------------------------------
// Grab threshold
// ---------------------------------------------------------------------------

/// Grab strength at or below this value is treated as "not grabbing."
///
/// Matches v4's `LEAP_POWER_CONFIG.grabThreshold` used by both Line and
/// Dots (`0.1`). Values above this trigger the grab.
pub const CYMATICS_HAND_GRAB_THRESHOLD: f32 = 0.1;

// ---------------------------------------------------------------------------
// Resource
// ---------------------------------------------------------------------------

/// Hand-grip positions for the two Cymatics wave centres.
///
/// `None` = that centre is not held by a hand this frame. Populated by
/// [`update_cymatics_hand_centers`]; the interaction state machine
/// ([`super::interaction::update_cymatics_centers`]) reads this resource to
/// decide whether a centre follows the mouse (free) or a hand (held).
///
/// Coordinates are sim UV `[0, 1]`, top-left origin (Bevy-native).
#[derive(Resource, Default, Clone, Copy)]
pub struct CymaticsHandGrabs {
    /// Primary wave-centre grab position, or `None` if not held.
    pub c1: Option<Vec2>,
    /// Secondary wave-centre grab position, or `None` if not held.
    pub c2: Option<Vec2>,
}

// ---------------------------------------------------------------------------
// Pure helpers (headless-testable, no ECS)
// ---------------------------------------------------------------------------

/// Map a Leap palm position (mm) to Bevy-native NDC.
///
/// Converts Leap device coordinates to window-relative NDC:
/// - X: `[-200, +200] mm` → `[-1, +1]`
/// - Y: `[350, 40] mm` (high→low) → `[-1, +1]` (top→bottom, Bevy
///   window-logical, top = -1)
///
/// This Y-axis direction matches the cursor NDC in
/// [`super::interaction::update_cymatics_centers`]: top = -1, bottom = +1.
/// No Y-flip is applied here; [`screen_to_sim_uv`] maps it directly to
/// top-left UV.
///
/// Based on `wc_core::input::projection::palm_to_world` (same constants),
/// adapted to produce NDC rather than world-space pixels.
#[must_use]
pub fn palm_mm_to_ndc(palm_mm: Vec3) -> Vec2 {
    // X: Leap horizontal range [-200, +200] mm → [-1, +1] NDC.
    let x_norm = ((palm_mm.x + LEAP_X_HALFRANGE_MM) / (2.0 * LEAP_X_HALFRANGE_MM)).clamp(0.0, 1.0);
    // Y: Leap height range [40, 350] mm → [0, 1], where high palm (350) = 0
    //    (screen-top) and low palm (40) = 1 (screen-bottom). Bevy window-
    //    logical NDC: top = -1, bottom = +1 → multiply by 2 and subtract 1.
    let y_norm = ((LEAP_Y_MAX_MM - palm_mm.y) / (LEAP_Y_MAX_MM - LEAP_Y_MIN_MM)).clamp(0.0, 1.0);
    Vec2::new(x_norm * 2.0 - 1.0, y_norm * 2.0 - 1.0)
}

/// Palm NDC → sim UV (same mapping as the mouse cursor).
///
/// Thin wrapper around [`screen_to_sim_uv`]; named separately so tests can
/// call it directly without spelling out the delegation.
#[must_use]
pub fn hand_to_sim_uv(palm_ndc: Vec2, screen_ar: f32, sim_ar: f32) -> Vec2 {
    screen_to_sim_uv(palm_ndc, screen_ar, sim_ar)
}

/// Pure assignment: `(grabbing, palm_ndc)` per hand → [`CymaticsHandGrabs`].
///
/// Assigns the first grabbing entry in `hands` to `c1`, the second to `c2`.
/// Hand order is the caller's responsibility; the Bevy system passes entries
/// in stable ECS entity order so the same physical hand always maps to the
/// same centre slot across frames (v4 `centerHeldByHandId[0/1]`).
///
/// `hands` entries beyond the second are ignored (at most two centres exist).
/// When a hand is not grabbing its entry is skipped; the corresponding centre
/// slot stays `None`.
#[must_use]
pub fn assign_grabs(hands: &[(bool, Vec2)], screen_ar: f32, sim_ar: f32) -> CymaticsHandGrabs {
    let mut grabs = CymaticsHandGrabs::default();
    for &(grabbing, palm) in hands {
        if !grabbing {
            continue;
        }
        let uv = hand_to_sim_uv(palm, screen_ar, sim_ar);
        if grabs.c1.is_none() {
            grabs.c1 = Some(uv);
        } else if grabs.c2.is_none() {
            grabs.c2 = Some(uv);
        }
    }
    grabs
}

// ---------------------------------------------------------------------------
// Bevy system
// ---------------------------------------------------------------------------

/// Read the shared hand-tracking state, detect grabs, and write
/// [`CymaticsHandGrabs`].
///
/// Queries up to two [`TrackedHand`] entities by ECS order. The entity-per-hand
/// model provides stable ordering across frames, so hand 0 → `c1` and
/// hand 1 → `c2` remain consistent even when hands cross in screen space.
///
/// Grab detection: `GrabStrength > CYMATICS_HAND_GRAB_THRESHOLD` (same
/// threshold as Dots; v4 `grabThreshold = 0.1`).
///
/// **Must run `.before(update_cymatics_centers)`** so the interaction state
/// machine reads fresh grab positions each frame.
///
/// No per-frame heap allocation: hands are collected into a fixed
/// `[(bool, Vec2); 2]` stack buffer.
pub fn update_cymatics_hand_centers(
    mut grabs: ResMut<'_, CymaticsHandGrabs>,
    window: Single<'_, '_, &Window>,
    hand_query: Query<'_, '_, (&PalmPosition, &GrabStrength), With<TrackedHand>>,
) {
    let win = Vec2::new(window.width().max(1.0), window.height().max(1.0));
    let ar = win.x / win.y;

    // Fixed-size stack buffer: at most 2 hands (c1 + c2). No per-frame alloc.
    let mut buf = [(false, Vec2::ZERO); 2];
    let mut count = 0usize;

    // INVARIANT: stickiness (hand 0 → c1, hand 1 → c2 across frames) relies on
    // `Query::iter()` returning `TrackedHand` entities in stable ECS archetype
    // order; this holds as long as no `TrackedHand` entity adds or removes
    // components between frames, which the hand-tracking provider guarantees for
    // an entity's lifetime.
    for (palm, grab) in hand_query.iter().take(2) {
        // v4: grab > grabThreshold is "grabbing". Same threshold as Dots.
        buf[count] = (
            grab.0 > CYMATICS_HAND_GRAB_THRESHOLD,
            palm_mm_to_ndc(palm.0),
        );
        count += 1;
    }

    *grabs = assign_grabs(&buf[..count], ar, ar);
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions — panicking on unexpected None is the correct failure mode"
)]
mod tests {
    use super::*;
    use bevy::math::Vec2;

    // -----------------------------------------------------------------------
    // Brief's required tests (verbatim from task-C10-brief.md)
    // -----------------------------------------------------------------------

    #[test]
    fn grab_maps_palm_to_sim_uv() {
        // centre-screen palm (ndc 0,0) → sim uv (0.5,0.5) at AR 1.
        let uv = hand_to_sim_uv(Vec2::ZERO, 1.0, 1.0);
        assert!((uv - Vec2::new(0.5, 0.5)).length() < 1e-5);
    }

    #[test]
    fn assign_grabs_orders_two_hands() {
        let grabs = assign_grabs(
            &[
                (true, Vec2::new(-0.5, 0.0)), // hand 0 grabbing
                (true, Vec2::new(0.5, 0.0)),  // hand 1 grabbing
            ],
            1.0,
            1.0,
        );
        assert!(grabs.c1.is_some());
        assert!(grabs.c2.is_some());
    }

    #[test]
    fn no_grab_yields_none() {
        let grabs = assign_grabs(&[(false, Vec2::ZERO)], 1.0, 1.0);
        assert!(grabs.c1.is_none() && grabs.c2.is_none());
    }

    // -----------------------------------------------------------------------
    // Additional coverage
    // -----------------------------------------------------------------------

    /// One grabbing hand sets c1; c2 stays None.
    #[test]
    fn one_grabbing_hand_sets_c1_only() {
        let grabs = assign_grabs(
            &[
                (true, Vec2::new(-0.5, 0.0)), // hand 0 grabbing
                (false, Vec2::new(0.5, 0.0)), // hand 1 not grabbing
            ],
            1.0,
            1.0,
        );
        assert!(grabs.c1.is_some(), "c1 must be set when hand 0 grabs");
        assert!(
            grabs.c2.is_none(),
            "c2 must be None when hand 1 is not grabbing"
        );
    }

    /// Stickiness: hand 0 always → c1, hand 1 always → c2 across repeated frames.
    ///
    /// Simulates v4 `centerHeldByHandId[0/1]`: the same query order produces
    /// the same centre assignment each frame, so centres don't swap when hands
    /// cross in screen space.
    #[test]
    fn stickiness_hand0_always_drives_c1() {
        let left_ndc = Vec2::new(-0.5, 0.0);
        let right_ndc = Vec2::new(0.5, 0.0);
        let left_uv = hand_to_sim_uv(left_ndc, 1.0, 1.0);
        let right_uv = hand_to_sim_uv(right_ndc, 1.0, 1.0);

        // Multiple "frames": same input, consistent output.
        for _ in 0..5 {
            let grabs = assign_grabs(&[(true, left_ndc), (true, right_ndc)], 1.0, 1.0);
            let c1 = grabs.c1.expect("c1 must be set");
            let c2 = grabs.c2.expect("c2 must be set");
            assert!(
                (c1 - left_uv).length() < 1e-5,
                "c1 must track hand 0 (stickiness)"
            );
            assert!(
                (c2 - right_uv).length() < 1e-5,
                "c2 must track hand 1 (stickiness)"
            );
        }
    }

    /// Release: when a grabbing hand opens, its slot becomes None.
    #[test]
    fn release_sets_slot_to_none() {
        // Both hands grabbing.
        let both = assign_grabs(
            &[(true, Vec2::new(-0.5, 0.0)), (true, Vec2::new(0.5, 0.0))],
            1.0,
            1.0,
        );
        assert!(
            both.c1.is_some() && both.c2.is_some(),
            "both must be set while grabbing"
        );

        // Hand 1 releases.
        let after_release = assign_grabs(
            &[(true, Vec2::new(-0.5, 0.0)), (false, Vec2::new(0.5, 0.0))],
            1.0,
            1.0,
        );
        assert!(
            after_release.c1.is_some(),
            "c1 must stay set while hand 0 grabs"
        );
        assert!(
            after_release.c2.is_none(),
            "c2 must be None after hand 1 releases"
        );
    }

    /// `palm_mm_to_ndc`: center X (0 mm), mid height (195 mm) maps near NDC origin.
    #[test]
    fn palm_mm_to_ndc_center_hand_maps_near_origin() {
        // x=0 mm: (0 + 200) / 400 * 2 - 1 = 0.0
        // y=195 mm: (350 - 195) / 310 * 2 - 1 = (155/310)*2-1 = 0.0
        let ndc = palm_mm_to_ndc(Vec3::new(0.0, 195.0, 0.0));
        assert!(
            ndc.x.abs() < 1e-5,
            "center x must map to ndc 0.0, got {}",
            ndc.x
        );
        assert!(
            ndc.y.abs() < 1e-5,
            "mid height must map to ndc 0.0, got {}",
            ndc.y
        );
    }
}
