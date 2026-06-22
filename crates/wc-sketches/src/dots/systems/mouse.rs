//! Pointer-driven mouse attractor lifecycle for the Dots ("Fabric") sketch.
//!
//! - [`update_dots_mouse_attractor`] tracks pointer button transitions across
//!   mouse and touch, and updates [`DotsMouseAttractorState`]: the rising edge
//!   of "any source pressed" sets `power = DOTS_MOUSE_POWER_PRESS`; the falling
//!   edge (explicit release) immediately zeros power; position follows the cursor
//!   every frame.
//! - [`decay_dots_mouse_attractor`] decays the attractor's power geometrically
//!   each frame via `power = FLOOR + (power - FLOOR) * DECAY`. While the button
//!   is held, power asymptotes toward `DOTS_MOUSE_POWER_FLOOR = 2.0` — but
//!   never reaches zero. Only an explicit release event (from
//!   `update_dots_mouse_attractor`) can zero the power.
//!
//! Faithful-to-v4 note: the press/decay math mirrors v4 exactly — `createAttractor`
//! sets `power = 1`, `ATTRACTOR_POWER_DECAY_FLOOR = 2`, so a held attractor's power
//! RISES asymptotically toward 2 rather than falling. Do not "fix" it.
//! The idle-veto keep-awake-while-held behavior is a v5 choice: v4's sleep gate
//! (`hasActiveAttractors` requires `power > 2.01`) is never triggered by the mouse
//! attractor (which asymptotes to 2.0 from below), so a held mouse does NOT block
//! v4's screensaver. This gating difference will be revisited in D6's screensaver
//! work.

use bevy::prelude::*;
use wc_core::input::pointer::PointerState;
use wc_core::settings::EguiPointerCaptured;

/// Lifecycle state for the Dots mouse attractor — power that activates on click
/// and evolves geometrically while held. The decay math mirrors v4: `power = 1`
/// on press (below floor); each frame `power = floor + (power - floor) * 0.9`,
/// which causes a held attractor to RISE asymptotically to floor (2.0). Power
/// becomes exactly zero only on explicit release.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct DotsMouseAttractorState {
    /// Current power. `0.0` = inactive. Also read by `dots_idle_veto` (in
    /// `crate::dots::mod`) to keep the sketch `Active` while the attractor
    /// is still active.
    pub power: f32,
    /// World-space position (followed every frame the cursor moves).
    pub position: [f32; 2],
}

/// v4 `ATTRACTOR_POWER_DECAY_SPEED = 0.9`. Per-frame geometric decay multiplier.
pub const DOTS_MOUSE_POWER_DECAY: f32 = 0.9;

/// v4 `ATTRACTOR_POWER_DECAY_FLOOR = 2`. The attractor's power asymptotes toward
/// this floor while the button is held. Because press starts at 1 (below floor),
/// power rises toward this value — never exceeds it. Only an explicit release can
/// set power to zero.
pub const DOTS_MOUSE_POWER_FLOOR: f32 = 2.0;

/// v4 `createAttractor` initial power = 1. On press, power is set to this value.
/// Because this is below `DOTS_MOUSE_POWER_FLOOR`, a held attractor's power
/// rises asymptotically toward the floor rather than falling. This is the
/// correct v4 Dots behavior; do not change it to equal or exceed the floor.
pub const DOTS_MOUSE_POWER_PRESS: f32 = 1.0;

