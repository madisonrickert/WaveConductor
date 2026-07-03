//! Hand grab-and-fling orbit control for the Flame sketch, plus the idle
//! veto that keeps the sketch `Active` through a fling's coast-down and the
//! warm-amber bone overlay registration (wired in [`crate::flame::FlamePlugin::build`]).
//!
//! ## v4 lineage
//!
//! Ports v4's `_grabbingHandCount` / `_lastGrabX` / `_grabMouseOffset*`
//! interaction state (`.worktrees/v4/src/sketches/flame/index.ts:243-264`):
//! while a hand's grab strength exceeds [`GRAB_THRESHOLD`], its projected
//! window position drives the orbit camera the same way a mouse drag does
//! (see [`crate::flame::systems::camera::update_flame_camera`]), and on
//! release the last frame-to-frame delta becomes fling momentum that decays
//! in the camera system. v4 line 264 additionally routes the grab into the
//! fractal warp exactly like the mouse pointer does — [`FlameGrabState::warp_px`]
//! is the single pixel-space value both input paths write, and
//! [`crate::flame::systems::sim_params::update_flame_sim`] maps it into
//! `FlameState.warp_input` every frame regardless of who last wrote it.
//!
//! ## Data flow
//!
//! [`update_flame_hands`] runs before `update_flame_camera` each frame: it
//! gathers every [`TrackedHand`] whose [`GrabStrength`] clears
//! [`GRAB_THRESHOLD`], projects palms to window-logical pixels via
//! [`palm_to_world`] plus the world→window flip documented on that function,
//! averages them, and hands the result to the pure [`step_grab`] state
//! machine (extracted so the grab/orbit/warp math is unit-testable without
//! spinning up a `Window` or hand entities). `update_flame_sim` then reads
//! `FlameGrabState.warp_px`, only letting the pointer overwrite it while
//! `grabbing_count == 0`.
//!
//! Per-frame allocation-free: hand samples are gathered into a fixed-capacity
//! stack array, matching the pattern in `dots::systems::post_params`.

use std::f32::consts::TAU;

use bevy::prelude::*;
use wc_core::input::entity::{GrabStrength, PalmPosition, TrackedHand};
use wc_core::input::projection::palm_to_world;

use crate::flame::systems::camera::FlameCamera;

/// v4 `grabStrength > 0.5`: below this a hand's grab does not count toward
/// the average or the grabbing-hand count.
pub const GRAB_THRESHOLD: f32 = 0.5;

/// Upper bound on simultaneously-grabbing hands gathered per frame. Generous
/// for a two-hand kiosk interaction; bounds the stack buffer in
/// [`update_flame_hands`] so the per-frame gather never allocates.
const MAX_GRAB_SAMPLES: usize = 8;

/// v4's `_grabbingHandCount` / `_lastGrabX` / `_grabMouseOffset*` state, plus
/// `warp_px` — the v4 `mousePosition` analogue in window-logical pixels: the
/// single pixel-space source [`crate::flame::systems::sim_params::update_flame_sim`]
/// maps into the fractal warp every frame.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq)]
pub struct FlameGrabState {
    /// Number of hands currently grabbing (`GrabStrength > GRAB_THRESHOLD`).
    /// `0` when no hand is grabbing; read by [`flame_idle_veto`] and by
    /// `update_flame_sim` to decide whether the pointer may overwrite
    /// `warp_px` this frame.
    pub grabbing_count: usize,
    /// Grabbing-hand centroid (window-logical pixels) as of the last steady
    /// grab frame — the reference point the next frame's delta is measured
    /// against.
    pub last: Vec2,
    /// Offset between `warp_px` and the centroid, stashed on the first frame
    /// of a grab so the warp continues from wherever the pointer left it
    /// instead of snapping to the hand's raw position.
    pub mouse_offset: Vec2,
    /// The v4 `mousePosition` analogue, in window-logical pixels. Written by
    /// the pointer while `grabbing_count == 0` and by the steady-grab branch
    /// of [`step_grab`] otherwise; always the source `update_flame_sim` maps
    /// into `[-1, 1]` fractal warp coordinates.
    pub warp_px: Vec2,
}

