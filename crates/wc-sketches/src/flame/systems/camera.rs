//! CPU orbit camera for the Flame sketch: autorotate + drag + wheel zoom +
//! grab-fling momentum (F10 sets [`FlameCamera::angular_velocity`] on release).
//!
//! No `Camera3d` entity exists (see the plan's "Approved deviations" note):
//! [`FlameCamera`] is pure CPU state, and [`crate::flame::render::drive_flame_material`]
//! reads it each frame to build the two mat4 uniforms the vertex shader uses
//! to project the point cloud in-material, drawing into the app's single 2D
//! window camera.

use std::f32::consts::TAU;

use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::prelude::*;
use wc_core::input::pointer::PointerState;

use crate::flame::settings::FlameSettings;

/// v4's `OrbitControls` minimum/maximum distance bounds.
const MIN_DISTANCE: f32 = 0.1;
/// v4's `OrbitControls` minimum/maximum distance bounds.
const MAX_DISTANCE: f32 = 8.0;
/// Polar angle clamp margin (v4 `OrbitControls` avoids the poles to keep the
/// look-at basis well-defined).
const POLAR_EPSILON: f32 = 0.01;
/// Wheel-zoom sensitivity: each scroll "line" scales distance by this factor.
const ZOOM_SENSITIVITY: f32 = 0.1;
/// Per-frame-at-60fps momentum decay (v4 kept per-frame units; applied
/// dt-scaled below so the decay rate is frame-rate independent).
const MOMENTUM_DECAY: f32 = 0.95;

/// CPU orbit camera around the origin. Produces the two mat4 uniforms; no
/// `Camera3d` entity exists (see the plan's deviation note).
#[derive(Resource, Debug, Clone, Copy)]
pub struct FlameCamera {
    /// Azimuth around +Y, radians.
    pub azimuth: f32,
    /// Polar angle from +Y, radians, clamped to `(POLAR_EPSILON, PI - POLAR_EPSILON)`.
    pub polar: f32,
    /// Orbit radius, clamped to v4's `OrbitControls` bounds `[0.1, 8.0]`.
    pub distance: f32,
    /// Grab-fling momentum (azimuth, polar) in rad/frame-at-60fps (v4 kept
    /// per-frame units; applied dt-scaled: `v * dt * 60`).
    pub angular_velocity: Vec2,
    /// Cursor position at the previous frame while dragging.
    pub last_drag: Option<Vec2>,
}

impl Default for FlameCamera {
    /// v4 start pose: eye `(0.0, 0.35, 0.7)`. `distance = sqrt(0.35^2 + 0.7^2)`,
    /// `polar = acos(0.35 / distance)` (angle from +Y), `azimuth = 0.0` (eye
    /// lies in the +Z half of the XZ plane, azimuth's zero direction).
    fn default() -> Self {
        let distance = (0.35_f32 * 0.35 + 0.7 * 0.7).sqrt();
        Self {
            azimuth: 0.0,
            polar: (0.35_f32 / distance).acos(),
            distance,
            angular_velocity: Vec2::ZERO,
            last_drag: None,
        }
    }
}

impl FlameCamera {
    /// Spherical-to-Cartesian eye position, orbiting the origin.
    ///
    /// `polar` is measured from +Y (not from the equator), so `y = cos(polar)`
    /// and the `x`/`z` split carries `sin(polar)` scaled by the azimuth's
    /// sine/cosine — the standard physics convention, matching v4's
    /// `OrbitControls` spherical parametrization.
    #[must_use]
    pub fn eye(&self) -> Vec3 {
        self.distance
            * Vec3::new(
                self.polar.sin() * self.azimuth.sin(),
                self.polar.cos(),
                self.polar.sin() * self.azimuth.cos(),
            )
    }

    /// `view_from_model = view * rotateX(-PI/2)`, baking v4's
    /// `pointCloud.rotateX(-PI/2)` model transform into the view matrix (see
    /// [`crate::flame::render::default_view_matrices`] for the fixed-pose
    /// equivalent this replaces).
    #[must_use]
    pub fn view_from_model(&self) -> Mat4 {
        let view = Mat4::look_at_rh(self.eye(), Vec3::ZERO, Vec3::Y);
        view * Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2)
    }

    /// Perspective projection: fovy 60 deg, near 0.01, far 25 (v4 camera),
    /// matching [`crate::flame::render::default_view_matrices`]'s projection.
    #[must_use]
    pub fn clip_from_view(aspect: f32) -> Mat4 {
        Mat4::perspective_rh(60.0_f32.to_radians(), aspect, 0.01, 25.0)
    }
}

