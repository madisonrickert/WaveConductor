//! Leap palm-position to world-space coordinate projection.
//!
//! Ported from v4's `.worktrees/v4/src/leap/util.ts:115-124`
//! (`mapLeapToThreePosition`). Top-down orthographic mapping onto the
//! Leap device's (X, Y) plane — Y is height above the device, NOT the
//! user-facing Z axis. The hand's vertical motion above the device drives
//! the on-screen vertical position.
//!
//! ## Two coordinate stories: anisotropic reach, isotropic shape
//!
//! This module maps two different *kinds* of quantity out of Leap mm-space,
//! and they deliberately use different scales:
//!
//! - **Position** ([`palm_to_world`]) maps a single point (the palm, a
//!   fingertip delta for a gesture, …) so it can reach every edge of the
//!   window. The Leap tracking box is far from window-shaped — it is a
//!   `400mm × 310mm` roughly-square volume, remapped onto windows that range
//!   from ultrawide landscape to tall portrait — so each axis is scaled
//!   *independently* to the window's full width/height
//!   (`px/mm` ratio = `(window.x / LEAP_X_SPAN_MM) / (window.y / LEAP_Y_SPAN_MM)`,
//!   which is `~1.38` at 16:9 landscape and `~0.44` at 9:16 portrait). Dots'
//!   and Line's attractors and Flame's gesture deltas rely on this — a
//!   single point has no shape to distort, so the anisotropy is invisible.
//! - **Shape** ([`bone_to_world`]) maps a *constellation* of points (the 20
//!   hand-mesh bone centres) relative to a shared anchor. If each bone were
//!   pushed through `palm_to_world` independently, the whole hand would
//!   inherit the axis's px/mm ratio as visible squash/stretch — imperceptible
//!   in 16:9 landscape (`~1.38`, close to isotropic) but a hand squashed to
//!   `~44%` width in 9:16 portrait. `bone_to_world` instead maps the anchor
//!   through `palm_to_world` (so the hand still reaches the full window) and
//!   places every other bone relative to that anchor using one **isotropic**
//!   px/mm scale on both axes, so the hand's internal proportions never
//!   distort regardless of window aspect ratio.

use bevy::math::{Vec2, Vec3};

/// Leap palm X full half-range, in millimetres. The device tracks
/// `[-200, +200]` mm horizontally as the usable region.
pub const LEAP_X_HALFRANGE_MM: f32 = 200.0;

/// Lowest palm height (mm above device) we map to screen-bottom.
pub const LEAP_Y_MIN_MM: f32 = 40.0;

/// Highest palm height (mm above device) we map to screen-top.
pub const LEAP_Y_MAX_MM: f32 = 350.0;

/// Full width of the Leap tracking box on X, in millimetres
/// (`2 * LEAP_X_HALFRANGE_MM`). Named so [`palm_to_world`] and
/// [`bone_to_world`] share one source of truth for the mm→px denominator.
pub const LEAP_X_SPAN_MM: f32 = 2.0 * LEAP_X_HALFRANGE_MM;

/// Full height of the Leap tracking box on Y, in millimetres
/// (`LEAP_Y_MAX_MM - LEAP_Y_MIN_MM`). This is also the denominator
/// [`bone_to_world`] uses for its isotropic px/mm scale (see module docs) —
/// the "fit-to-height" convention already used by the radiance sketch's
/// aspect-correct dancer.
pub const LEAP_Y_SPAN_MM: f32 = LEAP_Y_MAX_MM - LEAP_Y_MIN_MM;

/// Fraction of the screen reserved as deadzone at each edge.
///
/// v4 reserved 20% on each side (usable centre 60%), but that clamps the hand
/// to a box well inside the window — the full range of hand motion can't reach
/// the edges. We map the full Leap range to the full viewport (`0.0` deadzone)
/// so the hand mesh and attractor can travel to the window edges. This is an
/// intentional divergence from v4 (per the "full range of motion" requirement);
/// the constant stays so a deadzone can be reintroduced if desired.
pub const SCREEN_DEADZONE: f32 = 0.0;

