//! Per-frame driver that updates [`crate::dots::post_process::DotsPostParams`]
//! from the live cursor position (or hand focal), window resolution, and
//! [`super::super::settings::DotsSettings`] values.
//!
//! ## Role
//!
//! [`update_dots_post_params`] runs every `Update` frame while
//! `sketch_active(AppState::Dots)` is true. It writes four fields into the
//! [`DotsPostParams`] resource, which [`ExtractResourcePlugin`] then mirrors
//! into the render world for the explode post-process pass:
//!
//! - `i_resolution`: current window size in logical pixels.
//! - `i_mouse`: cursor in v4 UV coordinates `(x/w, (h−y)/h)` when no hand is
//!   grabbing; the eased hand focal (world → Dots UV) when a hand is active.
//!   When both are absent the previous value is kept (no corner snap).
//! - `shrink_factor`: live from [`super::super::settings::DotsSettings::shrink_factor`]
//!   (v4 default 0.98).
//! - `gamma`: live from [`super::super::settings::DotsSettings::gamma`].
//!
//! ## Hand focal override
//!
//! While any [`wc_core::input::entity::TrackedHand`] entity carries a
//! [`crate::dots::hand_attractors::DotsHandAttractor`] with
//! `power.abs() > 1e-2`, the explode spiral center follows a center-biased,
//! exponentially-smoothed hand centroid instead of the cursor. The smoothed
//! state persists in [`DotsExplodeFocal`] so the focal eases from the cursor
//! position when the hand appears and relaxes to center when the grab releases
//! — no snapping. Hands and mouse are mutually exclusive: a hand overrides the
//! cursor exactly as [`crate::line::systems::sim_params::LineSmearFocal`] does
//! for the Line smear effect.
//!
//! ## Y-flip convention
//!
//! Bevy's `PointerState.cursor` uses window logical coordinates: top-left
//! origin, +y down. v4's `iMouse` uses a bottom-left-origin UV:
//! `(mouseX / width, (height − mouseY) / height)`. The explode shader was
//! ported to match v4's UV, so the same flip is applied here so the spiral
//! centers on the physical cursor. The exact formula is tested in
//! `tests::i_mouse_v4_uv_formula_with_known_cursor`.
//!
//! ## No-cursor guard
//!
//! When `PointerState.cursor` is `None` (no mouse movement, no active touch)
//! and no hand is grabbing, `i_mouse` is left unchanged from its previous
//! value. Writing `[0.0, 0.0]` would move the explode spiral to the top-left
//! corner, which is visually wrong. The resource is initialised to
//! `[0.5, 0.5]` (screen centre) on `OnEnter(AppState::Dots)`.
//!
//! ## Allocation budget
//!
//! All arithmetic is on stack scalars. The hand sample gather uses a
//! fixed-capacity stack array `[(f32, [f32; 2]); MAX_ATTRACTORS]` plus a
//! count — no heap allocation on the hot path, satisfying the multi-hour soak
//! requirement.
//!
//! [`ExtractResourcePlugin`]: bevy::render::extract_resource::ExtractResourcePlugin

use bevy::prelude::*;
use wc_core::input::entity::TrackedHand;
use wc_core::input::pointer::PointerState;

use crate::dots::hand_attractors::DotsHandAttractor;
use crate::dots::post_process::DotsPostParams;
use crate::dots::settings::DotsSettings;
use crate::line::systems::sim_params::{ease_focal, weighted_focal, FOCAL_CENTER_WEIGHT};
use crate::particles::particle::MAX_ATTRACTORS;

/// World-space focal point for the Dots explode (chromatic-aberration) spiral
/// center.
///
/// Stored in world coordinates (origin at screen center, +y up) so it shares
/// the same reference frame as [`DotsHandAttractor::position`] values.
///
/// While a hand is grabbing, [`update_dots_post_params`] eases this value
/// toward the center-biased hand centroid each frame using
/// [`ease_focal`]. When no hand is active the cursor drives `i_mouse`
/// directly, and this resource is kept in sync with the cursor so the next
/// hand grab eases from the cursor position rather than from a stale point.
///
/// Inserted at [`Vec2::ZERO`] (screen center) on `OnEnter(AppState::Dots)` and
/// removed on `OnExit` — mirroring
/// [`crate::line::systems::sim_params::LineSmearFocal`]. Deliberately a
/// `Resource`, not a `Local`, so it cannot carry a stale focal across a Dots
/// re-entry.
#[derive(Resource, Debug, Clone, Copy)]
pub struct DotsExplodeFocal(pub Vec2);