/// `Update`, gated under **both** `sketch_active(Flame)` and
/// `in_screensaver(Flame)`: autorotate is the screensaver's motion, while
/// drag/zoom input is inert there (no pointer capture during attract mode).
///
/// Order of operations each frame: autorotate advances azimuth, a held-left-
/// button drag overrides azimuth/polar directly from the cursor delta, wheel
/// scroll rescales distance, and — when nothing is dragging — decaying fling
/// momentum (set by F10's hand-grab release) keeps nudging azimuth/polar.
/// Polar is clamped last so no path can push the eye through a pole.
pub fn update_flame_camera(
    time: Res<'_, Time>,
    settings: Res<'_, FlameSettings>,
    pointer: Res<'_, PointerState>,
    mouse_buttons: Res<'_, ButtonInput<MouseButton>>,
    scroll: Res<'_, AccumulatedMouseScroll>,
    window: Single<'_, '_, &Window>,
    mut camera: ResMut<'_, FlameCamera>,
) {
    let dt = time.delta_secs();

    // Autorotate: speed 1 = one orbit (TAU radians) per 60 seconds, v4's
    // `OrbitControls.autoRotateSpeed = 1` convention.
    camera.azimuth += settings.autorotate_speed * (TAU / 60.0) * dt;

    // Pointer drag: while the left button is held, the frame-to-frame cursor
    // delta (in window logical pixels) drives azimuth/polar directly. THREE's
    // `OrbitControls` divides both axes by the client *height* (not a
    // per-axis width/height split), matching here.
    let h = window.height().max(1.0);
    if mouse_buttons.pressed(MouseButton::Left) {
        if let Some(cursor) = pointer.cursor {
            if let Some(last) = camera.last_drag {
                let delta = cursor - last;
                camera.azimuth -= delta.x / h * TAU;
                camera.polar -= delta.y / h * TAU;
            }
            camera.last_drag = Some(cursor);
        }
    } else {
        camera.last_drag = None;
    }

    // Wheel zoom: each scroll line scales distance by (1 - 0.1 * lines),
    // clamped to v4's OrbitControls bounds.
    if scroll.delta.y != 0.0 {
        camera.distance = (camera.distance * (1.0 - ZOOM_SENSITIVITY * scroll.delta.y))
            .clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    // Fling momentum: only applied while nothing is actively dragging (a held
    // drag overrides the pose directly above). F10 sets `angular_velocity` on
    // hand-grab release; it decays geometrically each frame toward zero.
    if camera.last_drag.is_none() {
        let velocity = camera.angular_velocity;
        camera.azimuth -= velocity.x * dt * 60.0;
        camera.polar -= velocity.y * dt * 60.0;
        camera.angular_velocity *= MOMENTUM_DECAY.powf(dt * 60.0);
    }

    // Clamp last: no path above (autorotate, drag, fling) can push the eye
    // through a pole, which would make `view_from_model`'s look-at basis
    // degenerate.
    camera.polar = camera
        .polar
        .clamp(POLAR_EPSILON, std::f32::consts::PI - POLAR_EPSILON);
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;

    /// Default pose = v4 camera (0, 0.35, 0.7).
    #[test]
    fn default_pose_is_v4_camera() {
        let cam = FlameCamera::default();
        let eye = cam.eye();
        assert!((eye.x - 0.0).abs() < 1e-5);
        assert!((eye.y - 0.35).abs() < 1e-4);
        assert!((eye.z - 0.7).abs() < 1e-4);
    }

    /// Autorotate at speed 1 covers TAU in 60 s of accumulated dt.
    #[test]
    fn autorotate_speed_one_is_one_orbit_per_minute() {
        let mut az = 0.0_f32;
        let dt = 1.0 / 60.0;
        for _ in 0..3600 {
            az += 1.0 * (std::f32::consts::TAU / 60.0) * dt;
        }
        assert!((az - std::f32::consts::TAU).abs() < 1e-3);
    }

    /// Zoom clamps to v4's `OrbitControls` bounds [0.1, 8.0].
    #[test]
    fn zoom_clamps_to_v4_bounds() {
        let mut cam = FlameCamera::default();
        for _ in 0..200 {
            cam.distance = (cam.distance * 0.9).clamp(0.1, 8.0);
        }
        assert!((cam.distance - 0.1).abs() < 1e-6);
        for _ in 0..200 {
            cam.distance = (cam.distance * 1.1).clamp(0.1, 8.0);
        }
        assert!((cam.distance - 8.0).abs() < 1e-6);
    }

    /// Momentum decays toward zero at 0.95/frame and moves the azimuth.
    #[test]
    fn fling_momentum_decays() {
        let mut cam = FlameCamera {
            angular_velocity: Vec2::new(0.02, 0.0),
            ..FlameCamera::default()
        };
        let az0 = cam.azimuth;
        let dt = 1.0 / 60.0;
        for _ in 0..240 {
            cam.azimuth -= cam.angular_velocity.x * dt * 60.0;
            cam.angular_velocity *= 0.95_f32.powf(dt * 60.0);
        }
        assert!((cam.azimuth - az0).abs() > 1e-6, "azimuth must move");
        assert!(cam.angular_velocity.length() < 1e-4, "momentum must decay");
    }

    /// Matrices are finite for arbitrary poses (no NaN at polar clamp edges).
    #[test]
    fn matrices_are_finite() {
        for polar in [0.011_f32, 1.0, std::f32::consts::PI - 0.011] {
            let cam = FlameCamera {
                polar,
                azimuth: 2.3,
                distance: 3.0,
                ..FlameCamera::default()
            };
            let m = cam.view_from_model();
            assert!(m.is_finite());
        }
    }
}
