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

use crate::input::hand::{LandmarkIndex, LANDMARK_COUNT};
use crate::input::projection::{LEAP_X_HALFRANGE_MM, LEAP_Y_MAX_MM, LEAP_Y_MIN_MM};

/// Fallback depth (mm, Leap convention) when the size estimator is disabled.
///
/// A single webcam yields no *direct* hand-Z: the landmark model's `z` is a
/// relative, near-zero depth ([`super::landmark::project_landmarks`] scales it
/// by the ROI size, so it lands around `±0.1`), **not** a Leap-range depth in
/// `[40, 350]`. Downstream consumers written for Leap assume the mm convention —
/// most consequentially Line's power model
/// `wanted = grab^1.5 · 5^((−z + 350) / 160)`. Feeding it a near-zero `z` makes
/// the depth term `5^(350/160) ≈ 34×`, which pins the attractor on regardless of
/// grab.
///
/// The live path derives depth from apparent hand size instead (see
/// [`estimate_depth_mm`]); this constant is the **escape hatch**: setting the
/// calibration gain `k <= 0` (dev-panel slider "Depth calibration k") disables
/// the estimator and pins `z` here, restoring the fixed-depth behaviour where
/// the power term is a *constant* and grab alone drives attractor strength.
/// That makes `k = 0` the instant rollback knob during a live set if the
/// estimated depth ever misbehaves on stage.
///
/// Calibration: `5^((−120 + 350) / 160) ≈ 10.1×`, so under the pin a full fist
/// (`grab = 1`) reaches power `≈ 10` — matching a mouse press
/// (`crate`-external `MOUSE_POWER_PRESS = 10`), the known-good interactive
/// reference — while a relaxed hand decays toward zero. In estimator terms the
/// pin sits at `≈ 0.51 m` (inverting [`distance_m_to_leap_z_mm`]), so the
/// familiar at-rest feel carries over when the estimator is on and the hand is
/// at a typical desk distance.
pub const MEDIAPIPE_DEPTH_PROXY_MM: f32 = 120.0;

/// Default calibration gain `k` for [`size_estimated_distance_m`]: the camera
/// focal length expressed in **square-side units** (the unit of the padded
/// square image whose `[0, 1]` span the landmarks are normalized to).
///
/// Pinhole model: a segment of metric length `S` at distance `D` projects to
/// `S · f / D` on the sensor, so with `f` in square-side units the normalized
/// image segment is `image_size = f · world_size / distance` — inverted by the
/// estimator. A typical 63° HFOV webcam has `f = (W/2) / tan(31.5°) ≈ 0.82·W`,
/// i.e. `k ≈ 0.82` of the square side; `0.8` is a round default close to that.
///
/// Sanity example (also pinned by a unit test): a 0.08 m wrist→middle-MCP
/// segment at 0.6 m reads `0.8 · 0.08 / 0.6 ≈ 0.107` of the square side.
///
/// **Calibration procedure** (hardware, dev panel): stand at a tape-measured
/// 0.5 m from the camera with an open, steady hand and tune the
/// "Depth calibration k" slider until the "Est. distance (mm)" diagnostic reads
/// ≈ 500 mm. Cross-checks: at rest distance the Line attractor power should
/// match the previous build's ~10× feel; pushing toward the camera should
/// strengthen it smoothly without latching; beyond ~1 m it should fade to 1×.
pub const DEFAULT_DEPTH_CALIBRATION_K: f32 = 0.8;

/// Near rail of the depth remap: estimated camera distances at/under `0.35 m`
/// map to [`DEPTH_NEAR_LEAP_MM`]. Anchored to Line's power model
/// `5^((−z + 350) / 160)`: the near rail's `z = 40` gives `5^(310/160) ≈ 22.7×`
/// — the strongest push-in response, reached with the hand at arm-into-the-lens
/// range.
pub const DEPTH_NEAR_M: f32 = 0.35;

/// Far rail of the depth remap: estimated camera distances at/over `1.0 m` map
/// to [`DEPTH_FAR_LEAP_MM`], whose `z = 350` makes the power term exactly `1×`
/// — a hand a metre or more away contributes no depth boost. The old fixed
/// 120 mm pin corresponds to `≈ 0.51 m` on this ramp (`≈ 10×`), preserving the
/// familiar at-rest strength.
pub const DEPTH_FAR_M: f32 = 1.0;

/// Leap z (mm) emitted at the near rail ([`DEPTH_NEAR_M`]); the Leap working
/// volume's near plane and the power model's `≈ 22.7×` maximum.
pub const DEPTH_NEAR_LEAP_MM: f32 = 40.0;

/// Leap z (mm) emitted at the far rail ([`DEPTH_FAR_M`]); the power model's
/// `1×` neutral point.
pub const DEPTH_FAR_LEAP_MM: f32 = 350.0;

