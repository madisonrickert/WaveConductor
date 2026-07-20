//! VR-style "grab space" hand navigation for the Flame sketch (prior art:
//! Google Earth VR grip nav), plus the idle veto that keeps the sketch
//! `Active` through a fling's coast-down and the warm-amber bone overlay
//! registration (wired in [`crate::flame::FlamePlugin::build`]).
//!
//! ## Interaction model
//!
//! Party-guest ergonomics: grabbing the scene moves the scene, the way you
//! would drag a physical map floating in front of you.
//!
//! - **One grabbing hand = PAN.** The content follows the hand ~1:1 in screen
//!   space ([`FlameCamera::pan_by_pixels`]'s "content follows hand" sign
//!   convention). Releasing with motion leaves a pan fling on
//!   [`FlameCamera::pan_velocity`] that coasts and decays in
//!   [`crate::flame::systems::camera::update_flame_camera`], like throwing a
//!   map. One hand never orbits.
//! - **Two grabbing hands = ZOOM + ROTATE + PAN about the grip.** The spread
//!   ratio against the engage-time anchor drives zoom
//!   ([`FlameCamera::set_distance_clamped`], exponentiated by
//!   [`FlameSettings::two_hand_zoom_gamma`]); the change in the *angle* of the
//!   inter-hand line yaws the camera azimuth (turntable rotate, scaled by
//!   [`FlameSettings::two_hand_rotate_gain`], signed so the scene visually
//!   rotates with the hands' twist); and the midpoint delta pans. Releasing
//!   carries a modest yaw momentum from the recent twist velocity, but never a
//!   pan fling and never a zoom momentum.
//! - **Hysteresis.** A hand engages at [`GRAB_ENGAGE_THRESHOLD`] and stays
//!   engaged until it drops below [`GRAB_RELEASE_THRESHOLD`] (per-hand state
//!   in `GrabEngagement`), so a wavering grip near the boundary does not
//!   stutter between pan and zoom/rotate modes. (Same idea as wc-core's
//!   `input::button` press/release thresholds, kept local because the gather
//!   needs per-hand *positions* alongside the engagement bit.)
//! - **Re-anchor on any hand-count change.** A 1↔2 transition re-stashes
//!   every reference (centroid, spread, distance, line angle, warp offset)
//!   and kills live momentum, so transitions never jump or pop.
//!
//! ## v4 lineage
//!
//! The state layout ports v4's `_grabbingHandCount` / `_lastGrabX` /
//! `_grabMouseOffset*` interaction bookkeeping
//! (`.worktrees/v4/src/sketches/flame/index.ts:243-264`), though the gesture
//! mapping has moved on from v4's grab-to-orbit (which party guests read as
//! "broken pan"). v4 line 264's grab→warp routing is retained exactly:
//! [`FlameGrabState::warp_px`] is the single pixel-space value both input
//! paths write, and [`crate::flame::systems::sim_params::update_flame_sim`]
//! maps it into `FlameState.warp_input` every frame regardless of who last
//! wrote it.
//!
//! ## Data flow
//!
//! [`update_flame_hands`] runs before `update_flame_camera` each frame: it
//! projects palms to window-logical pixels via [`palm_to_world`] plus the
//! world→window flip documented on that function, folds the engaged hands
//! into a `GrabGather` (centroid, count, spread, line angle — see
//! `gather_grabbing`, which also steps the per-hand hysteresis), and hands it
//! to the pure `step_grab` state machine (extracted so the pan/zoom/rotate/
//! warp math is unit-testable without spinning up a `Window` or hand
//! entities). `update_flame_sim` then reads `FlameGrabState.warp_px`, only
//! letting the pointer overwrite it while `grabbing_count == 0`; both grab
//! branches keep writing it from the centroid.
//!
//! Per-frame allocation-free: hand samples are gathered into a fixed-capacity
//! stack array, matching the pattern in `dots::systems::post_params`.

use std::f32::consts::{FRAC_PI_2, PI};

use bevy::prelude::*;
use wc_core::input::entity::{GrabStrength, PalmPosition, TrackedHand};
use wc_core::input::projection::palm_to_world;

use crate::flame::settings::FlameSettings;
use crate::flame::systems::camera::FlameCamera;

/// Grab strength above which a *disengaged* hand engages. Deliberately higher
/// than [`GRAB_RELEASE_THRESHOLD`]: the hysteresis gap keeps a wavering grip
/// from stuttering between the one-hand pan and two-hand zoom/rotate modes.
pub const GRAB_ENGAGE_THRESHOLD: f32 = 0.7;

/// Grab strength below which an *engaged* hand releases. See
/// [`GRAB_ENGAGE_THRESHOLD`] for the hysteresis rationale.
pub const GRAB_RELEASE_THRESHOLD: f32 = 0.45;

/// Upper bound on simultaneously-tracked hands gathered per frame. Generous
/// for a two-hand kiosk interaction; bounds the stack buffer in
/// [`update_flame_hands`] and the [`GrabEngagement`] table so the per-frame
/// gather never allocates.
const MAX_GRAB_SAMPLES: usize = 8;

/// Momentum blend factor on the previous frame's velocity (the v4 fling
/// idiom): `v = OLD * v_prev + (1 - OLD) * v_frame`.
const MOMENTUM_BLEND_OLD: f32 = 0.7;
/// Momentum blend factor on this frame's velocity sample. See
/// [`MOMENTUM_BLEND_OLD`].
const MOMENTUM_BLEND_NEW: f32 = 0.3;

/// Per-hand grab hysteresis memory, rebuilt every frame from the hands
/// present in that frame's samples (a hand that disappears from tracking
/// simply drops out). Fixed-capacity so [`FlameGrabState`] stays `Copy` and
/// the per-frame path stays allocation-free.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GrabEngagement {
    /// `(hand key, engaged)` pairs from the most recent gather. Keys are
    /// opaque per-hand identifiers (`Entity::to_bits` in production).
    entries: [(u64, bool); MAX_GRAB_SAMPLES],
    /// Number of live entries in `entries`.
    len: usize,
}