/// Tracks pointer-button-equivalent transitions across mouse and touch, and
/// updates [`DotsMouseAttractorState`].
///
/// Matches v4 Dots `pointerdown`/`pointerup`: only the rising edge of "any
/// source pressed" sets `power = DOTS_MOUSE_POWER_PRESS`; the falling edge
/// (explicit release) immediately sets `power = 0.0`. While held,
/// `decay_dots_mouse_attractor` runs and causes power to rise asymptotically
/// toward the floor. Positional updates move the attractor every frame.
///
/// Release detection is independent of `pointer.primary`: an off-screen release
/// still zeros power.
///
/// ## egui pointer-capture gating
///
/// Mouse and touch presses are gated by [`EguiPointerCaptured`]: a click inside
/// the Settings panel would otherwise fire this handler, spawning a stray
/// attractor under the slider. When egui has captured the pointer, we suppress
/// only the press edge — release events and continuous position updates still
/// fire so an attractor activated outside the panel and dragged over it still
/// releases cleanly.
///
/// Hand attractors for Dots are handled in Plan D3 (a separate system and
/// attractor slot).
pub fn update_dots_mouse_attractor(
    pointer: Res<'_, PointerState>,
    mouse_buttons: Res<'_, bevy::input::ButtonInput<bevy::input::mouse::MouseButton>>,
    touches: Res<'_, bevy::input::touch::Touches>,
    egui_captured: Option<Res<'_, EguiPointerCaptured>>,
    window: Single<'_, '_, &Window>,
    mut state: ResMut<'_, DotsMouseAttractorState>,
) {
    let mouse_just_pressed = mouse_buttons.just_pressed(bevy::input::mouse::MouseButton::Left);
    let mouse_just_released = mouse_buttons.just_released(bevy::input::mouse::MouseButton::Left);
    let touch_just_pressed = touches.iter_just_pressed().next().is_some();
    let touch_just_released = touches.iter_just_released().next().is_some();

    let pointer_captured = egui_captured.is_some_and(|c| c.0);
    let just_pressed = (mouse_just_pressed || touch_just_pressed) && !pointer_captured;
    let just_released = mouse_just_released || touch_just_released;

    // Use `pointer.cursor` (mouse/touch only), not `pointer.primary`, to avoid
    // the hand-tracking path hijacking the mouse attractor position (D3 wires
    // hand attractors on a separate slot; the mouse attractor tracks the cursor).
    if let Some(cursor_window) = pointer.cursor {
        let w = window.width();
        let h = window.height();
        let wx = cursor_window.x - w * 0.5;
        let wy = -(cursor_window.y - h * 0.5);
        state.position = [wx, wy];

        if just_pressed {
            state.power = DOTS_MOUSE_POWER_PRESS;
        }
    }
    // v4 parity: explicit release events zero the attractor immediately. The
    // geometric decay alone never reaches zero (it asymptotes to floor, or rises
    // toward it when press < floor), so without this explicit zero a released
    // attractor would pull forever. Release detection is independent of
    // `pointer.primary` so an off-screen release still zeros.
    if just_released {
        state.power = 0.0;
    }
}

