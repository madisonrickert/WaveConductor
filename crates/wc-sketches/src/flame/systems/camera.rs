//! CPU orbit camera for the Flame sketch: autorotate + drag + wheel zoom +
//! grab-fling momentum (the hand grab layer in
//! [`crate::flame::systems::hands`] sets [`FlameCamera::angular_velocity`]
//! from two-hand twist and [`FlameCamera::pan_velocity`] from one-hand pan on
//! release) + hand pan (moves the [`FlameCamera::target`] look-at point) + a
//! settle-to-home ease that recenters polar/distance/target whenever nothing
//! is actively holding the camera (no hand grab, no mouse drag, no live
//! momentum).
//!
//! No `Camera3d` entity exists (see the plan's "Approved deviations" note):
//! [`FlameCamera`] is pure CPU state, and [`crate::flame::render::drive_flame_material`]
//! reads it each frame to build the two mat4 uniforms the vertex shader uses
//! to project the point cloud in-material, drawing into the app's single 2D
//! window camera.

use std::f32::consts::TAU;

use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::prelude::*;
use wc_core::input::pointer::{PointerOverUi, PointerState};

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
/// dt-scaled below so the decay rate is frame-rate independent). Shared by
/// the angular (yaw/polar) and pan flings so both coasts feel like the same
/// material.
const MOMENTUM_DECAY: f32 = 0.95;
/// Below this magnitude a decaying momentum snaps to exactly zero, so the
/// geometric decay terminates, the settle-to-home gate reopens, and the idle
/// veto (which tests against the same `1e-4`) releases.
const MOMENTUM_EPSILON: f32 = 1e-4;
/// The v4 camera's field of view (60 degrees). In landscape this is the
/// vertical FOV exactly as in v4; [`contain_fovy`] widens the vertical FOV in
/// portrait so this 60-degree cone always spans the window's *smaller*
/// dimension. Shared by [`FlameCamera::clip_from_view`] and the pan math in
/// [`FlameCamera::pan_by_pixels`].
const FOVY: f32 = std::f32::consts::PI / 3.0;