impl Default for GrabEngagement {
    fn default() -> Self {
        Self {
            entries: [(0, false); MAX_GRAB_SAMPLES],
            len: 0,
        }
    }
}

impl GrabEngagement {
    /// Was this hand engaged as of the previous gather? Unknown hands (newly
    /// tracked) start disengaged, so they must cross the higher
    /// [`GRAB_ENGAGE_THRESHOLD`] to join.
    fn was_engaged(&self, key: u64) -> bool {
        self.entries[..self.len]
            .iter()
            .any(|&(k, engaged)| k == key && engaged)
    }
}

/// One step of the per-hand press/release hysteresis: an engaged hand stays
/// engaged until it drops below [`GRAB_RELEASE_THRESHOLD`]; a disengaged hand
/// must exceed [`GRAB_ENGAGE_THRESHOLD`] to engage.
pub(crate) fn grab_hysteresis(was_engaged: bool, strength: f32) -> bool {
    if was_engaged {
        strength > GRAB_RELEASE_THRESHOLD
    } else {
        strength > GRAB_ENGAGE_THRESHOLD
    }
}

/// The grab interaction state: v4's `_grabbingHandCount` / `_lastGrabX` /
/// `_grabMouseOffset*` bookkeeping plus `warp_px` — the v4 `mousePosition`
/// analogue in window-logical pixels: the single pixel-space source
/// [`crate::flame::systems::sim_params::update_flame_sim`] maps into the
/// fractal warp every frame — and the two-hand zoom/rotate anchors.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq)]
pub struct FlameGrabState {
    /// Number of hands currently engaged (per-hand hysteresis, see
    /// `grab_hysteresis`). `0` when no hand is grabbing; read by
    /// `flame_idle_veto` and by `update_flame_sim` to decide whether the
    /// pointer may overwrite `warp_px` this frame.
    pub grabbing_count: usize,
    /// Engaged-hand centroid (window-logical pixels) as of the last steady
    /// grab frame — the reference point the next frame's delta is measured
    /// against.
    pub last: Vec2,
    /// Offset between `warp_px` and the centroid, stashed on the first frame
    /// of a grab so the warp continues from wherever the pointer left it
    /// instead of snapping to the hand's raw position.
    pub mouse_offset: Vec2,
    /// The v4 `mousePosition` analogue, in window-logical pixels. Written by
    /// the pointer while `grabbing_count == 0` and by the steady-grab branch
    /// of `step_grab` otherwise; always the source `update_flame_sim` maps
    /// into `[-1, 1]` fractal warp coordinates.
    pub warp_px: Vec2,
    /// Inter-hand spread (window px, floored to 1.0) stashed when the second
    /// hand engaged — the denominator anchor of the zoom ratio.
    pub anchor_spread: f32,
    /// Camera distance stashed when the second hand engaged — the numerator
    /// anchor of the zoom ratio (anchor-based zoom accumulates no drift).
    pub anchor_distance: f32,
    /// Window-coordinate angle of the inter-hand line (radians, `atan2(dy, dx)`
    /// with +y down) as of the last steady two-hand frame — the reference the
    /// next frame's twist delta is measured against. Compared modulo π (the
    /// line is undirected; see `wrap_line_angle_delta`).
    pub last_line_angle: f32,
    /// Per-hand grab hysteresis memory (see [`GrabEngagement`]).
    pub(crate) engagement: GrabEngagement,
}

/// One frame's gathered engaged-hand geometry: hands whose hysteresis state
/// is engaged contribute to the centroid; the first two also define `spread`
/// and `line_angle` (`MAX_HANDS` is 2 upstream, so "first two" is exhaustive).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GrabGather {
    /// Mean window-logical position of all engaged hands (for two hands, the
    /// grip midpoint the pan gesture drags).
    pub centroid: Vec2,
    /// Number of engaged hands (drives the 0/1/2 interaction mode).
    pub count: usize,
    /// Window-pixel distance between the first two engaged hands; `0.0` when
    /// fewer than two engage.
    pub spread: f32,
    /// Window-coordinate angle (radians, `atan2(dy, dx)`, +y down) of the
    /// line from the first engaged hand to the second; `0.0` when fewer than
    /// two engage. The gather's hand order is not guaranteed stable across
    /// frames, so consumers must compare angles modulo π (undirected line —
    /// see [`wrap_line_angle_delta`]).
    pub line_angle: f32,
}

/// Gather the engaged hands out of this frame's samples, stepping each hand's
/// grab hysteresis (`engagement` is rebuilt from this frame's hands; absent
/// hands drop out and re-enter disengaged). `samples` is
/// `(hand key, window-px position, grab strength)` per tracked hand. Returns
/// `None` when no hand is engaged.
pub(crate) fn gather_grabbing(
    samples: &[(u64, Vec2, f32)],
    engagement: &mut GrabEngagement,
) -> Option<GrabGather> {
    let previous = *engagement;
    engagement.len = 0;

    let mut sum = Vec2::ZERO;
    let mut count = 0_usize;
    let mut first_two = [Vec2::ZERO; 2];
    for &(key, position, strength) in samples.iter().take(MAX_GRAB_SAMPLES) {
        let engaged = grab_hysteresis(previous.was_engaged(key), strength);
        engagement.entries[engagement.len] = (key, engaged);
        engagement.len += 1;
        if engaged {
            if count < 2 {
                first_two[count] = position;
            }
            sum += position;
            count += 1;
        }
    }
    if count == 0 {
        return None;
    }
    // `count` is bounded by MAX_GRAB_SAMPLES (a handful of hands); the loss
    // of precision converting it to f32 for the average is immaterial.
    #[allow(
        clippy::as_conversions,
        clippy::cast_precision_loss,
        reason = "hand count is small and bounded (<= MAX_GRAB_SAMPLES)"
    )]
    let n = count as f32;
    let (spread, line_angle) = if count >= 2 {
        let line = first_two[1] - first_two[0];
        (line.length(), line.y.atan2(line.x))
    } else {
        (0.0, 0.0)
    };
    Some(GrabGather {
        centroid: sum / n,
        count,
        spread,
        line_angle,
    })
}