/// Maps a Leap palm position to centered world-space coordinates.
///
/// Inputs:
/// - `palm_mm` — palm centroid in Leap device coordinates (mm). Uses x, y;
///   z is ignored for position (but consumed by Line's power modulator).
/// - `window` — viewport size in logical pixels.
///
/// Output: world-space `Vec2` with origin at screen center, +y up. Compatible
/// with v5's existing mouse-attractor coordinate system.
///
/// Mapping (with the default `SCREEN_DEADZONE = 0.0`):
/// - X: `-200..+200 mm` → `0%..100%` of canvas width.
/// - Y: `350..40 mm` (high to low) → `0%..100%` of canvas height (inverted —
///   raising the hand moves the attractor toward screen-top).
#[must_use]
pub fn palm_to_world(palm_mm: Vec3, window: Vec2) -> Vec2 {
    let usable = 1.0 - 2.0 * SCREEN_DEADZONE;

    let x_norm = ((palm_mm.x + LEAP_X_HALFRANGE_MM) / LEAP_X_SPAN_MM).clamp(0.0, 1.0);
    let canvas_x = window.x * (SCREEN_DEADZONE + usable * x_norm);
    let world_x = canvas_x - window.x * 0.5;

    let y_norm = ((LEAP_Y_MAX_MM - palm_mm.y) / LEAP_Y_SPAN_MM).clamp(0.0, 1.0);
    let canvas_y = window.y * (SCREEN_DEADZONE + usable * y_norm);
    let world_y = -(canvas_y - window.y * 0.5);

    Vec2::new(world_x, world_y)
}