/// Floor for the normalized image segment in [`size_estimated_distance_m`]:
/// guards the division when landmarks collapse (a degenerate segment reads as
/// "infinitely far" and clamps to the far rail rather than dividing by zero).
const MIN_IMAGE_SEGMENT_NORM: f32 = 1e-4;

/// Estimate the hand's camera distance (metres) from apparent size.
///
/// Similar triangles / pinhole projection: `image_size = k · world_size /
/// distance`, inverted to `distance = k · world_size / image_size`, where
///
/// - `world_size_m` — metric length of a reference hand segment from the
///   landmark model's WORLD output (the pipeline uses wrist → middle MCP, the
///   same segment as [`super::signals::hand_scale`]; ~0.08–0.09 m on an adult);
/// - `image_size_norm` — the same segment's projected length in
///   **square-normalized** image units (xy only). Square-norm is isotropic
///   (one scale for both axes), which a length measurement needs;
///   content-norm is NOT (the bar-stripping rescales y differently from x for
///   a non-square camera) and must not be used here;
/// - `k` — the camera focal length in square-side units
///   ([`DEFAULT_DEPTH_CALIBRATION_K`]).
#[must_use]
pub fn size_estimated_distance_m(world_size_m: f32, image_size_norm: f32, k: f32) -> f32 {
    // distance = k · S / s, with s floored so a collapsed segment reads as
    // far (→ clamped to the far rail downstream), never a division by zero.
    k * world_size_m / image_size_norm.max(MIN_IMAGE_SEGMENT_NORM)
}

/// Remap an estimated camera distance (m) into the Leap z convention (mm).
///
/// Linear ramp `[DEPTH_NEAR_M, DEPTH_FAR_M] → [DEPTH_NEAR_LEAP_MM,
/// DEPTH_FAR_LEAP_MM]`, clamped to the rails — see those constants for the
/// power-model anchoring (0.35 m → ~22.7×, ~0.51 m → ~10× like the old pin,
/// ≥ 1 m → 1×).
#[must_use]
pub fn distance_m_to_leap_z_mm(distance_m: f32) -> f32 {
    // t ∈ [0, 1] across the ramp: 0 at the near rail, 1 at the far rail.
    let t = (distance_m - DEPTH_NEAR_M) / (DEPTH_FAR_M - DEPTH_NEAR_M);
    // Lerp into the Leap z range, clamped so out-of-ramp distances hold a rail.
    (DEPTH_FAR_LEAP_MM - DEPTH_NEAR_LEAP_MM)
        .mul_add(t, DEPTH_NEAR_LEAP_MM)
        .clamp(DEPTH_NEAR_LEAP_MM, DEPTH_FAR_LEAP_MM)
}

/// Size-estimated hand depth in the Leap z convention (mm), or the fixed
/// [`MEDIAPIPE_DEPTH_PROXY_MM`] pin when the estimator is disabled.
///
/// Measures the wrist → middle-MCP reference segment in both landmark spaces —
/// metric metres in `world`, square-normalized xy in `img_square_norm` — and
/// runs it through [`size_estimated_distance_m`] + [`distance_m_to_leap_z_mm`].
///
/// `k <= 0` is the **escape hatch**: it returns the pin exactly, reproducing
/// the pre-estimator behaviour (see [`MEDIAPIPE_DEPTH_PROXY_MM`]). The raw
/// estimate is noisy frame-to-frame; the pipeline smooths it per track
/// ([`super::signals::HandTracker::assign`]'s depth EMA) before emitting.
#[must_use]
pub fn estimate_depth_mm(
    world: &[Vec3; LANDMARK_COUNT],
    img_square_norm: &[Vec3; LANDMARK_COUNT],
    k: f32,
) -> f32 {
    if k <= 0.0 {
        return MEDIAPIPE_DEPTH_PROXY_MM;
    }
    let wrist = LandmarkIndex::Wrist.as_index();
    let middle_mcp = LandmarkIndex::MiddleMcp.as_index();
    // Metric segment: full 3D distance (the world output is orthographic, so
    // its z is real geometry).
    let world_size_m = world[wrist].distance(world[middle_mcp]);
    // Image segment: xy ONLY — image z is a relative model value in a
    // different unit and would corrupt the projected length.
    let image_size_norm = img_square_norm[wrist]
        .truncate()
        .distance(img_square_norm[middle_mcp].truncate());
    distance_m_to_leap_z_mm(size_estimated_distance_m(world_size_m, image_size_norm, k))
}