/// Wrap an inter-hand *line* angle delta into `(-PI/2, PI/2]`. The line is
/// undirected (the gather's hand order can swap between frames, flipping the
/// raw `atan2` by π), so twist is only meaningful modulo π; per-frame twists
/// are far smaller than π/2, so the wrap recovers the true delta and makes a
/// hand-order swap read as zero twist instead of a half-turn jump.
pub(crate) fn wrap_line_angle_delta(delta: f32) -> f32 {
    let wrapped = delta.rem_euclid(PI);
    if wrapped > FRAC_PI_2 {
        wrapped - PI
    } else {
        wrapped
    }
}

/// Pure grab-space navigation state transition, extracted so it is
/// unit-testable without a `Window` or hand entities. `gather` is this
/// frame's engaged-hand geometry (see [`gather_grabbing`]); `window` is the
/// window-logical size in pixels; `zoom_gamma` / `pan_sensitivity` /
/// `rotate_gain` are [`FlameSettings::two_hand_zoom_gamma`],
/// [`FlameSettings::hand_pan_sensitivity`], and
/// [`FlameSettings::two_hand_rotate_gain`].
///
/// - No hand engaged (`gather` is `None`): `grabbing_count` drops to `0`.
///   Momentum already stored on the camera ([`FlameCamera::angular_velocity`]
///   from two-hand twist, [`FlameCamera::pan_velocity`] from one-hand pan) is
///   left alone — it decays in `update_flame_camera`, producing the fling
///   coast.
/// - First frame of a grab, or the engaged-hand count changed mid-grab
///   (`state.grabbing_count != gather.count`): stash the offset between the
///   current warp and the new centroid, seed `last`, and zero both momenta so
///   a stale fling doesn't fight the fresh grip. When the new count is two
///   hands, also stash `anchor_spread` / `anchor_distance` / `last_line_angle`
///   — the references zoom and twist are measured against. The camera is left
///   untouched this frame — there is no prior centroid yet to measure a delta
///   against, so transitions never jump.
/// - Steady one-hand grab: PAN. The frame-to-frame centroid delta drives
///   [`FlameCamera::pan_by_pixels`] (content follows the hand ~1:1 in screen
///   space) and feeds `pan_velocity` via the `0.7/0.3` momentum blend, so
///   release throws the map. Angular momentum is held at zero — one hand
///   never orbits. The warp tracks the hand through the stashed offset (v4
///   line 264: the grab drives the fractal warp like the mouse).
/// - Steady two-hand grab: ZOOM + ROTATE + PAN about the grip. The current
///   spread against `anchor_spread` gives a ratio that (exponentiated by
///   `zoom_gamma`) scales `anchor_distance` into the new camera distance —
///   anchor-based, so spreading back to the engage spread returns exactly to
///   the engage distance, with no per-frame drift. The twist of the
///   inter-hand line (wrapped modulo π, see [`wrap_line_angle_delta`]) yaws
///   `azimuth` by `rotate_gain * twist` — window angles grow clockwise on
///   screen (+y down) and a positive azimuth step also reads as clockwise
///   scene rotation, so the scene visually rotates with the hands. The yaw
///   feeds `angular_velocity.x` via the momentum blend (negated: the coast in
///   `update_flame_camera` applies `azimuth -= v.x`), so releasing out of a
///   twist keeps a modest tactile spin; `angular_velocity.y` and
///   `pan_velocity` are held at zero every two-hand frame, so releasing out
///   of two-hand mode never pan-flings or tilt-flings, and zoom carries no
///   momentum at all.
pub(crate) fn step_grab(
    state: &mut FlameGrabState,
    camera: &mut FlameCamera,
    gather: Option<GrabGather>,
    window: Vec2,
    zoom_gamma: f32,
    pan_sensitivity: f32,
    rotate_gain: f32,
) {
    let Some(gather) = gather else {
        state.grabbing_count = 0;
        return;
    };

    if state.grabbing_count != gather.count {
        state.mouse_offset = state.warp_px - gather.centroid;
        state.last = gather.centroid;
        camera.angular_velocity = Vec2::ZERO;
        camera.pan_velocity = Vec2::ZERO;
        state.grabbing_count = gather.count;
        if gather.count >= 2 {
            // The `.max(1.0)` px floor guards the ratio against a degenerate
            // zero spread (overlapping palms).
            state.anchor_spread = gather.spread.max(1.0);
            state.anchor_distance = camera.distance;
            state.last_line_angle = gather.line_angle;
        }
        return;
    }

    let delta_px = gather.centroid - state.last;
    if gather.count >= 2 {
        // Two-hand mode: spread ratio → zoom (anchor-based, no drift), line
        // twist → azimuth yaw (with a modest release momentum), midpoint
        // delta → pan. Pan momentum stays dead so releasing never pan-flings.
        let ratio = state.anchor_spread / gather.spread.max(1.0);
        camera.set_distance_clamped(state.anchor_distance * ratio.powf(zoom_gamma));
        let twist = wrap_line_angle_delta(gather.line_angle - state.last_line_angle);
        let yaw = rotate_gain * twist;
        camera.azimuth += yaw;
        // The coast in `update_flame_camera` applies `azimuth -= v.x * dt*60`,
        // so continuing this frame's `+yaw` motion needs `v.x` blended toward
        // `-yaw`. `v.y` stays zero: twist never tilts, so release never
        // polar-flings.
        camera.angular_velocity.x =
            camera.angular_velocity.x * MOMENTUM_BLEND_OLD - yaw * MOMENTUM_BLEND_NEW;
        camera.angular_velocity.y = 0.0;
        camera.pan_velocity = Vec2::ZERO;
        camera.pan_by_pixels(delta_px, window, pan_sensitivity);
        state.last_line_angle = gather.line_angle;
    } else {
        // One-hand mode: pan only — content follows the hand ~1:1 in screen
        // space, and the delta feeds the pan fling for a throw on release.
        // Angular momentum is held at zero: one hand never orbits.
        camera.pan_by_pixels(delta_px, window, pan_sensitivity);
        camera.pan_velocity =
            camera.pan_velocity * MOMENTUM_BLEND_OLD + delta_px * MOMENTUM_BLEND_NEW;
        camera.angular_velocity = Vec2::ZERO;
    }
    state.last = gather.centroid;
    state.warp_px = gather.centroid + state.mouse_offset;
}

