//! Leap palm-position to world-space coordinate projection.
//!
//! Ported from v4's `.worktrees/v4/src/leap/util.ts:115-124`
//! (`mapLeapToThreePosition`). Top-down orthographic mapping onto the
//! Leap device's (X, Y) plane — Y is height above the device, NOT the
//! user-facing Z axis. The hand's vertical motion above the device drives
//! the on-screen vertical position.

use bevy::math::{Vec2, Vec3};

/// Leap palm X full half-range, in millimetres. The device tracks
/// `[-200, +200]` mm horizontally as the usable region.
pub const LEAP_X_HALFRANGE_MM: f32 = 200.0;

/// Lowest palm height (mm above device) we map to screen-bottom.
pub const LEAP_Y_MIN_MM: f32 = 40.0;

/// Highest palm height (mm above device) we map to screen-top.
pub const LEAP_Y_MAX_MM: f32 = 350.0;

/// Fraction of the screen reserved as deadzone at each edge. v4 uses 20%,
/// so the usable region is the centered 60% of the viewport.
pub const SCREEN_DEADZONE: f32 = 0.2;

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
/// v4 mapping:
/// - X: `-200..+200 mm` → `20%..80%` of canvas width.
/// - Y: `350..40 mm` (high to low) → `20%..80%` of canvas height (inverted —
///   raising the hand moves the attractor toward screen-top).
#[must_use]
pub fn palm_to_world(palm_mm: Vec3, window: Vec2) -> Vec2 {
    let usable = 1.0 - 2.0 * SCREEN_DEADZONE;

    let x_norm = ((palm_mm.x + LEAP_X_HALFRANGE_MM) / (2.0 * LEAP_X_HALFRANGE_MM)).clamp(0.0, 1.0);
    let canvas_x = window.x * (SCREEN_DEADZONE + usable * x_norm);
    let world_x = canvas_x - window.x * 0.5;

    let y_norm =
        ((LEAP_Y_MAX_MM - palm_mm.y) / (LEAP_Y_MAX_MM - LEAP_Y_MIN_MM)).clamp(0.0, 1.0);
    let canvas_y = window.y * (SCREEN_DEADZONE + usable * y_norm);
    let world_y = -(canvas_y - window.y * 0.5);

    Vec2::new(world_x, world_y)
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
        // slightly above mid -> world Y slightly positive.
        // y_norm = (350 - 200) / 310 = 0.4839; canvas_y = 720 * (0.2 + 0.6*0.4839) = 720 * 0.4903 = 353.0
        // world_y = -(353.0 - 360.0) = 7.0
        assert!(approx(world.y, 7.0), "y = {}", world.y);
    }

    #[test]
    fn upper_left_extreme_maps_to_upper_left_usable_corner() {
        // palm at (-200, 350, _) — hand far left, hand high
        let world = palm_to_world(Vec3::new(-LEAP_X_HALFRANGE_MM, LEAP_Y_MAX_MM, 0.0), WINDOW);
        // X: canvas_x = 0.2 * 1280 = 256; world_x = 256 - 640 = -384
        assert!(approx(world.x, -384.0), "x = {}", world.x);
        // Y: canvas_y = 0.2 * 720 = 144; world_y = -(144 - 360) = 216
        assert!(approx(world.y, 216.0), "y = {}", world.y);
    }

    #[test]
    fn lower_right_extreme_maps_to_lower_right_usable_corner() {
        // palm at (+200, 40, _) — hand far right, hand low
        let world = palm_to_world(Vec3::new(LEAP_X_HALFRANGE_MM, LEAP_Y_MIN_MM, 0.0), WINDOW);
        assert!(approx(world.x, 384.0), "x = {}", world.x);
        assert!(approx(world.y, -216.0), "y = {}", world.y);
    }

    #[test]
    fn out_of_range_palm_clamps_to_usable_edge() {
        // palm at (-300, 500, _) — beyond Leap's stated range
        let world = palm_to_world(Vec3::new(-300.0, 500.0, 0.0), WINDOW);
        // Both axes should clamp to the usable edge — same as -200, 350.
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
}