/// Per-frame `Update` system — writes [`DotsPostParams`] from the live hand
/// focal or cursor, window resolution, and [`DotsSettings`].
///
/// Runs only while `sketch_active(AppState::Dots)` is true (registered in
/// [`crate::dots::DotsPlugin::build`]).
///
/// - `i_resolution` — current window size in logical pixels.
/// - `i_mouse` — hand focal in Dots v4 UV when a grabbing hand is active;
///   cursor in v4 UV `(x/w, (h−y)/h)` otherwise. When both are absent the
///   previous value is kept (no corner snap). Uses [`PointerState::cursor`]
///   (mouse/touch only) for the mouse path.
/// - `shrink_factor` — live from [`DotsSettings::shrink_factor`] (v4 default
///   0.98).
/// - `gamma` — live from [`DotsSettings::gamma`].
///
/// Per-frame no-allocation guarantee: the hand sample gather uses a
/// fixed-capacity stack array `[(f32, [f32; 2]); MAX_ATTRACTORS]` + a
/// count — no `Vec` on the hot path.
pub fn update_dots_post_params(
    window: Single<'_, '_, &Window>,
    pointer: Res<'_, PointerState>,
    settings: Res<'_, DotsSettings>,
    time: Res<'_, Time>,
    dots_hands: Query<'_, '_, &DotsHandAttractor, With<TrackedHand>>,
    mut params: ResMut<'_, DotsPostParams>,
    mut focal: ResMut<'_, DotsExplodeFocal>,
) {
    let w = window.width();
    let h = window.height();

    params.i_resolution = [w, h];
    params.shrink_factor = settings.shrink_factor;
    params.gamma = settings.gamma;

    let dt = time.delta_secs();

    // Gather grabbing-hand samples (raw power weight, world position as [f32; 2])
    // into a fixed-capacity stack buffer matching `weighted_focal`'s slice signature.
    // DotsHandAttractor.position is world-space Vec2; `.to_array()` converts to [f32; 2].
    // No heap allocation — satisfies the per-frame hot-path constraint.
    let mut samples = [(0.0_f32, [0.0_f32; 2]); MAX_ATTRACTORS];
    let mut n = 0_usize;
    for hand in &dots_hands {
        if hand.power.abs() > 1e-2 && n < MAX_ATTRACTORS {
            samples[n] = (hand.power, hand.position.to_array());
            n += 1;
        }
    }

    if n > 0 {
        // Hand overrides mouse: ease the focal toward the center-biased centroid.
        // The exponential filter prevents snapping when a jittery hand moves, and
        // the centroid relaxes smoothly to screen center as the grab releases.
        // Mirror of Line's `LineSmearFocal` logic in `update_sim_params`.
        let target = weighted_focal(&samples[..n], FOCAL_CENTER_WEIGHT);
        let eased = ease_focal(
            focal.0.to_array(),
            target,
            dt,
            settings.explode_focal_smoothing,
        );
        focal.0 = Vec2::from(eased);
        // World-space → Dots v4 UV: world x in [-w/2, w/2] maps to [0, 1];
        // world y in [-h/2, h/2] maps to [0, 1] (+y up in world, +y up in UV).
        params.i_mouse = [(focal.0.x + w * 0.5) / w, (focal.0.y + h * 0.5) / h];
    } else if let Some(cursor) = pointer.cursor {
        // v4 UV convention: `iMouse = (mouseX / width, (height − mouseY) / height)`.
        // PointerState.cursor is top-left origin (+y down); dividing (h − y) by h
        // flips to a bottom-left UV origin, matching how the explode shader was ported
        // from the v4 GLSL. When no cursor is present, leave i_mouse unchanged so
        // the spiral stays at its last known position rather than snapping to a corner.
        params.i_mouse = [cursor.x / w, (h - cursor.y) / h];
        // Keep the focal in sync with the cursor so a later hand grab eases from
        // the cursor position, not from a stale point.
        // Inverse of the world→UV formula: world_x = cursor.x - w/2,
        // world_y = (h - cursor.y) - h/2.
        focal.0 = Vec2::new(cursor.x - w * 0.5, (h - cursor.y) - h * 0.5);
    }
    // (no hand, no cursor: leave i_mouse unchanged — existing guard)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions — panicking on unexpected system-run failure is the correct behaviour"
)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use bevy::math::Vec2;
    use std::time::Duration;
    use wc_core::input::pointer::PointerState;

    /// Build a minimal world with all resources `update_dots_post_params` requires.
    ///
    /// Spawns a `Window` entity at `(width_px × height_px)`, inserts
    /// `PointerState`, `DotsSettings`, `DotsPostParams`, `DotsExplodeFocal`,
    /// and `Time` (advanced by 16 ms so `delta_secs() > 0`).
    fn setup_world(
        width_px: u32,
        height_px: u32,
        cursor: Option<Vec2>,
        gamma: f32,
        shrink_factor: f32,
        prior_i_mouse: [f32; 2],
        focal_start: Vec2,
    ) -> World {
        let mut world = World::new();

        world.spawn(Window {
            resolution: (width_px, height_px).into(),
            ..Default::default()
        });

        world.insert_resource(PointerState {
            cursor,
            ..Default::default()
        });

        world.insert_resource(DotsSettings {
            gamma,
            shrink_factor,
            ..Default::default()
        });

        // Insert DotsPostParams with the given prior i_mouse so the no-cursor
        // test can verify the value is left unchanged.
        world.insert_resource(DotsPostParams {
            i_resolution: [0.0, 0.0],
            i_mouse: prior_i_mouse,
            shrink_factor: 0.0,
            gamma: 0.0,
        });

        // Focal resource — starts at the provided world-space position.
        world.insert_resource(DotsExplodeFocal(focal_start));

        // Time — advance by 16 ms so delta_secs() > 0.0 (avoids α = 0 in
        // ease_focal, which would produce no movement even toward a target).
        let mut time = Time::<()>::default();
        time.advance_by(Duration::from_millis(16));
        world.insert_resource(time);

        world
    }

    /// Core correctness: `i_resolution`, gamma, `shrink_factor` from settings,
    /// and the exact v4 Y-flip formula for `i_mouse` are all written in one pass.
    ///
    /// Cursor (200.0, 150.0) on an 800 × 600 window:
    ///   `i_mouse.x` = 200 / 800 = 0.25
    ///   `i_mouse.y` = (600 − 150) / 600 = 450 / 600 = 0.75
    ///
    /// `shrink_factor` = 0.95 (from settings, not hardcoded 0.98) — pins that
    /// the value is live from [`DotsSettings`], not a hardcoded constant.
    ///
    /// This assertion pins the v4 UV convention. If the formula changes, this
    /// test breaks immediately — which is the intent.
    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "literals are exact IEEE-754 fractions; bit-exact comparison is correct here"
    )]
    fn i_mouse_v4_uv_formula_with_known_cursor() {
        let mut world = setup_world(
            800,
            600,
            Some(Vec2::new(200.0, 150.0)),
            1.7,
            0.95, // non-default shrink so we can pin the settings-driven path
            [0.5, 0.5],
            Vec2::ZERO,
        );

        world
            .run_system_once(update_dots_post_params)
            .expect("update_dots_post_params run");

        let params = world.resource::<DotsPostParams>();

        // Resolution
        assert_eq!(
            params.i_resolution,
            [800.0, 600.0],
            "i_resolution must equal the window size"
        );

        // Gamma read from DotsSettings
        assert_eq!(params.gamma, 1.7, "gamma must come from DotsSettings");

        // shrink_factor comes from DotsSettings (0.95), not the old hardcoded 0.98.
        assert_eq!(
            params.shrink_factor, 0.95,
            "shrink_factor must come from DotsSettings, not be hardcoded"
        );

        // v4 i_mouse: (x/w, (h-y)/h) — Y-flip pinned so a convention change is caught
        //   x component: 200.0 / 800.0 = 0.25
        //   y component: (600.0 - 150.0) / 600.0 = 0.75
        let expected_x = 200.0_f32 / 800.0_f32;
        let expected_y = (600.0_f32 - 150.0_f32) / 600.0_f32;
        assert_eq!(
            params.i_mouse,
            [expected_x, expected_y],
            "i_mouse must follow v4 UV convention: (x/w, (h-y)/h); \
             expected [{expected_x}, {expected_y}], got {:?}",
            params.i_mouse,
        );
    }

    /// When `PointerState.cursor` is `None` and no hand is active, `i_mouse`
    /// must be LEFT UNCHANGED from its prior value. Writing `[0,0]` would snap
    /// the explode spiral to the top-left corner, which is visually wrong.
    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "prior value is a literal — bit-exact equality is the correct assertion"
    )]
    fn no_cursor_leaves_i_mouse_unchanged() {
        let prior = [0.3_f32, 0.6_f32];
        let mut world = setup_world(1280, 720, None, 1.0, 0.98, prior, Vec2::ZERO);

        world
            .run_system_once(update_dots_post_params)
            .expect("update_dots_post_params run (no cursor)");

        let params = world.resource::<DotsPostParams>();
        assert_eq!(
            params.i_mouse, prior,
            "i_mouse must be unchanged when PointerState.cursor is None and no hand grabs; \
             got {:?}, expected {prior:?}",
            params.i_mouse,
        );
    }

    /// Verify that resolution and gamma are still written correctly even when
    /// there is no cursor (the no-cursor guard must not block the other fields).
    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "literal defaults — bit-exact comparison is correct"
    )]
    fn resolution_and_gamma_written_when_cursor_absent() {
        let mut world = setup_world(1920, 1080, None, 2.2, 0.98, [0.5, 0.5], Vec2::ZERO);

        world
            .run_system_once(update_dots_post_params)
            .expect("update_dots_post_params run (no cursor, resolution check)");

        let params = world.resource::<DotsPostParams>();
        assert_eq!(
            params.i_resolution,
            [1920.0, 1080.0],
            "i_resolution must be written even when cursor is absent"
        );
        assert_eq!(
            params.gamma, 2.2,
            "gamma must be written even when cursor is absent"
        );
        assert_eq!(
            params.shrink_factor, 0.98,
            "shrink_factor must be written even when cursor is absent"
        );
    }

    /// A grabbing hand (power > 1e-2) overrides the mouse and eases `i_mouse`
    /// toward the hand's UV coordinates.
    ///
    /// Hand at world (200, 100) on an 800 × 600 window:
    ///   raw UV = [(200 + 400) / 800, (100 + 300) / 600] = [0.75, 0.667]
    ///
    /// Center-biased via `weighted_focal([(1.0, [200, 100])], 0.15)`:
    ///   centroid ≈ [200 / 1.15, 100 / 1.15] ≈ [173.9, 87.0]
    ///   target UV ≈ [(173.9 + 400) / 800, (87.0 + 300) / 600] ≈ [0.717, 0.645]
    ///
    /// After one 16 ms ease from [`Vec2::ZERO`] at τ = 0.25 (α ≈ 0.063),
    /// `i_mouse` must have moved strictly off [0.5, 0.5] toward the hand's UV.
    #[test]
    fn grabbing_hand_eases_i_mouse_toward_hand_uv() {
        // No cursor — the hand must be the only `i_mouse` driver.
        let mut world = setup_world(800, 600, None, 1.0, 0.98, [0.5, 0.5], Vec2::ZERO);

        // Spawn a TrackedHand + DotsHandAttractor at world (200, 100), power > 1e-2.
        world.spawn((
            TrackedHand,
            DotsHandAttractor {
                power: 1.0,
                position: Vec2::new(200.0, 100.0),
            },
        ));

        world
            .run_system_once(update_dots_post_params)
            .expect("update_dots_post_params run (grabbing hand)");

        let params = world.resource::<DotsPostParams>();

        // After one 16 ms ease from center [0.5, 0.5] toward the hand's
        // target UV ≈ [0.717, 0.645], `i_mouse` must have moved strictly
        // in the right direction — off center, not yet at the target.
        assert!(
            params.i_mouse[0] > 0.5 && params.i_mouse[0] < 0.75,
            "i_mouse.x must have eased toward hand UV (0.75), strictly off 0.5; got {}",
            params.i_mouse[0]
        );
        assert!(
            params.i_mouse[1] > 0.5 && params.i_mouse[1] < 0.667,
            "i_mouse.y must have eased toward hand UV (0.667), strictly off 0.5; got {}",
            params.i_mouse[1]
        );
    }

    /// A hand with power <= 1e-2 is below the active-grab threshold and must
    /// NOT override the cursor.
    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "cursor formula is exact for these literal inputs"
    )]
    fn low_power_hand_does_not_override_cursor() {
        let mut world = setup_world(
            800,
            600,
            Some(Vec2::new(200.0, 150.0)),
            1.0,
            0.98,
            [0.5, 0.5],
            Vec2::ZERO,
        );

        // A hand present but with negligible power (at or below the 1e-2 threshold).
        world.spawn((
            TrackedHand,
            DotsHandAttractor {
                power: 0.005, // << 1e-2 threshold
                position: Vec2::new(300.0, 200.0),
            },
        ));

        world
            .run_system_once(update_dots_post_params)
            .expect("update_dots_post_params run (low-power hand)");

        let params = world.resource::<DotsPostParams>();
        // Cursor UV for (200, 150) on 800×600: [0.25, 0.75].
        let expected_x = 200.0_f32 / 800.0_f32;
        let expected_y = (600.0_f32 - 150.0_f32) / 600.0_f32;
        assert_eq!(
            params.i_mouse,
            [expected_x, expected_y],
            "cursor must drive i_mouse when no hand has power > 1e-2"
        );
    }
}