/// Average the window-logical positions of hands whose grab strength clears
/// [`GRAB_THRESHOLD`]; hands at or below it are excluded entirely. Returns
/// `(None, 0)` when no hand is grabbing.
pub(crate) fn average_grabbing(samples: &[(Vec2, f32)]) -> (Option<Vec2>, usize) {
    let mut sum = Vec2::ZERO;
    let mut count = 0_usize;
    for &(position, grab) in samples {
        if grab > GRAB_THRESHOLD {
            sum += position;
            count += 1;
        }
    }
    if count == 0 {
        return (None, 0);
    }
    // `count` is bounded by MAX_GRAB_SAMPLES (a handful of hands); the loss
    // of precision converting it to f32 for the average is immaterial.
    #[allow(
        clippy::as_conversions,
        clippy::cast_precision_loss,
        reason = "hand count is small and bounded (<= MAX_GRAB_SAMPLES)"
    )]
    let n = count as f32;
    (Some(sum / n), count)
}

/// Pure grab/orbit/warp state transition (the v4 `_grabbingHandCount` update
/// path), extracted so it is unit-testable without a `Window` or hand
/// entities. `avg`/`grab_count` are this frame's gathered grabbing-hand
/// centroid and count (see [`average_grabbing`]); `window` is the
/// window-logical size in pixels.
///
/// - No hand grabbing (`avg` is `None`): `grabbing_count` drops to `0`.
///   Momentum already stored on `camera.angular_velocity` is left alone — it
///   decays in `update_flame_camera`, producing the fling coast.
/// - First frame of a grab, or the grabbing-hand count changed mid-grab
///   (`state.grabbing_count != grab_count`): stash the offset between the
///   current warp and the new centroid, seed `last`, and zero momentum so a
///   stale fling doesn't fight the fresh grab (v4 lines 243-252). The orbit
///   and warp are left untouched this frame — there is no prior centroid yet
///   to measure a delta against.
/// - Steady grab (count unchanged): the frame-to-frame centroid delta drives
///   the orbit directly, feeds the momentum accumulator via a `0.7/0.3`
///   blend, and the warp tracks the hand through the stashed offset (v4
///   line 264: the grab drives the fractal warp like the mouse).
pub(crate) fn step_grab(
    state: &mut FlameGrabState,
    camera: &mut FlameCamera,
    avg: Option<Vec2>,
    grab_count: usize,
    window: Vec2,
) {
    let Some(avg) = avg else {
        state.grabbing_count = 0;
        return;
    };

    if state.grabbing_count != grab_count {
        state.mouse_offset = state.warp_px - avg;
        state.last = avg;
        camera.angular_velocity = Vec2::ZERO;
        state.grabbing_count = grab_count;
        return;
    }

    // Per-axis delta (unlike the mouse drag's uniform /height split in
    // `update_flame_camera`): v4 divided grab delta by width/height
    // component-wise.
    let delta = (avg - state.last) / window * TAU;
    camera.azimuth -= delta.x;
    camera.polar -= delta.y;
    camera.angular_velocity = camera.angular_velocity * 0.7 + delta * 0.3;
    state.last = avg;
    state.warp_px = avg + state.mouse_offset;
}

/// `Update`, gated `sketch_active(AppState::Flame)`, ordered
/// `.before(update_flame_camera)`: gathers this frame's grabbing hands and
/// steps [`step_grab`].
///
/// Palms project to world-space via [`palm_to_world`] (centered origin, +y
/// up), then flip to window-logical pixels (top-left origin, +y down, the
/// same convention `PointerState` and `update_flame_sim` use) via
/// `x + w/2, h/2 - y` — the world→window formula documented on
/// `palm_to_world`.
pub fn update_flame_hands(
    window: Single<'_, '_, &Window>,
    hands: Query<'_, '_, (&PalmPosition, &GrabStrength), With<TrackedHand>>,
    mut grab_state: ResMut<'_, FlameGrabState>,
    mut camera: ResMut<'_, FlameCamera>,
) {
    let window_size = Vec2::new(window.width().max(1.0), window.height().max(1.0));

    // Fixed-capacity stack buffer: no heap allocation on this per-frame path.
    let mut samples = [(Vec2::ZERO, 0.0_f32); MAX_GRAB_SAMPLES];
    let mut n = 0_usize;
    for (palm, grab) in &hands {
        if n >= MAX_GRAB_SAMPLES {
            break;
        }
        let world = palm_to_world(palm.0, window_size);
        let window_px = Vec2::new(world.x + window_size.x * 0.5, window_size.y * 0.5 - world.y);
        samples[n] = (window_px, grab.0);
        n += 1;
    }

    let (avg, grab_count) = average_grabbing(&samples[..n]);
    step_grab(&mut grab_state, &mut camera, avg, grab_count, window_size);
}