/// Maps a hand bone centre to world space with **isotropic shape** around a
/// **reach-preserving anchor**.
///
/// The anchor (typically the palm centroid, `palm_mm`) maps through
/// [`palm_to_world`] unchanged — it keeps the existing per-axis reach, so the
/// hand as a whole can still travel to every window edge. Every bone is then
/// placed *relative to that anchor* using a single px/mm scale derived from
/// the window height (`window.y / LEAP_Y_SPAN_MM` — the same scale
/// [`palm_to_world`]'s Y axis already uses), applied identically to both the
/// X and Y mm offsets. Because one scalar drives both axes, a bone's
/// millimetre offset from the palm is never stretched or squashed
/// differently per axis — the hand's proportions look the same in landscape
/// and portrait, only its overall reach differs (per [`palm_to_world`]).
///
/// Inputs:
/// - `palm_mm` — the hand's anchor point in Leap device coordinates (mm),
///   passed straight through to [`palm_to_world`]. Z is ignored (see
///   [`palm_to_world`]'s doc).
/// - `bone_mm` — a bone centre in the same Leap device coordinates (mm).
/// - `window` — viewport size in logical pixels.
///
/// Output: world-space `Vec2`, same convention as [`palm_to_world`] (origin
/// at screen center, +y up).
#[must_use]
pub fn bone_to_world(palm_mm: Vec3, bone_mm: Vec3, window: Vec2) -> Vec2 {
    let anchor = palm_to_world(palm_mm, window);

    // Single isotropic px/mm scale, driven by height only (fit-to-height),
    // applied to both the X and Y mm deltas so the bone constellation's
    // shape is preserved regardless of window aspect ratio.
    let scale = window.y / LEAP_Y_SPAN_MM;

    // Both mm axes and both world axes increase in the same physical
    // direction here (mm +x is device-right = world +x screen-right; mm +y
    // is "higher above the device" = world +y screen-up), so the offset
    // carries over with no sign flip — unlike `palm_to_world`, which inverts
    // Y because it maps into a *canvas* Y (top-down) before flipping to
    // world Y (bottom-up).
    let offset_mm = Vec2::new(bone_mm.x - palm_mm.x, bone_mm.y - palm_mm.y);

    anchor + offset_mm * scale
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Vec2 = Vec2::new(1280.0, 720.0);

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.5
    }

    #[test]
    fn center_palm_maps_near_screen_center_with_slight_y_bias() {
        // palm at (0, 200, 0) — mid-Y of the Leap range
        let world = palm_to_world(Vec3::new(0.0, 200.0, 0.0), WINDOW);
        // X should be exactly center
        assert!(approx(world.x, 0.0), "x = {}", world.x);
        // Y: Y range is [40..350], mid is 195 (not 200). So palm Y=200 is
        // slightly above mid -> world Y slightly positive. With deadzone 0:
        // y_norm = (350 - 200) / 310 = 0.4839; canvas_y = 720 * 0.4839 = 348.4
        // world_y = -(348.4 - 360.0) = 11.6
        assert!(approx(world.y, 11.6), "y = {}", world.y);
    }

    #[test]
    fn upper_left_extreme_maps_to_window_corner() {
        // palm at (-200, 350, _) — hand far left, hand high → top-left corner.
        let world = palm_to_world(Vec3::new(-LEAP_X_HALFRANGE_MM, LEAP_Y_MAX_MM, 0.0), WINDOW);
        // X: canvas_x = 0 * 1280 = 0; world_x = 0 - 640 = -640 (left edge).
        assert!(approx(world.x, -640.0), "x = {}", world.x);
        // Y: canvas_y = 0; world_y = -(0 - 360) = 360 (top edge).
        assert!(approx(world.y, 360.0), "y = {}", world.y);
    }

    #[test]
    fn lower_right_extreme_maps_to_window_corner() {
        // palm at (+200, 40, _) — hand far right, hand low → bottom-right corner.
        let world = palm_to_world(Vec3::new(LEAP_X_HALFRANGE_MM, LEAP_Y_MIN_MM, 0.0), WINDOW);
        // X: canvas_x = 1280; world_x = 1280 - 640 = 640 (right edge).
        assert!(approx(world.x, 640.0), "x = {}", world.x);
        // Y: canvas_y = 720; world_y = -(720 - 360) = -360 (bottom edge).
        assert!(approx(world.y, -360.0), "y = {}", world.y);
    }

    #[test]
    fn out_of_range_palm_clamps_to_window_edge() {
        // palm at (-300, 500, _) — beyond Leap's stated range
        let world = palm_to_world(Vec3::new(-300.0, 500.0, 0.0), WINDOW);
        // Both axes should clamp to the window edge — same as -200, 350.
        let edge = palm_to_world(Vec3::new(-LEAP_X_HALFRANGE_MM, LEAP_Y_MAX_MM, 0.0), WINDOW);
        assert!(approx(world.x, edge.x));
        assert!(approx(world.y, edge.y));
    }

    #[test]
    fn z_axis_is_ignored_for_position() {
        // Two palms differing only in Z should map to the same world coords.
        let a = palm_to_world(Vec3::new(50.0, 150.0, 0.0), WINDOW);
        let b = palm_to_world(Vec3::new(50.0, 150.0, 250.0), WINDOW);
        let c = palm_to_world(Vec3::new(50.0, 150.0, -250.0), WINDOW);
        assert!(approx(a.x, b.x) && approx(a.y, b.y), "{a:?} != {b:?}");
        assert!(approx(a.x, c.x) && approx(a.y, c.y), "{a:?} != {c:?}");
    }

    // -------------------------------------------------------------------
    // bone_to_world
    // -------------------------------------------------------------------

    const PORTRAIT_WINDOW: Vec2 = Vec2::new(1080.0, 1920.0);
    const LANDSCAPE_WINDOW: Vec2 = Vec2::new(1920.0, 1080.0);

    /// The anchor bone (offset zero from the palm) must map to exactly the
    /// same point `palm_to_world` would give the palm directly — the anchor
    /// carries the existing anisotropic reach mapping unchanged.
    #[test]
    fn anchor_bone_matches_palm_to_world() {
        for window in [WINDOW, PORTRAIT_WINDOW, LANDSCAPE_WINDOW] {
            let palm = Vec3::new(-75.0, 220.0, 180.0);
            let expected = palm_to_world(palm, window);
            let got = bone_to_world(palm, palm, window);
            assert!(
                approx(got.x, expected.x) && approx(got.y, expected.y),
                "window={window:?}: anchor bone {got:?} != palm_to_world {expected:?}"
            );
        }
    }

    /// A (10mm, 10mm) bone offset from the palm must map to an exactly
    /// isotropic pixel offset — equal magnitude on both axes — regardless of
    /// whether the window is portrait or landscape. This is the core
    /// shape-preservation property: `palm_to_world`'s anisotropic per-axis
    /// scale (which would otherwise squash this offset horizontally in
    /// portrait) must not leak into bone placement.
    #[test]
    fn isotropic_bone_offset_is_equal_on_both_axes_in_any_orientation() {
        let palm = Vec3::new(0.0, 195.0, 200.0);
        let bone = palm + Vec3::new(10.0, 10.0, 0.0);

        for window in [PORTRAIT_WINDOW, LANDSCAPE_WINDOW] {
            let anchor = bone_to_world(palm, palm, window);
            let placed = bone_to_world(palm, bone, window);
            let delta = placed - anchor;
            assert!(
                approx(delta.x, delta.y),
                "window={window:?}: offset not isotropic, delta={delta:?}"
            );
            // Sanity: the offset must be nonzero (scale isn't accidentally 0).
            assert!(delta.x.abs() > 0.0, "window={window:?}: zero offset");
        }
    }

    /// The isotropic scale used for bone offsets must equal the height-based
    /// px/mm scale `palm_to_world`'s Y axis already applies
    /// (`window.y / LEAP_Y_SPAN_MM`) — this is the documented "fit-to-height"
    /// contract, and it must hold in both window orientations.
    #[test]
    fn bone_offset_scale_matches_existing_y_behavior() {
        for window in [WINDOW, PORTRAIT_WINDOW, LANDSCAPE_WINDOW] {
            let palm = Vec3::new(0.0, 195.0, 200.0);
            let dy_mm = 12.5;
            let bone = palm + Vec3::new(0.0, dy_mm, 0.0);

            let anchor = bone_to_world(palm, palm, window);
            let placed = bone_to_world(palm, bone, window);
            let actual_delta_y = placed.y - anchor.y;

            let expected_scale = window.y / LEAP_Y_SPAN_MM;
            let expected_delta_y = dy_mm * expected_scale;

            assert!(
                approx(actual_delta_y, expected_delta_y),
                "window={window:?}: delta_y={actual_delta_y}, expected={expected_delta_y}"
            );

            // Cross-check against palm_to_world directly: moving the *palm*
            // by the same dy_mm (holding X fixed at the range interior so
            // clamping doesn't interfere) must move world_y by the same
            // amount, since both use the same y_norm-derived scale.
            let palm_moved = palm + Vec3::new(0.0, dy_mm, 0.0);
            let world_before = palm_to_world(palm, window);
            let world_after = palm_to_world(palm_moved, window);
            let palm_delta_y = world_after.y - world_before.y;
            assert!(
                approx(actual_delta_y, palm_delta_y),
                "window={window:?}: bone delta_y={actual_delta_y} != palm_to_world delta_y={palm_delta_y}"
            );
        }
    }

    /// A pure-X offset must use the *height*-based scale, not the
    /// window-width-based scale `palm_to_world`'s X axis would use — this is
    /// what makes bone placement isotropic instead of inheriting X's
    /// independent (and, in portrait, much larger) px/mm ratio.
    #[test]
    fn bone_offset_x_uses_height_based_scale_not_width_based() {
        let palm = Vec3::new(0.0, 195.0, 200.0);
        let dx_mm = 15.0;
        let bone = palm + Vec3::new(dx_mm, 0.0, 0.0);

        for window in [PORTRAIT_WINDOW, LANDSCAPE_WINDOW] {
            let anchor = bone_to_world(palm, palm, window);
            let placed = bone_to_world(palm, bone, window);
            let actual_delta_x = placed.x - anchor.x;

            let height_based = dx_mm * (window.y / LEAP_Y_SPAN_MM);
            let width_based = dx_mm * (window.x / LEAP_X_SPAN_MM);

            assert!(
                approx(actual_delta_x, height_based),
                "window={window:?}: delta_x={actual_delta_x}, expected height-based={height_based}"
            );
            if approx(window.x, window.y) {
                // Square window: the two scales coincide, nothing to
                // distinguish — skip the "not width-based" check.
                continue;
            }
            assert!(
                (actual_delta_x - width_based).abs() > 0.5,
                "window={window:?}: delta_x unexpectedly matches width-based scale"
            );
        }
    }
}