/// Map a content-normalized `MediaPipe` image point into the Leap-device-mm
/// convention.
///
/// - `p.x`, `p.y` are **content-normalized** coordinates in `[0, 1]` (origin
///   top-left, +y down). The pipeline's `ContentRect::to_content_norm` step
///   strips the square-padding bars before this call, so `[0, 1]` spans the
///   camera's actual image area and the full Leap Y range is reachable.
///   (Prior to Phase P3 this received raw square-norm coordinates, which
///   compressed vertical reach to 56% for a 1280×720 camera.)
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

    // --- size-estimated depth (Phase P5) ----------------------------------

    /// Build (world, image) landmark arrays whose wrist→middle-MCP segments
    /// have the given lengths (world metres, image square-norm units). Only
    /// indices 0 and 9 matter to the estimator; the rest stay zero.
    fn depth_fixture(
        world_size_m: f32,
        image_size_norm: f32,
    ) -> (
        [Vec3; crate::input::hand::LANDMARK_COUNT],
        [Vec3; crate::input::hand::LANDMARK_COUNT],
    ) {
        use crate::input::hand::LandmarkIndex;
        let mut world = [Vec3::ZERO; crate::input::hand::LANDMARK_COUNT];
        let mut img = [Vec3::ZERO; crate::input::hand::LANDMARK_COUNT];
        world[LandmarkIndex::Wrist.as_index()] = Vec3::ZERO;
        world[LandmarkIndex::MiddleMcp.as_index()] = Vec3::new(0.0, -world_size_m, 0.0);
        img[LandmarkIndex::Wrist.as_index()] = Vec3::new(0.5, 0.6, 0.02);
        // Image z is deliberately non-zero junk: the estimator must measure the
        // segment in xy ONLY (square-norm is isotropic in xy; z is a relative
        // model value in a different unit).
        img[LandmarkIndex::MiddleMcp.as_index()] = Vec3::new(0.5, 0.6 - image_size_norm, -0.04);
        (world, img)
    }

    #[test]
    fn estimator_recovers_distance_from_similar_triangles() {
        // The doc's sanity example: a 0.08 m hand segment seen as 0.107 of the
        // square side with k = 0.8 ⇒ 0.8 · 0.08 / 0.107 ≈ 0.598 m.
        let d = size_estimated_distance_m(0.08, 0.107, 0.8);
        assert!((d - 0.598).abs() < 0.005, "distance {d} m");
        // Remapped into Leap z: (0.598 − 0.35) / 0.65 of [40, 350] ≈ 158.4 mm.
        let z = distance_m_to_leap_z_mm(d);
        assert!((z - 158.4).abs() < 1.0, "z {z} mm");
    }

    #[test]
    fn estimated_depth_is_monotonic_in_inverse_image_size() {
        // Smaller on screen ⇒ farther away ⇒ larger Leap z (weaker power term).
        let z_of = |img: f32| distance_m_to_leap_z_mm(size_estimated_distance_m(0.08, img, 0.8));
        assert!(z_of(0.16) < z_of(0.107), "bigger image segment is nearer");
        assert!(z_of(0.107) < z_of(0.09), "smaller image segment is farther");
    }

    #[test]
    fn estimated_depth_clamps_at_both_rails() {
        // A huge image segment (hand at the lens) → below D_NEAR_M → near rail.
        let near = distance_m_to_leap_z_mm(size_estimated_distance_m(0.08, 0.5, 0.8));
        assert!((near - 40.0).abs() < 1e-3, "near rail {near}");
        // A tiny image segment → beyond D_FAR_M → far rail.
        let far = distance_m_to_leap_z_mm(size_estimated_distance_m(0.08, 0.02, 0.8));
        assert!((far - 350.0).abs() < 1e-3, "far rail {far}");
        // A degenerate (zero) image segment must not divide by zero; it reads
        // as "infinitely far" and rides the far rail.
        let degenerate = distance_m_to_leap_z_mm(size_estimated_distance_m(0.08, 0.0, 0.8));
        assert!((degenerate - 350.0).abs() < 1e-3, "degenerate {degenerate}");
    }

    #[test]
    fn non_positive_k_disables_the_estimator_to_the_fixed_pin() {
        // The escape hatch: k <= 0 must reproduce today's fixed 120 mm pin
        // EXACTLY (instant rollback knob during a live set).
        let (world, img) = depth_fixture(0.09, 0.15);
        assert!(
            (estimate_depth_mm(&world, &img, 0.0) - MEDIAPIPE_DEPTH_PROXY_MM).abs() < f32::EPSILON
        );
        assert!(
            (estimate_depth_mm(&world, &img, -0.5) - MEDIAPIPE_DEPTH_PROXY_MM).abs() < f32::EPSILON
        );
        // And a positive k uses the wrist→middle-MCP segments:
        // 0.8 · 0.09 / 0.15 = 0.48 m → (0.48 − 0.35)/0.65 · 310 + 40 = 102 mm.
        let z = estimate_depth_mm(&world, &img, 0.8);
        assert!((z - 102.0).abs() < 0.5, "z {z} mm");
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
