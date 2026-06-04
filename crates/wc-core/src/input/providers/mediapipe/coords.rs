//! `MediaPipe` image-normalized coordinates → Leap-device-millimetre convention.
//!
//! Downstream consumers ([`crate::input::projection::palm_to_world`], Line's
//! `grab^1.5 · 5^((−z+350)/160)` power model, `HandMesh`) were written for the
//! Leap provider, which emits palm position in **device millimetres**: x in
//! `[-200, +200]` ([`LEAP_X_HALFRANGE_MM`]), y as height-above-device in
//! `[40, 350]` ([`LEAP_Y_MIN_MM`]..[`LEAP_Y_MAX_MM`]). To keep every consumer
//! unchanged, the `MediaPipe` provider maps the full webcam frame into that same
//! convention rather than inventing a new coordinate space.
//!
//! `MediaPipe` emits normalized image coordinates: x, y in `[0, 1]` with the
//! origin at the **top-left** (+y points **down**). The mapping therefore flips
//! y, optionally mirrors x (webcam-as-mirror), and rescales into millimetres.

use bevy::math::Vec3;

use crate::input::projection::{LEAP_X_HALFRANGE_MM, LEAP_Y_MAX_MM, LEAP_Y_MIN_MM};

/// Map a `MediaPipe` normalized image point into the Leap-device-mm convention.
///
/// - `p.x`, `p.y` are normalized image coordinates in `[0, 1]` (origin top-left,
///   +y down).
/// - `p.z` is the caller-supplied depth proxy already expressed in the mm
///   convention the power model expects (passed through unchanged here; hand-Z
///   is not required deck-wide, so it is best-effort — see the design spec).
/// - `mirror` flips x so the webcam behaves as a mirror: a hand at the left of
///   the image (`x = 0`) appears at the user's right (`+200 mm`).
///
/// Returns the point in Leap-device millimetres: x in `[-200, +200]`, y as
/// height-above-device in `[40, 350]`.
// Consumed by the two-stage pipeline (plan Phase 8); lands here with the
// coordinate-glue foundation and its tests ahead of its caller.
#[allow(dead_code)]
#[must_use]
pub fn image_norm_to_leap_mm(p: Vec3, mirror: bool) -> Vec3 {
    let x_m = if mirror { 1.0 - p.x } else { p.x };
    // [0, 1] → [-HALF, +HALF].
    let x_mm = (x_m - 0.5) * (2.0 * LEAP_X_HALFRANGE_MM);
    // Image y (top = 0) → height mm (top = MAX): y_mm = MAX - y·(MAX − MIN).
    let y_mm = LEAP_Y_MAX_MM - p.y * (LEAP_Y_MAX_MM - LEAP_Y_MIN_MM);
    Vec3::new(x_mm, y_mm, p.z)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Vec3;

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 0.5, "{a} vs {b}");
    }

    #[test]
    fn frame_left_maps_to_positive_x_when_mirrored() {
        // Mirror on: a hand at image-left (x=0) appears at the user's RIGHT
        // → +200 mm. (Webcam-as-mirror.)
        let p = image_norm_to_leap_mm(Vec3::new(0.0, 0.5, 0.0), true);
        approx(p.x, LEAP_X_HALFRANGE_MM);
    }

    #[test]
    fn frame_right_maps_to_negative_x_when_mirrored() {
        let p = image_norm_to_leap_mm(Vec3::new(1.0, 0.5, 0.0), true);
        approx(p.x, -LEAP_X_HALFRANGE_MM);
    }

    #[test]
    fn raising_hand_maps_toward_screen_top() {
        // image y=0 is the top of the frame → height LEAP_Y_MAX_MM.
        let top = image_norm_to_leap_mm(Vec3::new(0.5, 0.0, 0.0), true);
        approx(top.y, LEAP_Y_MAX_MM);
        let bot = image_norm_to_leap_mm(Vec3::new(0.5, 1.0, 0.0), true);
        approx(bot.y, LEAP_Y_MIN_MM);
    }

    #[test]
    fn mirror_off_preserves_left_right() {
        // No mirror: image-left (x=0) stays left → -200 mm.
        let p = image_norm_to_leap_mm(Vec3::new(0.0, 0.5, 0.0), false);
        approx(p.x, -LEAP_X_HALFRANGE_MM);
    }

    #[test]
    fn z_passes_through_unchanged() {
        let p = image_norm_to_leap_mm(Vec3::new(0.3, 0.7, 123.0), true);
        approx(p.z, 123.0);
    }

    #[test]
    fn agrees_with_leap_projection_on_a_centered_pose() {
        use crate::input::projection::palm_to_world;
        use bevy::math::Vec2;

        let window = Vec2::new(1280.0, 720.0);
        // A palm at image-center (mirror on) → (0 mm, mid-height). Through
        // palm_to_world that lands near screen-center, exactly as a Leap
        // mid-range palm does — proving the `MediaPipe` mapping feeds the
        // existing projection the same way the Leap provider does.
        let mm = image_norm_to_leap_mm(Vec3::new(0.5, 0.5, 0.0), true);
        let world = palm_to_world(mm, window);
        assert!(world.x.abs() < 1.0, "x={}", world.x);
        // Leap Y range mid is 195 mm; image-center maps to exactly 195 mm, which
        // projects to a small positive y (slight bias), well within half-screen.
        assert!(world.y.abs() < 40.0, "y={}", world.y);
    }
}