/// `Update`, gated `sketch_active(AppState::Flame)`, ordered
/// `.before(update_flame_camera)`: gathers this frame's hands (stepping the
/// per-hand grab hysteresis) and steps `step_grab`.
///
/// Palms project to world-space via [`palm_to_world`] (centered origin, +y
/// up), then flip to window-logical pixels (top-left origin, +y down, the
/// same convention `PointerState` and `update_flame_sim` use) via
/// `x + w/2, h/2 - y` — the world→window formula documented on
/// `palm_to_world`. Each hand's `Entity` bits key its hysteresis slot.
pub fn update_flame_hands(
    window: Single<'_, '_, &Window>,
    hands: Query<'_, '_, (Entity, &PalmPosition, &GrabStrength), With<TrackedHand>>,
    mut grab_state: ResMut<'_, FlameGrabState>,
    mut camera: ResMut<'_, FlameCamera>,
    settings: Res<'_, FlameSettings>,
) {
    let window_size = Vec2::new(window.width().max(1.0), window.height().max(1.0));

    // Fixed-capacity stack buffer: no heap allocation on this per-frame path.
    let mut samples = [(0_u64, Vec2::ZERO, 0.0_f32); MAX_GRAB_SAMPLES];
    let mut n = 0_usize;
    for (entity, palm, grab) in &hands {
        if n >= MAX_GRAB_SAMPLES {
            break;
        }
        let world = palm_to_world(palm.0, window_size);
        let window_px = Vec2::new(world.x + window_size.x * 0.5, window_size.y * 0.5 - world.y);
        samples[n] = (entity.to_bits(), window_px, grab.0);
        n += 1;
    }

    let gather = gather_grabbing(&samples[..n], &mut grab_state.engagement);
    step_grab(
        &mut grab_state,
        &mut camera,
        gather,
        window_size,
        settings.two_hand_zoom_gamma,
        settings.hand_pan_sensitivity,
        settings.two_hand_rotate_gain,
    );
}

