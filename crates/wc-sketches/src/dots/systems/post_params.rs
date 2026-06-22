//! Per-frame driver that updates [`crate::dots::post_process::DotsPostParams`]
//! from the live cursor position, window resolution, and the [`super::super::settings::DotsSettings`]
//! gamma value.
//!
//! ## Role
//!
//! [`update_dots_post_params`] runs every `Update` frame while
//! `sketch_active(AppState::Dots)` is true. It writes four fields into the
//! [`DotsPostParams`] resource, which [`ExtractResourcePlugin`] then mirrors
//! into the render world for the explode post-process pass:
//!
//! - `i_resolution`: current window size in logical pixels.
//! - `i_mouse`: cursor in v4 UV coordinates `(x/w, (h−y)/h)` — top-left origin
//!   with Y flipped so the UV origin is bottom-left, matching the v4 GLSL
//!   convention used in `assets/shaders/dots/explode.wgsl`.
//! - `shrink_factor`: fixed at 0.98 (v4 default).
//! - `gamma`: live from [`super::super::settings::DotsSettings`].
//!
//! ## Y-flip convention
//!
//! Bevy's `PointerState.cursor` uses window logical coordinates: top-left
//! origin, +y down. v4's `iMouse` uses a bottom-left-origin UV:
//! `(mouseX / width, (height − mouseY) / height)`. The explode shader was
//! ported to match v4's UV, so the same flip is applied here so the spiral
//! centers on the physical cursor. The exact formula is tested in
//! [`tests::i_mouse_v4_uv_formula_with_known_cursor`].
//!
//! ## No-cursor guard
//!
//! When `PointerState.cursor` is `None` (no mouse movement, no active touch),
//! `i_mouse` is left unchanged from its previous value. Writing `[0.0, 0.0]`
//! would move the explode spiral to the top-left corner, which is visually
//! wrong. The resource is initialised to `[0.5, 0.5]` (screen centre) on
//! `OnEnter(AppState::Dots)`.
//!
//! ## Allocation budget
//!
//! All arithmetic is on stack scalars. No heap allocation occurs on the hot
//! path, satisfying the multi-hour soak requirement.
//!
//! [`ExtractResourcePlugin`]: bevy::render::extract_resource::ExtractResourcePlugin

use bevy::prelude::*;
use wc_core::input::pointer::PointerState;

use crate::dots::post_process::DotsPostParams;
use crate::dots::settings::DotsSettings;

/// Per-frame `Update` system — writes [`DotsPostParams`] from the live cursor,
/// window resolution, and [`DotsSettings`].
///
/// Runs only while `sketch_active(AppState::Dots)` is true (registered in
/// [`crate::dots::DotsPlugin::build`]).
///
/// - `i_resolution` — current window size in logical pixels.
/// - `i_mouse` — cursor in v4 UV coordinates `(x/w, (h−y)/h)`. When
///   `PointerState.cursor` is `None`, the previous value is kept (no corner
///   snap). Uses [`PointerState::cursor`] (mouse/touch only) so a tracked hand
///   does not hijack the explode centre.
/// - `shrink_factor` — always 0.98 (v4 default; no knob yet).
/// - `gamma` — live from [`DotsSettings::gamma`].
///
/// Per-frame no-allocation guarantee: only stack scalars are written.
pub fn update_dots_post_params(
    window: Single<'_, '_, &Window>,
    pointer: Res<'_, PointerState>,
    settings: Res<'_, DotsSettings>,
    mut params: ResMut<'_, DotsPostParams>,
) {
    let w = window.width();
    let h = window.height();

    params.i_resolution = [w, h];
    params.shrink_factor = 0.98;
    params.gamma = settings.gamma;

    // v4 UV convention: `iMouse = (mouseX / width, (height − mouseY) / height)`.
    // PointerState.cursor is top-left origin (+y down); dividing (h − y) by h
    // flips to a bottom-left UV origin, matching how the explode shader was ported
    // from the v4 GLSL. When no cursor is present, leave i_mouse unchanged so
    // the spiral stays at its last known position rather than snapping to a corner.
    if let Some(cursor) = pointer.cursor {
        params.i_mouse = [cursor.x / w, (h - cursor.y) / h];
    }
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
    use wc_core::input::pointer::PointerState;

    /// Build a minimal world with the resources `update_dots_post_params`
    /// requires. Spawns a Window entity at `(width_px × height_px)` pixels and
    /// inserts `PointerState`, `DotsSettings`, and `DotsPostParams`.
    fn setup_world(
        width_px: u32,
        height_px: u32,
        cursor: Option<Vec2>,
        gamma: f32,
        prior_i_mouse: [f32; 2],
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

        world
    }

    /// Core correctness: `i_resolution`, gamma, `shrink_factor`, and the exact v4
    /// Y-flip formula for `i_mouse` are all written in one pass.
    ///
    /// Cursor (200.0, 150.0) on an 800 × 600 window:
    ///   `i_mouse.x` = 200 / 800 = 0.25
    ///   `i_mouse.y` = (600 − 150) / 600 = 450 / 600 = 0.75
    ///
    /// This assertion pins the v4 UV convention. If the formula changes, this
    /// test breaks immediately — which is the intent.
    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "literals are exact IEEE-754 fractions; bit-exact comparison is correct here"
    )]
    fn i_mouse_v4_uv_formula_with_known_cursor() {
        let mut world = setup_world(800, 600, Some(Vec2::new(200.0, 150.0)), 1.7, [0.5, 0.5]);

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

        // shrink_factor is always the v4 default
        assert_eq!(
            params.shrink_factor, 0.98,
            "shrink_factor must be the v4 default 0.98"
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

    /// When `PointerState.cursor` is `None`, `i_mouse` must be LEFT UNCHANGED
    /// from its prior value. Writing [0,0] would snap the explode spiral to the
    /// top-left corner, which is visually wrong.
    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "prior value is a literal — bit-exact equality is the correct assertion"
    )]
    fn no_cursor_leaves_i_mouse_unchanged() {
        let prior = [0.3_f32, 0.6_f32];
        let mut world = setup_world(1280, 720, None, 1.0, prior);

        world
            .run_system_once(update_dots_post_params)
            .expect("update_dots_post_params run (no cursor)");

        let params = world.resource::<DotsPostParams>();
        assert_eq!(
            params.i_mouse, prior,
            "i_mouse must be unchanged when PointerState.cursor is None; \
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
        let mut world = setup_world(1920, 1080, None, 2.2, [0.5, 0.5]);

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
}