/// Effective vertical FOV that fits the v4 60-degree view cone to the
/// window's **smaller** dimension (a "contain" fit).
///
/// `Mat4::perspective_rh` takes a *vertical* FOV and derives the horizontal
/// one as `2·atan(tan(fovy/2)·aspect)`. With a fixed `FOVY` that is correct
/// in landscape (the fractal is framed against the shorter, vertical axis)
/// but collapses in portrait: at 9:16 the horizontal FOV shrinks to ~36°, so
/// a fractal composed for a 60° cone is mostly cropped off the sides and the
/// window reads as empty.
///
/// - `aspect >= 1` (landscape/square): vertical FOV = `FOVY`, byte-identical
///   to the v4 projection — landscape rendering is unchanged.
/// - `aspect < 1` (portrait): the vertical FOV widens to
///   `2·atan(tan(FOVY/2) / aspect)`, which makes the *horizontal* FOV exactly
///   `FOVY`. The 60° cone now spans the window width, so the fractal shows
///   at the same scale relative to the smaller dimension as it does in
///   landscape instead of being cropped.
///
/// Pure (no world access) so the aspect math is unit-testable; both
/// [`FlameCamera::clip_from_view`] and the pan pixel→world mapping call it, so
/// the grab metaphor's "content follows the hand ~1:1" stays exact in
/// portrait too.
#[must_use]
pub fn contain_fovy(aspect: f32) -> f32 {
    // `.max(1e-3)` guards a degenerate zero/negative aspect (a zero-sized
    // window is already floored to 1 px by every caller, so this is defensive).
    let aspect = aspect.max(1e-3);
    if aspect >= 1.0 {
        FOVY
    } else {
        // tan is well-defined here: FOVY/2 = 30° < 90°.
        2.0 * ((FOVY * 0.5).tan() / aspect).atan()
    }
}
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
    /// per-frame units; applied dt-scaled: `v * dt * 60`). Set by the hand
    /// layer's two-hand twist release; applied as `azimuth -= v.x`,
    /// `polar -= v.y` while coasting.
    pub angular_velocity: Vec2,
    /// Pan-fling momentum in window-pixels/frame-at-60fps: the one-hand grab
    /// release's "thrown map" coast. Applied dt-scaled through
    /// [`FlameCamera::pan_by_pixels`] (the same pixel→world mapping the live
    /// grab uses, so the coast speed matches the hand speed at release) and
    /// decayed by `MOMENTUM_DECAY`; killed on hitting the `PAN_MAX_RADIUS`
    /// clamp rather than grinding against it.
    pub pan_velocity: Vec2,
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
            pan_velocity: Vec2::ZERO,
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

    /// Perspective projection: near 0.01, far 25 (v4 camera), with the
    /// [`contain_fovy`] vertical FOV — 60 degrees in landscape (v4 parity),
    /// widened in portrait so the 60-degree cone fits the window's smaller
    /// dimension instead of cropping the fractal off the sides.
    /// [`crate::flame::render::default_view_matrices`] delegates here so the
    /// spawn placeholder uses the identical projection.
    #[must_use]
    pub fn clip_from_view(aspect: f32) -> Mat4 {
        Mat4::perspective_rh(contain_fovy(aspect), aspect, 0.01, 25.0)
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
    /// matches apparent on-screen hand speed at any zoom. `fovy` is the
    /// [`contain_fovy`] the projection actually uses — with the fixed `FOVY`
    /// the mapping would undershoot in portrait, where the effective FOV is
    /// wider (equivalently: `2·d·tan(FOVY/2) / min(w, h)`, since the 60° cone
    /// is fitted to the smaller dimension). The target is clamped to
    /// `PAN_MAX_RADIUS` so the fractal can always be recovered.
    pub fn pan_by_pixels(&mut self, delta_px: Vec2, window_size: Vec2, sensitivity: f32) {
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
        // Same FOV the projection uses (contain-fit), so one pixel of hand
        // motion maps to exactly one pixel of on-screen content motion in
        // either orientation.
        let h = window_size.y.max(1.0);
        let fovy = contain_fovy(window_size.x.max(1.0) / h);
        let world_per_pixel = 2.0 * self.distance * (fovy * 0.5).tan() / h;
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
/// button drag overrides azimuth/polar directly from the cursor delta (the
/// operator's orbit — the only polar/tilt access; guests' hands never tilt),
/// wheel scroll rescales distance, and — when nothing is dragging or
/// grabbing — the decaying flings the hand layer left behind keep coasting:
/// `angular_velocity` (two-hand twist release) nudges azimuth/polar and
/// `pan_velocity` (one-hand pan release) keeps translating the target
/// through [`FlameCamera::pan_by_pixels`]. Then, whenever no hand is
/// grabbing, no mouse drag is in progress, and no momentum is still live, a
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
    over_ui: Res<'_, PointerOverUi>,
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
    // A held left button is a drag in progress whether or not the cursor is
    // momentarily over egui chrome, so gate on the button alone here: nulling
    // `last_drag` when the cursor crosses onto the settings panel / name box /
    // overlay buttons would make the `last_drag.is_none()` fling + settle-to-home
    // branches below fire mid-drag, hitching the orbit toward home. Only the
    // orbit *delta* is suppressed while over UI — egui owns the drag there, and
    // orbiting the fractal underneath the panel would fight it.
    if mouse_buttons.pressed(MouseButton::Left) {
        if let Some(cursor) = pointer.cursor {
            if !over_ui.0 {
                if let Some(last) = camera.last_drag {
                    let delta = cursor - last;
                    camera.azimuth -= delta.x / h * TAU;
                    camera.polar -= delta.y / h * TAU;
                }
            }
            // Advance `last_drag` even while over UI so the excursion is
            // swallowed (no delta jump when the cursor crosses back onto the
            // scene) and the drag stays "in progress" for the settle/fling gate.
            camera.last_drag = Some(cursor);
        }
    } else {
        camera.last_drag = None;
    }

    // Wheel zoom: each scroll line scales distance by (1 - 0.1 * lines),
    // clamped to v4's OrbitControls bounds. Suppressed while the pointer is
    // over egui UI so scrolling the settings panel does not also zoom.
    if !over_ui.0 && scroll.delta.y != 0.0 {
        let zoomed = camera.distance * (1.0 - ZOOM_SENSITIVITY * scroll.delta.y);
        camera.set_distance_clamped(zoomed);
    }

    // Fling momentum: only applied while nothing actively holds the camera —
    // a held drag overrides the pose directly above, and while a hand grabs,
    // the hand layer both drives the pose directly and rewrites the velocity
    // accumulators every frame (applying them here too would double-count the
    // motion). The hand layer seeds `angular_velocity` (two-hand twist
    // release) and `pan_velocity` (one-hand pan release, the "thrown map");
    // both decay geometrically each frame toward zero and snap to exactly
    // zero below `MOMENTUM_EPSILON` so the coast terminates.
    if grab.grabbing_count == 0 && camera.last_drag.is_none() {
        let velocity = camera.angular_velocity;
        camera.azimuth -= velocity.x * dt * 60.0;
        camera.polar -= velocity.y * dt * 60.0;
        camera.angular_velocity *= MOMENTUM_DECAY.powf(dt * 60.0);
        if camera.angular_velocity.length() < MOMENTUM_EPSILON {
            camera.angular_velocity = Vec2::ZERO;
        }

        // Pan coast: window-pixel velocity through the same pixel→world pan
        // mapping the live grab uses, so the coast continues at the release's
        // apparent on-screen speed at any zoom.
        if camera.pan_velocity != Vec2::ZERO {
            let step = camera.pan_velocity * dt * 60.0;
            camera.pan_by_pixels(
                step,
                Vec2::new(window.width().max(1.0), h),
                settings.hand_pan_sensitivity,
            );
            camera.pan_velocity *= MOMENTUM_DECAY.powf(dt * 60.0);
            // Die at the pan clamp rather than grinding against it: once the
            // target is pinned to the `PAN_MAX_RADIUS` ball, further coast
            // would only fight `pan_by_pixels`' clamp every frame.
            if camera.pan_velocity.length() < MOMENTUM_EPSILON
                || camera.target.length() >= PAN_MAX_RADIUS - 1e-3
            {
                camera.pan_velocity = Vec2::ZERO;
            }
        }
    }

    // Settle-to-home: whenever nothing actively holds the camera (no hand
    // grabbing, no mouse drag) and no fling is still coasting (a live pan or
    // yaw momentum owns the motion until it decays out — easing against it
    // would fight the throw), polar/distance/target ease back to the v4
    // start pose so the kiosk always recovers from any gesture. The ease is
    // dt-correct (`1 - exp(-dt/tau)`) and also runs during the screensaver —
    // that is what recenters an abandoned pan for attract mode. During the
    // Idle activity window this system does not run at all
    // (zero-systems-when-Idle), so the ease pauses there by design; the
    // screensaver resumes it as the backstop.
    if grab.grabbing_count == 0
        && camera.last_drag.is_none()
        && camera.angular_velocity.length() <= MOMENTUM_EPSILON
        && camera.pan_velocity.length() <= MOMENTUM_EPSILON
    {
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
        camera.pan_velocity = Vec2::ZERO;
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;

    /// Landscape and square windows keep the v4 vertical FOV exactly — the
    /// contain fit only engages below aspect 1, so landscape rendering is
    /// unchanged by the portrait fix.
    #[test]
    fn contain_fovy_is_v4_fovy_in_landscape_and_square() {
        for aspect in [16.0_f32 / 9.0, 21.0 / 9.0, 4.0 / 3.0, 1.0] {
            assert!(
                (contain_fovy(aspect) - FOVY).abs() < 1e-6,
                "aspect {aspect} must keep the v4 60-degree fovy"
            );
        }
    }

    /// In portrait the *horizontal* FOV equals the v4 60-degree cone: the
    /// half-width tangent `tan(fovy/2) * aspect` must equal `tan(FOVY/2)`,
    /// i.e. the cone is fitted to the smaller (horizontal) dimension.
    #[test]
    fn contain_fovy_portrait_fits_cone_to_width() {
        for aspect in [9.0_f32 / 16.0, 3.0 / 4.0, 0.3] {
            let fovy = contain_fovy(aspect);
            assert!(fovy > FOVY, "portrait must widen the vertical FOV");
            assert!(fovy < std::f32::consts::PI, "fovy must stay < 180 deg");
            let half_width_tan = (fovy * 0.5).tan() * aspect;
            assert!(
                (half_width_tan - (FOVY * 0.5).tan()).abs() < 1e-5,
                "aspect {aspect}: horizontal FOV must equal the v4 60-degree cone"
            );
        }
    }

    /// Degenerate aspect inputs stay finite (defensive floor).
    #[test]
    fn contain_fovy_degenerate_aspect_is_finite() {
        for aspect in [0.0_f32, -1.0, f32::MIN_POSITIVE] {
            let fovy = contain_fovy(aspect);
            assert!(fovy.is_finite() && fovy > 0.0 && fovy < std::f32::consts::PI);
        }
    }

    /// The projection matrix uses the contain-fit fovy: at 9:16 the clip-space
    /// x scale must equal the landscape y scale (60-degree cone on the width),
    /// and at 16:9 the matrix is byte-identical to the fixed-fovy v4 one.
    #[test]
    fn clip_from_view_contains_cone_in_smaller_dimension() {
        let landscape = FlameCamera::clip_from_view(16.0 / 9.0);
        let v4 = Mat4::perspective_rh(FOVY, 16.0 / 9.0, 0.01, 25.0);
        assert_eq!(
            landscape, v4,
            "landscape projection must be unchanged from v4"
        );

        let portrait = FlameCamera::clip_from_view(9.0 / 16.0);
        // col(0).x is the x (horizontal) focal scale = 1 / (tan(fovy/2)·aspect);
        // fitting the 60-degree cone to the width makes it 1 / tan(30 deg),
        // which is the landscape *vertical* scale col(1).y.
        assert!(
            (portrait.x_axis.x - landscape.y_axis.y).abs() < 1e-5,
            "portrait horizontal scale must equal the landscape vertical scale"
        );
    }

    /// Pan pixel→world mapping tracks the contain-fit projection: in portrait
    /// the same pixel delta at the same distance moves the target by
    /// `height / width` times more world units (one hand pixel still equals
    /// one content pixel on screen).
    #[test]
    fn pan_by_pixels_matches_portrait_projection() {
        let mut landscape_cam = FlameCamera::default();
        landscape_cam.pan_by_pixels(Vec2::new(10.0, 0.0), Vec2::new(1920.0, 1080.0), 1.0);
        let mut portrait_cam = FlameCamera::default();
        portrait_cam.pan_by_pixels(Vec2::new(10.0, 0.0), Vec2::new(1080.0, 1920.0), 1.0);
        // world_per_pixel = 2·d·tan(FOVY/2)/min(w,h): identical min dimension
        // (1080) ⇒ identical world motion for the same pixel delta.
        assert!(
            (portrait_cam.target.x - landscape_cam.target.x).abs() < 1e-6,
            "same smaller dimension must give the same pan mapping ({} vs {})",
            portrait_cam.target.x,
            landscape_cam.target.x
        );
    }

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
        cam.pan_by_pixels(Vec2::new(10.0, 0.0), Vec2::new(1280.0, 720.0), 1.0);
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
        cam.pan_by_pixels(Vec2::new(0.0, 10.0), Vec2::new(1280.0, 720.0), 1.0);
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
            cam.pan_by_pixels(Vec2::new(500.0, 300.0), Vec2::new(1280.0, 720.0), 1.0);
        }
        assert!(cam.target.length() <= PAN_MAX_RADIUS + 1e-4);
    }

    /// Sensitivity 0 disables pan entirely.
    #[test]
    fn pan_by_pixels_sensitivity_zero_is_inert() {
        let mut cam = FlameCamera::default();
        cam.pan_by_pixels(Vec2::new(50.0, 50.0), Vec2::new(1280.0, 720.0), 0.0);
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
        world.insert_resource(PointerOverUi::default());
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

    /// Scrolling while the pointer is over egui UI must NOT zoom the fractal —
    /// the settings-panel scroll-leak regression. Starting at the home pose
    /// (settle-to-home is a no-op there) isolates the wheel's effect: with the
    /// guard set a nonzero wheel delta leaves the distance untouched; clearing
    /// it lets the same delta zoom.
    #[test]
    fn scroll_over_ui_does_not_zoom_but_scroll_over_scene_does() {
        use bevy::ecs::system::RunSystemOnce;

        fn distance_after_scroll(over_ui: bool) -> f32 {
            let mut world = World::new();
            let mut time = Time::<()>::default();
            time.advance_by(std::time::Duration::from_millis(16));
            world.insert_resource(time);
            world.insert_resource(FlameSettings {
                autorotate_speed: 0.0,
                ..FlameSettings::default()
            });
            world.insert_resource(PointerState::default());
            world.insert_resource(PointerOverUi(over_ui));
            world.insert_resource(ButtonInput::<MouseButton>::default());
            world.insert_resource(AccumulatedMouseScroll {
                delta: Vec2::new(0.0, 3.0),
                ..Default::default()
            });
            world.spawn(Window::default());
            world.insert_resource(FlameCamera::default());
            world.insert_resource(FlameGrabState::default());
            world
                .run_system_once(update_flame_camera)
                .expect("update_flame_camera must run");
            world.resource::<FlameCamera>().distance
        }

        let home = FlameCamera::default().distance;
        assert!(
            (distance_after_scroll(true) - home).abs() < 1e-6,
            "scroll over UI must not zoom"
        );
        assert!(
            (distance_after_scroll(false) - home).abs() > 1e-6,
            "scroll over the scene must zoom"
        );
    }

    /// Holding the left button while the cursor is over egui UI must NOT null
    /// `last_drag` and trigger settle-to-home mid-drag: a drag that crosses onto
    /// a corner button or the panel stays a drag, so the pose is held rather than
    /// easing home. (The regression: gating the drag branch on `!over_ui` dropped
    /// `last_drag`, activating the settle-to-home ease while the button was held.)
    #[test]
    fn held_drag_over_ui_does_not_settle_to_home() {
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
        // A drag in progress that has wandered onto the panel: cursor present,
        // over UI, left button held.
        world.insert_resource(PointerState {
            cursor: Some(Vec2::new(200.0, 200.0)),
            ..PointerState::default()
        });
        world.insert_resource(PointerOverUi(true));
        let mut buttons = ButtonInput::<MouseButton>::default();
        buttons.press(MouseButton::Left);
        world.insert_resource(buttons);
        world.insert_resource(AccumulatedMouseScroll::default());
        world.spawn(Window::default());
        world.insert_resource(FlameCamera {
            distance: 5.0,
            target: Vec3::new(1.0, 0.0, 0.0),
            ..FlameCamera::default()
        });
        world.insert_resource(FlameGrabState::default());

        world
            .run_system_once(update_flame_camera)
            .expect("update_flame_camera must run");
        let cam = *world.resource::<FlameCamera>();
        assert!(
            cam.last_drag.is_some(),
            "a held drag over UI must stay a drag in progress"
        );
        assert!(
            (cam.distance - 5.0).abs() < 1e-6,
            "no settle-to-home while a drag is held, got distance {}",
            cam.distance
        );
        assert!(
            (cam.target.x - 1.0).abs() < 1e-6,
            "target must not ease home mid-drag, got {}",
            cam.target.x
        );
    }

    /// Build a minimal world for `update_flame_camera` runs: autorotate off,
    /// no pointer/scroll input, the given camera and grab state.
    fn camera_world(camera: FlameCamera, grab: FlameGrabState) -> World {
        let mut world = World::new();
        let mut time = Time::<()>::default();
        time.advance_by(std::time::Duration::from_millis(100));
        world.insert_resource(time);
        world.insert_resource(FlameSettings {
            autorotate_speed: 0.0,
            ..FlameSettings::default()
        });
        world.insert_resource(PointerState::default());
        world.insert_resource(PointerOverUi::default());
        world.insert_resource(ButtonInput::<MouseButton>::default());
        world.insert_resource(AccumulatedMouseScroll::default());
        world.spawn(Window::default());
        world.insert_resource(camera);
        world.insert_resource(grab);
        world
    }

    /// A released pan fling coasts (the target keeps moving with the same
    /// content-follows-hand sign as the live pan) and decays toward zero.
    #[test]
    fn pan_momentum_coasts_and_decays() {
        use bevy::ecs::system::RunSystemOnce;

        let mut world = camera_world(
            FlameCamera {
                pan_velocity: Vec2::new(10.0, 0.0),
                ..FlameCamera::default()
            },
            FlameGrabState::default(),
        );
        world
            .run_system_once(update_flame_camera)
            .expect("update_flame_camera must run");
        let cam = *world.resource::<FlameCamera>();
        assert!(
            cam.target.x < 0.0,
            "+x pan momentum must keep panning the target -X, got {}",
            cam.target.x
        );
        assert!(
            cam.pan_velocity.length() < 10.0,
            "pan momentum must decay, got {}",
            cam.pan_velocity.length()
        );
        assert!(cam.pan_velocity.x > 0.0, "decay must not reverse the coast");
    }

    /// A pan fling dies at the `PAN_MAX_RADIUS` clamp instead of grinding
    /// against it forever.
    #[test]
    fn pan_momentum_dies_at_the_clamp() {
        use bevy::ecs::system::RunSystemOnce;

        // Content-follows-hand: a -x pixel velocity pans the target +X, i.e.
        // outward against a target already pinned at +X on the clamp ball.
        let mut world = camera_world(
            FlameCamera {
                target: Vec3::new(PAN_MAX_RADIUS, 0.0, 0.0),
                pan_velocity: Vec2::new(-50.0, 0.0),
                ..FlameCamera::default()
            },
            FlameGrabState::default(),
        );
        world
            .run_system_once(update_flame_camera)
            .expect("update_flame_camera must run");
        let cam = *world.resource::<FlameCamera>();
        assert_eq!(
            cam.pan_velocity,
            Vec2::ZERO,
            "momentum must die at the pan clamp"
        );
        assert!(cam.target.length() <= PAN_MAX_RADIUS + 1e-4);
    }

    /// While a hand grabs, the camera must not double-apply the momentum
    /// accumulators the hand layer is rewriting each frame: no coast happens.
    #[test]
    fn momentum_not_applied_while_grabbing() {
        use bevy::ecs::system::RunSystemOnce;

        let mut world = camera_world(
            FlameCamera {
                angular_velocity: Vec2::new(0.1, 0.0),
                pan_velocity: Vec2::new(10.0, 0.0),
                ..FlameCamera::default()
            },
            FlameGrabState {
                grabbing_count: 1,
                ..FlameGrabState::default()
            },
        );
        let az0 = FlameCamera::default().azimuth;
        world
            .run_system_once(update_flame_camera)
            .expect("update_flame_camera must run");
        let cam = *world.resource::<FlameCamera>();
        assert!(
            (cam.azimuth - az0).abs() < 1e-6,
            "angular momentum must not apply mid-grab"
        );
        assert_eq!(
            cam.target,
            Vec3::ZERO,
            "pan momentum must not apply mid-grab"
        );
        assert_eq!(
            cam.pan_velocity,
            Vec2::new(10.0, 0.0),
            "no decay mid-grab: the hand layer owns the accumulator"
        );
    }

    /// Settle-to-home is suppressed while any momentum is live (the throw
    /// owns the motion) and resumes once the coast has decayed out.
    #[test]
    fn settle_to_home_suppressed_while_momentum_live() {
        use bevy::ecs::system::RunSystemOnce;

        // Pan momentum live: distance must hold.
        let mut world = camera_world(
            FlameCamera {
                distance: 5.0,
                pan_velocity: Vec2::new(10.0, 0.0),
                ..FlameCamera::default()
            },
            FlameGrabState::default(),
        );
        world
            .run_system_once(update_flame_camera)
            .expect("update_flame_camera must run");
        assert!(
            (world.resource::<FlameCamera>().distance - 5.0).abs() < 1e-6,
            "no settle while a pan fling coasts"
        );

        // Yaw momentum live: distance must hold too.
        let mut world = camera_world(
            FlameCamera {
                distance: 5.0,
                angular_velocity: Vec2::new(0.05, 0.0),
                ..FlameCamera::default()
            },
            FlameGrabState::default(),
        );
        world
            .run_system_once(update_flame_camera)
            .expect("update_flame_camera must run");
        assert!(
            (world.resource::<FlameCamera>().distance - 5.0).abs() < 1e-6,
            "no settle while a yaw fling coasts"
        );

        // No momentum: settle resumes.
        let mut world = camera_world(
            FlameCamera {
                distance: 5.0,
                ..FlameCamera::default()
            },
            FlameGrabState::default(),
        );
        world
            .run_system_once(update_flame_camera)
            .expect("update_flame_camera must run");
        assert!(
            world.resource::<FlameCamera>().distance < 5.0,
            "settle resumes once the coast is over"
        );
    }
}