/// Idle veto for the Flame sketch: `true` while the camera is still coasting
/// from a released fling (angular *or* pan momentum above a small epsilon) or
/// a hand is actively grabbing. Keeps `SketchActivity::Active` through the
/// coast, mirroring `dots::dots_idle_veto`.
///
/// Registered via `RegisterIdleVetoExt::register_idle_veto` in
/// `FlamePlugin::build`.
pub(crate) fn flame_idle_veto(world: &World) -> bool {
    let coasting = world.get_resource::<FlameCamera>().is_some_and(|camera| {
        camera.angular_velocity.length() > 1e-4 || camera.pan_velocity.length() > 1e-4
    });
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

    /// Gather with fresh (all-disengaged) hysteresis state, for tests that
    /// exercise the geometry rather than the hysteresis itself.
    fn gather_fresh(samples: &[(u64, Vec2, f32)]) -> Option<GrabGather> {
        gather_grabbing(samples, &mut GrabEngagement::default())
    }

    // ── Hysteresis ─────────────────────────────────────────────────────────

    /// A disengaged hand engages only above `GRAB_ENGAGE_THRESHOLD`; an
    /// engaged hand releases only below `GRAB_RELEASE_THRESHOLD`.
    #[test]
    fn grab_hysteresis_engage_and_release_thresholds() {
        assert!(!grab_hysteresis(false, 0.6), "0.6 must not engage");
        assert!(grab_hysteresis(false, 0.8), "0.8 must engage");
        assert!(grab_hysteresis(true, 0.6), "0.6 must hold an engaged grip");
        assert!(grab_hysteresis(true, 0.5), "0.5 must hold an engaged grip");
        assert!(!grab_hysteresis(true, 0.4), "0.4 must release");
    }

    /// A wavering grip in the hysteresis band (`0.45..0.7`) holds its
    /// engagement across frames instead of stuttering.
    #[test]
    fn gather_hysteresis_holds_wavering_grip() {
        let mut engagement = GrabEngagement::default();
        let position = Vec2::new(100.0, 100.0);

        // Below engage: never joins.
        assert!(gather_grabbing(&[(1, position, 0.6)], &mut engagement).is_none());
        // Crosses engage: joins.
        assert!(gather_grabbing(&[(1, position, 0.8)], &mut engagement).is_some());
        // Sags into the band: stays engaged.
        assert!(gather_grabbing(&[(1, position, 0.55)], &mut engagement).is_some());
        // Drops below release: lets go.
        assert!(gather_grabbing(&[(1, position, 0.4)], &mut engagement).is_none());
        // Back in the band from below: still released (must re-cross 0.7).
        assert!(gather_grabbing(&[(1, position, 0.6)], &mut engagement).is_none());
    }

    /// A hand that disappears from tracking re-enters disengaged: its old
    /// engagement does not survive absence.
    #[test]
    fn gather_hysteresis_absent_hand_reenters_disengaged() {
        let mut engagement = GrabEngagement::default();
        let position = Vec2::new(100.0, 100.0);
        assert!(gather_grabbing(&[(1, position, 0.9)], &mut engagement).is_some());
        // Hand 1 vanishes for a frame.
        assert!(gather_grabbing(&[], &mut engagement).is_none());
        // Re-appears in the band: must re-cross the engage threshold.
        assert!(gather_grabbing(&[(1, position, 0.6)], &mut engagement).is_none());
    }

    // ── Gather geometry ────────────────────────────────────────────────────

    /// Gathering only includes hands above the engage threshold.
    #[test]
    fn gather_grabbing_only_includes_engaged_hands() {
        let samples = [
            (1, Vec2::new(0.0, 0.0), 0.9),
            (2, Vec2::new(100.0, 100.0), 0.3),
        ];
        let gather = gather_fresh(&samples).expect("one engaged hand");
        assert_eq!(gather.count, 1);
        assert_eq!(gather.centroid, Vec2::new(0.0, 0.0));
    }

    /// No hand above the engage threshold: no gather.
    #[test]
    fn gather_grabbing_none_when_all_below_threshold() {
        let samples = [
            (1, Vec2::new(0.0, 0.0), 0.1),
            (2, Vec2::new(50.0, 50.0), 0.69),
        ];
        assert!(gather_fresh(&samples).is_none());
    }

    /// Spread and line angle come from the first two engaged hands.
    #[test]
    fn gather_spread_and_angle_from_first_two_engaged() {
        let samples = [
            (1, Vec2::new(100.0, 300.0), 0.9),
            (2, Vec2::new(400.0, 600.0), 0.8),
        ];
        let gather = gather_fresh(&samples).expect("two engaged hands");
        assert_eq!(gather.count, 2);
        assert!((gather.spread - 300.0 * std::f32::consts::SQRT_2).abs() < 1e-3);
        assert_eq!(gather.centroid, Vec2::new(250.0, 450.0));
        // Line from (100,300) to (400,600): dy = dx = 300 → 45° in +y-down coords.
        assert!((gather.line_angle - std::f32::consts::FRAC_PI_4).abs() < 1e-5);
    }

    // ── Line-angle wrap ────────────────────────────────────────────────────

    /// Small twists pass through; a hand-order swap (π offset) reads as zero.
    #[test]
    fn wrap_line_angle_delta_handles_swap_and_small_twists() {
        assert!((wrap_line_angle_delta(0.1) - 0.1).abs() < 1e-6);
        assert!((wrap_line_angle_delta(-0.1) - (-0.1)).abs() < 1e-6);
        assert!(wrap_line_angle_delta(PI).abs() < 1e-6, "swap reads as zero");
        assert!(
            (wrap_line_angle_delta(PI + 0.1) - 0.1).abs() < 1e-5,
            "swap plus a small twist keeps the twist"
        );
        assert!(
            (wrap_line_angle_delta(-PI - 0.1) - (-0.1)).abs() < 1e-5,
            "negative swap plus twist keeps the twist"
        );
    }

    // ── step_grab: engage / re-anchor ──────────────────────────────────────

    /// First-grab-frame branch: stashes the warp/centroid offset, seeds
    /// `last`, and zeroes both momenta. No camera movement yet.
    #[test]
    fn first_grab_frame_stashes_offset_and_zeroes_momentum() {
        let mut state = FlameGrabState {
            warp_px: Vec2::new(0.2, -0.1),
            ..FlameGrabState::default()
        };
        let mut camera = FlameCamera {
            angular_velocity: Vec2::new(0.05, 0.02),
            pan_velocity: Vec2::new(3.0, -2.0),
            ..FlameCamera::default()
        };
        let avg = Vec2::new(640.0, 360.0);
        let gather = GrabGather {
            centroid: avg,
            count: 1,
            spread: 0.0,
            line_angle: 0.0,
        };
        let az0 = camera.azimuth;
        let polar0 = camera.polar;

        step_grab(&mut state, &mut camera, Some(gather), WINDOW, 1.0, 1.0, 1.0);

        assert_eq!(state.grabbing_count, 1);
        assert_eq!(state.last, avg);
        assert_eq!(state.mouse_offset, Vec2::new(0.2, -0.1) - avg);
        assert_eq!(camera.angular_velocity, Vec2::ZERO);
        assert_eq!(camera.pan_velocity, Vec2::ZERO);
        // Pose untouched on the stash frame.
        assert!((camera.azimuth - az0).abs() < 1e-6);
        assert!((camera.polar - polar0).abs() < 1e-6);
        assert_eq!(camera.target, Vec3::ZERO);
    }

    /// Count changing mid-grab (a second hand joins) re-triggers the stash
    /// branch instead of computing a (nonsensical) delta against the old
    /// single-hand centroid, and stashes the two-hand anchors.
    #[test]
    fn hand_count_change_mid_grab_restashes() {
        let mut state = FlameGrabState {
            grabbing_count: 1,
            last: Vec2::new(640.0, 360.0),
            mouse_offset: Vec2::new(1.0, 1.0),
            warp_px: Vec2::new(0.5, 0.5),
            ..FlameGrabState::default()
        };
        let mut camera = FlameCamera {
            angular_velocity: Vec2::new(0.2, 0.2),
            pan_velocity: Vec2::new(4.0, 0.0),
            ..FlameCamera::default()
        };
        let avg = Vec2::new(200.0, 200.0); // a second hand jumped the centroid
        let gather = GrabGather {
            centroid: avg,
            count: 2,
            spread: 300.0,
            line_angle: 0.4,
        };

        step_grab(&mut state, &mut camera, Some(gather), WINDOW, 1.0, 1.0, 1.0);

        assert_eq!(state.grabbing_count, 2);
        assert_eq!(state.last, avg);
        assert_eq!(camera.angular_velocity, Vec2::ZERO);
        assert_eq!(camera.pan_velocity, Vec2::ZERO);
        assert!((state.anchor_spread - 300.0).abs() < 1e-6);
        assert!((state.last_line_angle - 0.4).abs() < 1e-6);
    }

    /// Engaging a second hand stashes the zoom anchors and moves nothing.
    #[test]
    fn two_hand_engage_stashes_anchors_and_moves_nothing() {
        let mut state = FlameGrabState {
            grabbing_count: 1,
            ..FlameGrabState::default()
        };
        let mut camera = FlameCamera::default();
        let d0 = camera.distance;
        let gather = GrabGather {
            centroid: Vec2::new(640.0, 360.0),
            count: 2,
            spread: 400.0,
            line_angle: 0.25,
        };

        step_grab(&mut state, &mut camera, Some(gather), WINDOW, 1.0, 1.0, 1.0);

        assert_eq!(state.grabbing_count, 2);
        assert!((state.anchor_spread - 400.0).abs() < 1e-6);
        assert!((state.anchor_distance - d0).abs() < 1e-6);
        assert!((state.last_line_angle - 0.25).abs() < 1e-6);
        assert!(
            (camera.distance - d0).abs() < 1e-6,
            "no zoom on the stash frame"
        );
        assert_eq!(camera.target, Vec3::ZERO, "no pan on the stash frame");
    }

    // ── step_grab: one-hand pan ────────────────────────────────────────────

    /// Steady one-hand grab PANS (content follows the hand: +x hand motion
    /// pans the target -X), never orbits, blends pan momentum, and the warp
    /// tracks the hand through the stashed offset.
    #[test]
    fn steady_one_hand_pans_without_orbiting() {
        let mut state = FlameGrabState {
            grabbing_count: 1,
            last: Vec2::new(640.0, 360.0),
            mouse_offset: Vec2::new(10.0, -5.0),
            warp_px: Vec2::ZERO,
            ..FlameGrabState::default()
        };
        let mut camera = FlameCamera {
            pan_velocity: Vec2::new(10.0, 0.0),
            angular_velocity: Vec2::new(0.1, 0.0), // stale spin must be suppressed
            ..FlameCamera::default()
        };
        let az0 = camera.azimuth;
        let polar0 = camera.polar;
        let avg = Vec2::new(660.0, 360.0); // moved 20px in x only
        let gather = GrabGather {
            centroid: avg,
            count: 1,
            spread: 0.0,
            line_angle: 0.0,
        };

        step_grab(&mut state, &mut camera, Some(gather), WINDOW, 1.0, 1.0, 1.0);

        assert!(
            camera.target.x < 0.0,
            "content follows hand: +x motion pans target -X, got {}",
            camera.target.x
        );
        assert!((camera.azimuth - az0).abs() < 1e-6, "one hand never orbits");
        assert!((camera.polar - polar0).abs() < 1e-6, "one hand never tilts");
        assert_eq!(
            camera.angular_velocity,
            Vec2::ZERO,
            "no angular momentum from a one-hand pan"
        );
        let expected_pan_velocity = Vec2::new(10.0, 0.0) * 0.7 + Vec2::new(20.0, 0.0) * 0.3;
        assert!(
            (camera.pan_velocity - expected_pan_velocity).length() < 1e-5,
            "pan momentum blends 0.7/0.3"
        );
        assert_eq!(state.last, avg);
        assert_eq!(state.warp_px, avg + Vec2::new(10.0, -5.0));
    }

    /// One-hand release keeps the pan momentum for the coast (the throw);
    /// `grabbing_count` drops to 0 and the camera decays it later.
    #[test]
    fn one_hand_release_keeps_pan_momentum() {
        let mut state = FlameGrabState {
            grabbing_count: 1,
            ..FlameGrabState::default()
        };
        let mut camera = FlameCamera {
            pan_velocity: Vec2::new(6.0, -4.0),
            ..FlameCamera::default()
        };

        step_grab(&mut state, &mut camera, None, WINDOW, 1.0, 1.0, 1.0);

        assert_eq!(state.grabbing_count, 0);
        assert_eq!(
            camera.pan_velocity,
            Vec2::new(6.0, -4.0),
            "release must leave the pan fling for the camera to decay"
        );
    }

    // ── step_grab: two-hand zoom / rotate / pan ────────────────────────────

    /// Steady two-hand: spreading apart zooms in (distance shrinks by the
    /// inverse spread ratio); squeezing zooms out; gamma exponentiates.
    #[test]
    fn two_hand_spread_ratio_drives_distance() {
        let mut state = FlameGrabState {
            grabbing_count: 2,
            last: Vec2::new(640.0, 360.0),
            anchor_spread: 400.0,
            anchor_distance: 2.0,
            ..FlameGrabState::default()
        };
        let mut camera = FlameCamera {
            distance: 2.0,
            ..FlameCamera::default()
        };
        let apart = GrabGather {
            centroid: Vec2::new(640.0, 360.0),
            count: 2,
            spread: 500.0,
            line_angle: 0.0,
        };
        step_grab(&mut state, &mut camera, Some(apart), WINDOW, 1.0, 1.0, 1.0);
        assert!((camera.distance - 2.0 * (400.0 / 500.0)).abs() < 1e-5);

        let together = GrabGather {
            centroid: Vec2::new(640.0, 360.0),
            count: 2,
            spread: 200.0,
            line_angle: 0.0,
        };
        step_grab(
            &mut state,
            &mut camera,
            Some(together),
            WINDOW,
            1.0,
            1.0,
            1.0,
        );
        assert!((camera.distance - 2.0 * (400.0 / 200.0)).abs() < 1e-5);

        // gamma = 2 squares the ratio (anchor-based, so it replaces, not compounds).
        let apart2 = GrabGather {
            centroid: Vec2::new(640.0, 360.0),
            count: 2,
            spread: 800.0,
            line_angle: 0.0,
        };
        step_grab(&mut state, &mut camera, Some(apart2), WINDOW, 2.0, 1.0, 1.0);
        assert!((camera.distance - 2.0 * (400.0_f32 / 800.0).powi(2)).abs() < 1e-5);
    }

    /// Steady two-hand twist yaws the azimuth with the documented sign
    /// (window angle grows clockwise on screen; azimuth follows it, scaled by
    /// `rotate_gain`), leaves polar alone, and blends a yaw momentum whose
    /// sign continues the motion through the coast's `azimuth -= v.x` law.
    #[test]
    fn two_hand_twist_yaws_azimuth_with_momentum() {
        let mut state = FlameGrabState {
            grabbing_count: 2,
            last: Vec2::new(640.0, 360.0),
            anchor_spread: 400.0,
            anchor_distance: FlameCamera::default().distance,
            last_line_angle: 0.0,
            ..FlameGrabState::default()
        };
        let mut camera = FlameCamera::default();
        let az0 = camera.azimuth;
        let polar0 = camera.polar;
        let twist = 0.2_f32;
        let gain = 1.5_f32;
        let gather = GrabGather {
            centroid: Vec2::new(640.0, 360.0),
            count: 2,
            spread: 400.0,
            line_angle: twist,
        };

        step_grab(
            &mut state,
            &mut camera,
            Some(gather),
            WINDOW,
            1.0,
            1.0,
            gain,
        );

        assert!(
            (camera.azimuth - (az0 + gain * twist)).abs() < 1e-6,
            "azimuth follows the window-angle twist, scaled by rotate_gain"
        );
        assert!((camera.polar - polar0).abs() < 1e-6, "twist never tilts");
        // Coast applies `azimuth -= v.x`, so continuing +yaw needs v.x < 0.
        assert!(
            (camera.angular_velocity.x - (-gain * twist * 0.3)).abs() < 1e-6,
            "yaw momentum blends toward -yaw"
        );
        assert!(camera.angular_velocity.y.abs() < 1e-9, "no polar momentum");
        assert!((state.last_line_angle - twist).abs() < 1e-6);
    }

    /// A hand-order swap between frames (line angle jumps by π) produces no
    /// twist: the undirected-line wrap absorbs it.
    #[test]
    fn two_hand_hand_order_swap_produces_no_twist() {
        let mut state = FlameGrabState {
            grabbing_count: 2,
            last: Vec2::new(640.0, 360.0),
            anchor_spread: 400.0,
            anchor_distance: FlameCamera::default().distance,
            last_line_angle: 0.3,
            ..FlameGrabState::default()
        };
        let mut camera = FlameCamera::default();
        let az0 = camera.azimuth;
        let gather = GrabGather {
            centroid: Vec2::new(640.0, 360.0),
            count: 2,
            spread: 400.0,
            line_angle: 0.3 - PI, // same physical line, swapped hand order
        };

        step_grab(&mut state, &mut camera, Some(gather), WINDOW, 1.0, 1.0, 1.0);

        assert!(
            (camera.azimuth - az0).abs() < 1e-5,
            "a hand-order swap must not yaw the camera"
        );
    }

    /// Steady two-hand midpoint drag pans (target moves) and never tilts:
    /// polar holds still and pan momentum stays zeroed (no pan fling can
    /// survive a two-hand release).
    #[test]
    fn two_hand_midpoint_drag_pans_without_pan_momentum() {
        let mut state = FlameGrabState {
            grabbing_count: 2,
            last: Vec2::new(640.0, 360.0),
            anchor_spread: 400.0,
            anchor_distance: 0.7826,
            ..FlameGrabState::default()
        };
        let mut camera = FlameCamera {
            pan_velocity: Vec2::new(5.0, 5.0), // stale pan fling must be suppressed
            ..FlameCamera::default()
        };
        let polar0 = camera.polar;
        let gather = GrabGather {
            centroid: Vec2::new(660.0, 360.0),
            count: 2,
            spread: 400.0,
            line_angle: 0.0,
        };

        step_grab(&mut state, &mut camera, Some(gather), WINDOW, 1.0, 1.0, 1.0);

        assert!(
            camera.target.x < 0.0,
            "content follows hands: +x drag pans target -X"
        );
        assert!((camera.polar - polar0).abs() < 1e-6);
        assert_eq!(camera.pan_velocity, Vec2::ZERO);
        assert_eq!(state.last, gather.centroid);
        assert_eq!(
            state.warp_px,
            gather.centroid + state.mouse_offset,
            "warp still tracks the midpoint"
        );
    }

    /// Releasing straight out of two-hand mode leaves no pan fling and no
    /// polar momentum — only the (possibly zero) yaw momentum survives.
    #[test]
    fn two_hand_release_leaves_no_pan_fling() {
        let mut state = FlameGrabState {
            grabbing_count: 2,
            last: Vec2::new(640.0, 360.0),
            anchor_spread: 400.0,
            anchor_distance: 1.0,
            last_line_angle: 0.0,
            ..FlameGrabState::default()
        };
        // Seed stale momenta so the assertions prove the steady two-hand
        // frame actively zeroes them (not just that the default is 0).
        let mut camera = FlameCamera {
            angular_velocity: Vec2::new(0.2, 0.1),
            pan_velocity: Vec2::new(8.0, -3.0),
            ..FlameCamera::default()
        };
        let gather = GrabGather {
            centroid: Vec2::new(700.0, 400.0),
            count: 2,
            spread: 420.0,
            line_angle: 0.1,
        };
        step_grab(&mut state, &mut camera, Some(gather), WINDOW, 1.0, 1.0, 1.0);
        step_grab(&mut state, &mut camera, None, WINDOW, 1.0, 1.0, 1.0);
        assert_eq!(state.grabbing_count, 0);
        assert_eq!(
            camera.pan_velocity,
            Vec2::ZERO,
            "no pan fling out of two-hand mode"
        );
        assert!(
            camera.angular_velocity.y.abs() < 1e-9,
            "no polar momentum out of two-hand mode"
        );
        assert!(
            camera.angular_velocity.x.abs() > 1e-6,
            "the twist's yaw momentum survives the release"
        );
    }

    /// Dropping from two hands to one re-stashes (no jump) and then resumes
    /// one-hand pan on the following steady frame.
    #[test]
    fn two_to_one_transition_restashes_then_pans() {
        let mut state = FlameGrabState {
            grabbing_count: 2,
            last: Vec2::new(640.0, 360.0),
            anchor_spread: 400.0,
            anchor_distance: 1.0,
            ..FlameGrabState::default()
        };
        let mut camera = FlameCamera::default();

        let one = GrabGather {
            centroid: Vec2::new(300.0, 200.0),
            count: 1,
            spread: 0.0,
            line_angle: 0.0,
        };
        step_grab(&mut state, &mut camera, Some(one), WINDOW, 1.0, 1.0, 1.0);
        assert_eq!(
            camera.target,
            Vec3::ZERO,
            "transition frame must not jump the pan"
        );

        let moved = GrabGather {
            centroid: Vec2::new(320.0, 200.0),
            count: 1,
            spread: 0.0,
            line_angle: 0.0,
        };
        step_grab(&mut state, &mut camera, Some(moved), WINDOW, 1.0, 1.0, 1.0);
        assert!(
            camera.target.length() > 1e-6,
            "steady one-hand frame pans again"
        );
    }

    // ── Idle veto ──────────────────────────────────────────────────────────

    /// `flame_idle_veto`: false at rest, true with angular momentum, pan
    /// momentum, or an active grab.
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
        assert!(flame_idle_veto(&world), "coasting yaw fling must veto idle");

        world.insert_resource(FlameCamera {
            pan_velocity: Vec2::new(2.0, 0.0),
            ..FlameCamera::default()
        });
        assert!(flame_idle_veto(&world), "coasting pan fling must veto idle");

        world.insert_resource(FlameCamera::default());
        world.insert_resource(FlameGrabState {
            grabbing_count: 1,
            ..FlameGrabState::default()
        });
        assert!(flame_idle_veto(&world), "active grab must veto idle");
    }

    // ── World-level system wiring ──────────────────────────────────────────

    /// World-level: `update_flame_hands` counts a single engaged hand.
    /// Mirrors `drive_dots_audio_raises_envelope_from_hand_alone`.
    #[test]
    fn update_flame_hands_counts_one_grabbing_hand() {
        let mut world = World::new();
        world.insert_resource(FlameCamera::default());
        world.insert_resource(FlameGrabState::default());
        world.insert_resource(FlameSettings::default());
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

    /// World-level: a moving single engaged hand pans the camera target and
    /// leaves pan momentum (the fling seed) — and never orbits.
    #[test]
    fn update_flame_hands_one_hand_motion_pans() {
        let mut world = World::new();
        world.insert_resource(FlameCamera::default());
        world.insert_resource(FlameGrabState::default());
        world.insert_resource(FlameSettings::default());
        world.spawn(Window::default());
        let hand = world
            .spawn((
                TrackedHand,
                Transform::default(),
                Visibility::default(),
                PalmPosition(Vec3::new(0.0, 195.0, 200.0)),
                GrabStrength(0.9),
            ))
            .id();
        let az0 = world.resource::<FlameCamera>().azimuth;

        // Frame 1: engage (stash, no movement).
        world
            .run_system_once(update_flame_hands)
            .expect("engage frame");
        // Frame 2: hand moves.
        world
            .entity_mut(hand)
            .insert(PalmPosition(Vec3::new(40.0, 195.0, 200.0)));
        world
            .run_system_once(update_flame_hands)
            .expect("move frame");

        let camera = world.resource::<FlameCamera>();
        assert!(
            camera.target.length() > 1e-6,
            "one-hand motion must pan the target"
        );
        assert!(
            camera.pan_velocity.length() > 1e-6,
            "one-hand motion must seed the pan fling"
        );
        assert!(
            (camera.azimuth - az0).abs() < 1e-6,
            "one hand must never orbit"
        );
    }

    /// World-level: two engaged hands spreading apart zoom the camera in.
    #[test]
    fn update_flame_hands_two_hands_spreading_zooms_in() {
        let mut world = World::new();
        world.insert_resource(FlameCamera::default());
        world.insert_resource(FlameGrabState::default());
        world.insert_resource(FlameSettings::default());
        world.spawn(Window::default());
        let left = world
            .spawn((
                TrackedHand,
                Transform::default(),
                Visibility::default(),
                PalmPosition(Vec3::new(-50.0, 195.0, 200.0)),
                GrabStrength(0.9),
            ))
            .id();
        let right = world
            .spawn((
                TrackedHand,
                Transform::default(),
                Visibility::default(),
                PalmPosition(Vec3::new(50.0, 195.0, 200.0)),
                GrabStrength(0.9),
            ))
            .id();

        // Frame 1: engage (stash anchors, no movement yet).
        world
            .run_system_once(update_flame_hands)
            .expect("engage frame");
        let d0 = world.resource::<FlameCamera>().distance;
        assert_eq!(world.resource::<FlameGrabState>().grabbing_count, 2);

        // Frame 2: hands spread apart symmetrically -> zoom in.
        world
            .entity_mut(left)
            .insert(PalmPosition(Vec3::new(-120.0, 195.0, 200.0)));
        world
            .entity_mut(right)
            .insert(PalmPosition(Vec3::new(120.0, 195.0, 200.0)));
        world
            .run_system_once(update_flame_hands)
            .expect("spread frame");
        let d1 = world.resource::<FlameCamera>().distance;
        assert!(d1 < d0, "spreading apart must zoom in: {d1} !< {d0}");
    }
}