/// Idle veto for the Flame sketch: `true` while the camera is still coasting
/// from a released fling (`angular_velocity` above a small epsilon) or a hand
/// is actively grabbing. Keeps `SketchActivity::Active` through the coast,
/// mirroring `dots::dots_idle_veto`.
///
/// Registered via `RegisterIdleVetoExt::register_idle_veto` in
/// `FlamePlugin::build`.
pub(crate) fn flame_idle_veto(world: &World) -> bool {
    let coasting = world
        .get_resource::<FlameCamera>()
        .is_some_and(|camera| camera.angular_velocity.length() > 1e-4);
    let grabbing = world
        .get_resource::<FlameGrabState>()
        .is_some_and(|state| state.grabbing_count > 0);
    coasting || grabbing
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    const WINDOW: Vec2 = Vec2::new(1280.0, 720.0);

    /// (a) Averaging only includes hands above `GRAB_THRESHOLD`.
    #[test]
    fn average_grabbing_only_includes_hands_above_threshold() {
        let samples = [(Vec2::new(0.0, 0.0), 0.9), (Vec2::new(100.0, 100.0), 0.3)];
        let (avg, count) = average_grabbing(&samples);
        assert_eq!(count, 1);
        assert_eq!(avg, Some(Vec2::new(0.0, 0.0)));
    }

    /// No hand above threshold: no average, count 0.
    #[test]
    fn average_grabbing_none_when_all_below_threshold() {
        let samples = [(Vec2::new(0.0, 0.0), 0.1), (Vec2::new(50.0, 50.0), 0.49)];
        let (avg, count) = average_grabbing(&samples);
        assert_eq!(count, 0);
        assert_eq!(avg, None);
    }

    /// (b) First-grab-frame branch: stashes the warp/centroid offset, seeds
    /// `last`, and zeroes momentum. No orbit/warp movement yet.
    #[test]
    fn first_grab_frame_stashes_offset_and_zeroes_momentum() {
        let mut state = FlameGrabState {
            warp_px: Vec2::new(0.2, -0.1),
            ..FlameGrabState::default()
        };
        let mut camera = FlameCamera {
            angular_velocity: Vec2::new(0.05, 0.02),
            ..FlameCamera::default()
        };
        let avg = Vec2::new(640.0, 360.0);
        let az0 = camera.azimuth;
        let polar0 = camera.polar;

        step_grab(&mut state, &mut camera, Some(avg), 1, WINDOW);

        assert_eq!(state.grabbing_count, 1);
        assert_eq!(state.last, avg);
        assert_eq!(state.mouse_offset, Vec2::new(0.2, -0.1) - avg);
        assert_eq!(camera.angular_velocity, Vec2::ZERO);
        // Orbit pose untouched on the stash frame.
        assert!((camera.azimuth - az0).abs() < 1e-6);
        assert!((camera.polar - polar0).abs() < 1e-6);
    }

    /// (c) Steady-grab branch: `angular_velocity = 0.7 * old + 0.3 * delta`,
    /// azimuth/polar move by `-delta`, and `warp_px` tracks the hand through
    /// the stashed offset.
    #[test]
    fn steady_grab_updates_camera_and_warp() {
        let mut state = FlameGrabState {
            grabbing_count: 1,
            last: Vec2::new(640.0, 360.0),
            mouse_offset: Vec2::new(10.0, -5.0),
            warp_px: Vec2::ZERO,
        };
        let mut camera = FlameCamera {
            angular_velocity: Vec2::new(0.1, 0.0),
            ..FlameCamera::default()
        };
        let az0 = camera.azimuth;
        let polar0 = camera.polar;
        let avg = Vec2::new(660.0, 360.0); // moved 20px in x only

        step_grab(&mut state, &mut camera, Some(avg), 1, WINDOW);

        let expected_delta = Vec2::new(20.0, 0.0) / WINDOW * TAU;
        assert!((camera.azimuth - (az0 - expected_delta.x)).abs() < 1e-6);
        assert!((camera.polar - (polar0 - expected_delta.y)).abs() < 1e-6);
        let expected_velocity = Vec2::new(0.1, 0.0) * 0.7 + expected_delta * 0.3;
        assert!((camera.angular_velocity - expected_velocity).length() < 1e-6);
        assert_eq!(state.last, avg);
        assert_eq!(state.warp_px, avg + Vec2::new(10.0, -5.0));
    }

    /// Count changing mid-grab (a second hand joins) re-triggers the stash
    /// branch instead of computing a (nonsensical) delta against the old
    /// single-hand centroid.
    #[test]
    fn hand_count_change_mid_grab_restashes() {
        let mut state = FlameGrabState {
            grabbing_count: 1,
            last: Vec2::new(640.0, 360.0),
            mouse_offset: Vec2::new(1.0, 1.0),
            warp_px: Vec2::new(0.5, 0.5),
        };
        let mut camera = FlameCamera {
            angular_velocity: Vec2::new(0.2, 0.2),
            ..FlameCamera::default()
        };
        let avg = Vec2::new(200.0, 200.0); // a second hand jumped the centroid

        step_grab(&mut state, &mut camera, Some(avg), 2, WINDOW);

        assert_eq!(state.grabbing_count, 2);
        assert_eq!(state.last, avg);
        assert_eq!(camera.angular_velocity, Vec2::ZERO);
    }

    /// All hands released: `grabbing_count` drops to 0; momentum already on
    /// the camera is left for `update_flame_camera` to decay.
    #[test]
    fn release_zeroes_grabbing_count_but_keeps_momentum() {
        let mut state = FlameGrabState {
            grabbing_count: 1,
            ..FlameGrabState::default()
        };
        let mut camera = FlameCamera {
            angular_velocity: Vec2::new(0.03, 0.01),
            ..FlameCamera::default()
        };

        step_grab(&mut state, &mut camera, None, 0, WINDOW);

        assert_eq!(state.grabbing_count, 0);
        assert_eq!(camera.angular_velocity, Vec2::new(0.03, 0.01));
    }

    /// (d) `flame_idle_veto`: false at rest, true with momentum or a grab.
    #[test]
    fn idle_veto_false_at_rest_true_with_momentum_or_grab() {
        let mut world = World::new();
        world.insert_resource(FlameCamera::default());
        world.insert_resource(FlameGrabState::default());
        assert!(!flame_idle_veto(&world));

        world.insert_resource(FlameCamera {
            angular_velocity: Vec2::new(0.01, 0.0),
            ..FlameCamera::default()
        });
        assert!(flame_idle_veto(&world), "coasting fling must veto idle");

        world.insert_resource(FlameCamera::default());
        world.insert_resource(FlameGrabState {
            grabbing_count: 1,
            ..FlameGrabState::default()
        });
        assert!(flame_idle_veto(&world), "active grab must veto idle");
    }

    /// World-level: `update_flame_hands` counts a single grabbing hand.
    /// Mirrors `drive_dots_audio_raises_envelope_from_hand_alone`.
    #[test]
    fn update_flame_hands_counts_one_grabbing_hand() {
        let mut world = World::new();
        world.insert_resource(FlameCamera::default());
        world.insert_resource(FlameGrabState::default());
        world.spawn(Window::default());
        world.spawn((
            TrackedHand,
            Transform::default(),
            Visibility::default(),
            PalmPosition(Vec3::new(0.0, 195.0, 200.0)),
            GrabStrength(0.9),
        ));

        world
            .run_system_once(update_flame_hands)
            .expect("update_flame_hands must run");

        assert_eq!(world.resource::<FlameGrabState>().grabbing_count, 1);
    }
}
