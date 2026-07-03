//! CPU orbit camera for the Flame sketch: autorotate + drag + wheel zoom +
//! grab-fling momentum (F10 sets [`FlameCamera::angular_velocity`] on release) +
//! two-hand pan (moves the [`FlameCamera::target`] look-at point) + a
//! settle-to-home ease that recenters polar/distance/target whenever nothing
//! is actively holding the camera (no hand grab, no mouse drag).
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
use crate::flame::systems::hands::FlameGrabState;

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
/// Vertical field of view shared by [`FlameCamera::clip_from_view`] and the
/// pan math in [`FlameCamera::pan_by_pixels`] (v4 camera: 60 degrees).
const FOVY: f32 = std::f32::consts::PI / 3.0;
/// Maximum distance the pan target may wander from the origin: keeps the
/// fractal recoverable in-frame no matter how far a two-hand drag runs.
const PAN_MAX_RADIUS: f32 = 2.0;

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
    /// Orbit/look-at center, in model-world space. `Vec3::ZERO` is v4's fixed
    /// origin. Written only by [`FlameCamera::pan_by_pixels`] and the
    /// screensaver recenter (settle-to-home, see [`update_flame_camera`]).
    pub target: Vec3,
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
            target: Vec3::ZERO,
        }
    }
}

impl FlameCamera {
    /// Spherical-to-Cartesian eye position, orbiting [`FlameCamera::target`].
    ///
    /// `polar` is measured from +Y (not from the equator), so `y = cos(polar)`
    /// and the `x`/`z` split carries `sin(polar)` scaled by the azimuth's
    /// sine/cosine — the standard physics convention, matching v4's
    /// `OrbitControls` spherical parametrization.
    #[must_use]
    pub fn eye(&self) -> Vec3 {
        self.target
            + self.distance
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
        let view = Mat4::look_at_rh(self.eye(), self.target, Vec3::Y);
        view * Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2)
    }

    /// Perspective projection: fovy 60 deg, near 0.01, far 25 (v4 camera),
    /// matching [`crate::flame::render::default_view_matrices`]'s projection.
    #[must_use]
    pub fn clip_from_view(aspect: f32) -> Mat4 {
        Mat4::perspective_rh(FOVY, aspect, 0.01, 25.0)
    }

    /// Set the orbit radius, clamped to v4's `OrbitControls` bounds
    /// `[MIN_DISTANCE, MAX_DISTANCE]` — the single write path shared by wheel
    /// zoom and the two-hand spread zoom.
    pub fn set_distance_clamped(&mut self, distance: f32) {
        self.distance = distance.clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    /// Translate the pan `target` by a window-pixel delta (top-left origin,
    /// +y down), so on-screen content follows the hands (grab metaphor):
    /// hands moving right pan the camera left, hands moving down aim it up.
    ///
    /// `world_per_pixel = 2 * distance * tan(fovy/2) / window_height` is the
    /// world-space width of one pixel at the target plane, so pan speed
    /// matches apparent on-screen hand speed at any zoom. The target is
    /// clamped to `PAN_MAX_RADIUS` so the fractal can always be recovered.
    pub fn pan_by_pixels(&mut self, delta_px: Vec2, window_height: f32, sensitivity: f32) {
        // Unit vector from target toward the eye (the spherical terms are
        // already normalized); forward is its negation.
        let toward_eye = Vec3::new(
            self.polar.sin() * self.azimuth.sin(),
            self.polar.cos(),
            self.polar.sin() * self.azimuth.cos(),
        );
        let forward = -toward_eye;
        // look_at_rh basis: right = forward x up_world, up = right x forward.
        // The polar clamp keeps forward off the poles, so the cross products
        // stay well-conditioned.
        let right = forward.cross(Vec3::Y).normalize();
        let up = right.cross(forward);
        let world_per_pixel = 2.0 * self.distance * (FOVY * 0.5).tan() / window_height.max(1.0);
        let motion = (-right * delta_px.x + up * delta_px.y) * world_per_pixel * sensitivity;
        self.target = (self.target + motion).clamp_length_max(PAN_MAX_RADIUS);
    }

    /// Ease `polar`, `distance`, and the pan `target` toward the default
    /// (v4 start) pose by `alpha` — the settle-to-home restoration that
    /// guarantees no gesture leaves the kiosk in a permanently ugly state
    /// (Dots' `fabric_tension` home-spring is the same idea for particles).
    /// `azimuth` is deliberately exempt: autorotate owns it, and every
    /// azimuth is an equally valid view of the fractal.
    pub fn ease_toward_home(&mut self, alpha: f32) {
        let home = Self::default();
        self.polar += (home.polar - self.polar) * alpha;
        self.distance += (home.distance - self.distance) * alpha;
        // Home target is the origin, so the lerp reduces to a scale.
        self.target *= 1.0 - alpha;
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
/// Then, whenever no hand is grabbing and no mouse drag is in progress, a
/// settle-to-home ease pulls polar/distance/[`FlameCamera::target`] back
/// toward the default pose. Polar is clamped last so no path can push the eye
/// through a pole. Last of all (debug builds only),
/// `WC_DEBUG_FORCE_FLAME_CAMERA_POSE` pins a fixed zoomed-in/panned-off-center
/// pose for the `flame-camera-pose` capture scenario, overriding every path
/// above.
#[allow(
    clippy::too_many_arguments,
    reason = "a Bevy system's parameters are its data dependencies; the debug \
              camera-pose pin adds a ninth, and splitting would obscure the \
              single per-frame orbit/pan/settle pipeline"
)]
pub fn update_flame_camera(
    time: Res<'_, Time>,
    settings: Res<'_, FlameSettings>,
    pointer: Res<'_, PointerState>,
    mouse_buttons: Res<'_, ButtonInput<MouseButton>>,
    scroll: Res<'_, AccumulatedMouseScroll>,
    window: Single<'_, '_, &Window>,
    grab: Res<'_, FlameGrabState>,
    mut camera: ResMut<'_, FlameCamera>,
    #[cfg(debug_assertions)] debug_toggles: Option<Res<'_, wc_core::debug::DebugToggles>>,
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
        let zoomed = camera.distance * (1.0 - ZOOM_SENSITIVITY * scroll.delta.y);
        camera.set_distance_clamped(zoomed);
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

    // Settle-to-home: whenever nothing actively holds the camera (no hand
    // grabbing, no mouse drag), polar/distance/target ease back to the v4
    // start pose so the kiosk always recovers from any gesture. The ease is
    // dt-correct (`1 - exp(-dt/tau)`), gentle enough to coexist with a
    // decaying fling, and also runs during the screensaver — that is what
    // recenters an abandoned pan for attract mode. During the Idle activity
    // window this system does not run at all (zero-systems-when-Idle), so the
    // ease pauses there by design; the screensaver resumes it as the backstop.
    if grab.grabbing_count == 0 && camera.last_drag.is_none() {
        // `.max(0.1)` guards a hand-edited settings file against div-by-zero.
        let alpha = 1.0 - (-dt / settings.camera_return_seconds.max(0.1)).exp();
        camera.ease_toward_home(alpha);
    }

    // Wrap azimuth to [0, TAU): autorotate accumulates it unbounded, and after
    // a multi-hour soak `sin`/`cos` of a many-thousand-radian f32 lose enough
    // precision to micro-jitter the orbit. Wrapping is invisible (every
    // consumer takes sin/cos) and keeps the argument small forever.
    camera.azimuth = camera.azimuth.rem_euclid(TAU);

    // Clamp last: no path above (autorotate, drag, fling) can push the eye
    // through a pole, which would make `view_from_model`'s look-at basis
    // degenerate.
    camera.polar = camera
        .polar
        .clamp(POLAR_EPSILON, std::f32::consts::PI - POLAR_EPSILON);

    // Debug: WC_DEBUG_FORCE_FLAME_CAMERA_POSE pins a deterministic
    // zoomed-in/panned-off-center pose for the `flame-camera-pose` capture
    // scenario, overriding every interaction path above (autorotate, drag,
    // fling, settle-to-home) each frame so the target-aware view matrix has a
    // fixed, reproducible regression fixture without a pointer or hand.
    #[cfg(debug_assertions)]
    if debug_toggles
        .as_ref()
        .is_some_and(|t| t.force_flame_camera_pose)
    {
        camera.azimuth = 0.9;
        camera.polar = 1.1;
        camera.distance = 0.35;
        camera.target = Vec3::new(0.2, 0.0, 0.1);
        camera.angular_velocity = Vec2::ZERO;
    }
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

    /// Zoom clamps to v4's `OrbitControls` bounds `[0.1, 8.0]`.
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
                target: Vec3::new(1.5, -0.8, 0.4),
                ..FlameCamera::default()
            };
            let m = cam.view_from_model();
            assert!(m.is_finite());
        }
    }

    /// Pan moves the target opposite the hand delta's screen-right direction
    /// (content follows the hands), scaled by distance and fovy.
    #[test]
    fn pan_by_pixels_moves_target_left_when_hands_move_right() {
        let mut cam = FlameCamera::default(); // azimuth 0: eye on +Z, right = +X
        cam.pan_by_pixels(Vec2::new(10.0, 0.0), 720.0, 1.0);
        assert!(
            cam.target.x < 0.0,
            "target must move -X, got {}",
            cam.target.x
        );
        assert!(cam.target.y.abs() < 1e-6);
        assert!(cam.target.z.abs() < 1e-6);
    }

    /// Hands moving down (window +y) aim the camera up: target moves toward
    /// world +Y (camera-up at the default pose has positive Y).
    #[test]
    fn pan_by_pixels_moves_target_up_when_hands_move_down() {
        let mut cam = FlameCamera::default();
        cam.pan_by_pixels(Vec2::new(0.0, 10.0), 720.0, 1.0);
        assert!(
            cam.target.y > 0.0,
            "target must gain +Y, got {}",
            cam.target.y
        );
    }

    /// The pan target never leaves the `PAN_MAX_RADIUS` ball.
    #[test]
    fn pan_by_pixels_clamps_target_radius() {
        let mut cam = FlameCamera::default();
        for _ in 0..100 {
            cam.pan_by_pixels(Vec2::new(500.0, 300.0), 720.0, 1.0);
        }
        assert!(cam.target.length() <= PAN_MAX_RADIUS + 1e-4);
    }

    /// Sensitivity 0 disables pan entirely.
    #[test]
    fn pan_by_pixels_sensitivity_zero_is_inert() {
        let mut cam = FlameCamera::default();
        cam.pan_by_pixels(Vec2::new(50.0, 50.0), 720.0, 0.0);
        assert_eq!(cam.target, Vec3::ZERO);
    }

    /// `set_distance_clamped` enforces the v4 `OrbitControls` bounds.
    #[test]
    fn set_distance_clamped_enforces_bounds() {
        let mut cam = FlameCamera::default();
        cam.set_distance_clamped(0.001);
        assert!((cam.distance - 0.1).abs() < 1e-6);
        cam.set_distance_clamped(100.0);
        assert!((cam.distance - 8.0).abs() < 1e-6);
        cam.set_distance_clamped(1.5);
        assert!((cam.distance - 1.5).abs() < 1e-6);
    }

    /// The settle-to-home ease converges polar/distance/target back to the
    /// default pose while leaving azimuth alone (autorotate owns it).
    #[test]
    fn ease_toward_home_converges_pose_and_exempts_azimuth() {
        let home = FlameCamera::default();
        let mut cam = FlameCamera {
            azimuth: 2.7,
            polar: 3.0,
            distance: 7.5,
            target: Vec3::new(1.5, -0.5, 1.0),
            ..FlameCamera::default()
        };
        let dt = 1.0_f32 / 60.0;
        // 8s time constant, simulated for 60s: deviation shrinks by e^-7.5.
        let alpha = 1.0 - (-dt / 8.0_f32).exp();
        for _ in 0..3600 {
            cam.ease_toward_home(alpha);
        }
        assert!((cam.polar - home.polar).abs() < 1e-2);
        assert!((cam.distance - home.distance).abs() < 1e-2);
        assert!(cam.target.length() < 1e-2);
        assert!((cam.azimuth - 2.7).abs() < 1e-6, "azimuth must be exempt");
    }

    /// A single ease step moves each regressing channel strictly toward home.
    #[test]
    fn ease_toward_home_single_step_moves_toward_home() {
        let home = FlameCamera::default();
        let mut cam = FlameCamera {
            polar: home.polar + 1.0,
            distance: home.distance + 3.0,
            target: Vec3::new(1.0, 0.0, 0.0),
            ..FlameCamera::default()
        };
        cam.ease_toward_home(0.1);
        assert!((cam.polar - home.polar - 0.9).abs() < 1e-5);
        assert!((cam.distance - home.distance - 2.7).abs() < 1e-5);
        assert!((cam.target.x - 0.9).abs() < 1e-5);
    }

    /// World-level: the settle-to-home ease must be suppressed while a hand
    /// grabs the camera, and resume once the grab releases — the gate lives
    /// in `update_flame_camera`'s wiring, which the pure-method tests above
    /// cannot see.
    #[test]
    fn settle_to_home_suppressed_while_grabbing_resumes_on_release() {
        use bevy::ecs::system::RunSystemOnce;

        let mut world = World::new();
        let mut time = Time::<()>::default();
        time.advance_by(std::time::Duration::from_millis(100));
        world.insert_resource(time);
        // Autorotate off: azimuth is not under test here.
        world.insert_resource(FlameSettings {
            autorotate_speed: 0.0,
            ..FlameSettings::default()
        });
        world.insert_resource(PointerState::default());
        world.insert_resource(ButtonInput::<MouseButton>::default());
        world.insert_resource(AccumulatedMouseScroll::default());
        world.spawn(Window::default());
        world.insert_resource(FlameCamera {
            distance: 5.0,
            target: Vec3::new(1.0, 0.0, 0.0),
            ..FlameCamera::default()
        });
        world.insert_resource(FlameGrabState {
            grabbing_count: 1,
            ..FlameGrabState::default()
        });

        world
            .run_system_once(update_flame_camera)
            .expect("update_flame_camera must run");
        let held = *world.resource::<FlameCamera>();
        assert!(
            (held.distance - 5.0).abs() < 1e-6,
            "no ease while grabbing, got distance {}",
            held.distance
        );
        assert!(
            (held.target.x - 1.0).abs() < 1e-6,
            "no ease while grabbing, got target.x {}",
            held.target.x
        );

        world.insert_resource(FlameGrabState::default());
        world
            .run_system_once(update_flame_camera)
            .expect("update_flame_camera must run");
        let released = *world.resource::<FlameCamera>();
        assert!(released.distance < 5.0, "ease resumes on release");
        assert!(released.target.x < 1.0, "ease resumes on release");
    }
}