/// Decay the Dots mouse attractor power each frame regardless of input state.
///
/// v4 parity: `power = floor + (power - floor) * 0.9` per frame. Because
/// `DOTS_MOUSE_POWER_PRESS = 1.0 < DOTS_MOUSE_POWER_FLOOR = 2.0`, a freshly
/// pressed attractor's power RISES toward 2 while held. After release (which
/// zeros power directly), this system early-returns on the `power <= 0.0` guard.
/// The asymptotic decay never reaches zero — only an explicit release does.
///
/// Reviewer note: the rising-power-on-hold behavior is correct v4 Dots behavior
/// (`press=1`, `floor=2`, `decay_speed=0.9`). Do not "fix" it to clamp at 1 or
/// start at floor.
pub fn decay_dots_mouse_attractor(mut state: ResMut<'_, DotsMouseAttractorState>) {
    if state.power <= 0.0 {
        return;
    }
    // v4 `ATTRACTOR_POWER_DECAY_FLOOR + (power - ATTRACTOR_POWER_DECAY_FLOOR)
    // * ATTRACTOR_POWER_DECAY_SPEED`. When press=1 < floor=2, this expands to:
    //   2 + (1 - 2) * 0.9 = 2 - 0.9 = 1.1 (rising toward 2 each frame).
    state.power =
        DOTS_MOUSE_POWER_FLOOR + (state.power - DOTS_MOUSE_POWER_FLOOR) * DOTS_MOUSE_POWER_DECAY;
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions — panicking on unexpected system-run failure is the correct behaviour"
)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    /// Press sets power to exactly `DOTS_MOUSE_POWER_PRESS` (1.0).
    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "DOTS_MOUSE_POWER_PRESS is the literal 1.0 — the assignment is bit-exact"
    )]
    fn press_sets_power_to_one() {
        let mut world = World::new();
        world.insert_resource(DotsMouseAttractorState::default());

        // Directly simulate what update_dots_mouse_attractor does on a press
        // rising edge: set power to DOTS_MOUSE_POWER_PRESS. We verify the
        // constant value rather than running the full input system (which would
        // need ButtonInput/Touches/Window mock infrastructure).
        world.resource_mut::<DotsMouseAttractorState>().power = DOTS_MOUSE_POWER_PRESS;

        let state = world.resource::<DotsMouseAttractorState>();
        assert_eq!(
            state.power, 1.0,
            "press must set power = 1.0 (DOTS_MOUSE_POWER_PRESS)"
        );
    }

    /// After a press (power=1.0), one `decay_dots_mouse_attractor` step rises
    /// power above 1.0 toward the floor (2.0) and stays strictly below 2.0.
    /// This is the v4 invariant: press=1 < floor=2, so held power rises.
    #[test]
    fn decay_from_press_power_rises_toward_floor_and_stays_below() {
        let mut world = World::new();
        world.insert_resource(DotsMouseAttractorState {
            power: DOTS_MOUSE_POWER_PRESS, // 1.0
            position: [0.0, 0.0],
        });

        world
            .run_system_once(decay_dots_mouse_attractor)
            .expect("decay_dots_mouse_attractor run");

        let power = world.resource::<DotsMouseAttractorState>().power;
        assert!(
            power > DOTS_MOUSE_POWER_PRESS,
            "power {power:.4} must rise above press=1.0 after one decay step (floor=2.0)"
        );
        assert!(
            power < DOTS_MOUSE_POWER_FLOOR,
            "power {power:.4} must stay below floor=2.0 (asymptotic, never reaches it)"
        );
    }

    /// After a release (power set to 0.0), `decay_dots_mouse_attractor` is a
    /// no-op — power stays exactly 0.0.
    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "power is written as literal 0.0 — bit-exact zero comparison is correct"
    )]
    fn release_zeros_power_and_subsequent_decay_is_noop() {
        let mut world = World::new();
        world.insert_resource(DotsMouseAttractorState {
            power: 0.0, // explicit release already happened
            position: [0.0, 0.0],
        });

        world
            .run_system_once(decay_dots_mouse_attractor)
            .expect("decay_dots_mouse_attractor run");

        let power = world.resource::<DotsMouseAttractorState>().power;
        assert_eq!(
            power, 0.0,
            "decay must be a no-op when power is already 0.0 (released)"
        );
    }

    /// Drives [`update_dots_mouse_attractor`] through a press then a release,
    /// actually running the system via [`RunSystemOnce`] (not just asserting on
    /// a constant). Covers the input-gating path: a valid cursor + `just_pressed`
    /// sets power and records the world-space position; `just_released` zeros
    /// power regardless of prior state.
    ///
    /// Mirrors the approach of `tests/line_input.rs::left_press_activates_mouse_attractor`
    /// but uses a bare `World` + `RunSystemOnce` instead of the full
    /// `sketches_test_app()` harness (which requires render plugins unavailable
    /// in unit tests).
    #[test]
    fn press_sets_power_and_position_then_release_zeros_power() {
        use bevy::input::mouse::MouseButton;
        use bevy::input::touch::Touches;
        use bevy::input::ButtonInput;
        use bevy::math::Vec2;
        use bevy::window::Window;
        use wc_core::input::pointer::PointerState;

        let mut world = World::new();

        // Cursor at the center of a 1280×720 window → world-space (0.0, 0.0).
        world.insert_resource(PointerState {
            cursor: Some(Vec2::new(640.0, 360.0)),
            ..Default::default()
        });

        // Left button just pressed this tick.
        let mut mouse_buttons = ButtonInput::<MouseButton>::default();
        mouse_buttons.press(MouseButton::Left);
        world.insert_resource(mouse_buttons);

        // No touch input active.
        world.insert_resource(Touches::default());
        // EguiPointerCaptured absent → Option<Res<_>> is None (no egui capture).

        world.insert_resource(DotsMouseAttractorState::default());

        // Single<_, _, &Window> requires exactly one Window entity.
        world.spawn(Window {
            resolution: (1280_u32, 720_u32).into(),
            ..Default::default()
        });

        // --- Press: system must set power = DOTS_MOUSE_POWER_PRESS and record
        // the cursor's world-space coordinates.
        world
            .run_system_once(update_dots_mouse_attractor)
            .expect("update_dots_mouse_attractor (press) run");

        let state = *world.resource::<DotsMouseAttractorState>();
        #[allow(
            clippy::float_cmp,
            reason = "DOTS_MOUSE_POWER_PRESS is literal 1.0; the assignment is bit-exact"
        )]
        {
            assert_eq!(
                state.power, DOTS_MOUSE_POWER_PRESS,
                "press must set power to DOTS_MOUSE_POWER_PRESS ({DOTS_MOUSE_POWER_PRESS})"
            );
        }
        // Cursor at (640, 360) on a 1280×720 window → world-space origin (0, 0).
        assert!(
            state.position[0].abs() < 0.5,
            "world-space x should be ≈ 0 (cursor at horizontal center), got {}",
            state.position[0]
        );
        assert!(
            state.position[1].abs() < 0.5,
            "world-space y should be ≈ 0 (cursor at vertical center), got {}",
            state.position[1]
        );

        // --- Release: replace ButtonInput with just_released active, just_pressed
        // cleared. `press` then `release` on a fresh ButtonInput yields both edges;
        // `clear_just_pressed` removes the press edge so only just_released fires.
        let mut mouse_buttons = ButtonInput::<MouseButton>::default();
        mouse_buttons.press(MouseButton::Left);
        mouse_buttons.release(MouseButton::Left);
        mouse_buttons.clear_just_pressed(MouseButton::Left);
        world.insert_resource(mouse_buttons);

        world
            .run_system_once(update_dots_mouse_attractor)
            .expect("update_dots_mouse_attractor (release) run");

        let power = world.resource::<DotsMouseAttractorState>().power;
        #[allow(
            clippy::float_cmp,
            reason = "release path writes literal 0.0 — bit-exact zero comparison is correct"
        )]
        {
            assert_eq!(power, 0.0, "release must zero power immediately");
        }
    }
}
