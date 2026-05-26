//! Pointer-driven mouse attractor lifecycle.
//!
//! - [`update_mouse_attractor`] tracks pointer button transitions and updates
//!   [`MouseAttractorState`]: press rising edge sets `power = MOUSE_POWER_PRESS`,
//!   release falling edge immediately zeros power, and position follows the
//!   cursor every frame. Covers mouse, touch, and (feature-gated) hand pinch.
//! - [`decay_mouse_attractor`] decays the attractor's power geometrically each
//!   frame. While the button is held, power asymptotes toward
//!   `MOUSE_POWER_FLOOR = 2.0` but never reaches zero — only an explicit
//!   release event (from `update_mouse_attractor`) can zero the power.
//!   Matches v4's `MOUSE_ATTRACTOR_POWER_DECAY_SPEED` of `0.9`.

use bevy::prelude::*;
use wc_core::input::pointer::PointerState;

/// Lifecycle state for the mouse attractor — power that activates on click and
/// decays geometrically while held. Matches v4's behavior: `power=10` on press;
/// each frame `power = floor + (power - floor) * 0.9`, asymptoting to floor
/// while held. Power becomes exactly zero only on explicit release.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct MouseAttractorState {
    /// Current power. `0.0` = inactive. Also read by `line_idle_veto` (in
    /// `crate::line::mod`) to keep the sketch `Active` while the attractor
    /// is still decaying.
    pub power: f32,
    /// World-space position (followed every frame the cursor moves).
    pub position: [f32; 2],
}

/// v4 `MOUSE_ATTRACTOR_POWER_DECAY_SPEED = 0.9`.
pub const MOUSE_POWER_DECAY: f32 = 0.9;
/// v4 `MOUSE_ATTRACTOR_POWER_DECAY_FLOOR = 2.0`. Power asymptotes toward this
/// floor while the button is held but never reaches zero — only an explicit
/// release can set power to zero.
pub const MOUSE_POWER_FLOOR: f32 = 2.0;
/// v4 `enableMouseAttractor`: `power = 10` on click.
pub const MOUSE_POWER_PRESS: f32 = 10.0;

/// Pinch strength at which a hand counts as "pressed" (analogous to a finger
/// touching the screen). Leap Motion's `pinch_strength` ranges `[0, 1]`; this
/// threshold gives a comfortable "pinched" pose without false-triggering on
/// half-closed hands.
#[cfg(feature = "hand-tracking-gestures")]
pub const PINCH_PRESS_THRESHOLD: f32 = 0.85;

/// Tracks last-frame pinch state per chirality so we can detect press AND
/// release *edges* (rising = transition from below-threshold to above;
/// falling = transition from above to below). Without edge detection,
/// holding a pinched fist would re-trigger `MOUSE_POWER_PRESS` every frame,
/// and a slowly-relaxing pinch would never trigger the release path that
/// zeros power.
#[cfg(feature = "hand-tracking-gestures")]
#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct LastPinchState {
    /// Was the left hand above [`PINCH_PRESS_THRESHOLD`] last frame?
    pub left_pinched: bool,
    /// Was the right hand above [`PINCH_PRESS_THRESHOLD`] last frame?
    pub right_pinched: bool,
}

/// Tracks pointer-button-equivalent transitions across mouse, touch, and
/// (under the `hand-tracking-gestures` feature) tracked-hand pinch, and
/// updates [`MouseAttractorState`].
///
/// Matches v4's `pointerdown`/`pointerup`: only the rising edge of "any
/// source pressed" sets `power = MOUSE_POWER_PRESS`; the falling edge
/// (explicit release) immediately sets `power = 0.0`. Held with a stationary
/// input lets `decay_mouse_attractor` run asymptotically toward floor.
/// Positional updates just move the attractor.
///
/// Release detection is independent of `pointer.primary`: an off-screen
/// release still zeros power.
///
/// Hand-tracking gesture (feature-gated, since `HandTrackingState` has no
/// writer until Plan 12+ lands a provider): pinch strength ≥
/// [`PINCH_PRESS_THRESHOLD`] = 0.85 counts as pressed. [`LastPinchState`]
/// tracks per-chirality edges so a held pinch doesn't re-trigger every frame,
/// and a relaxing pinch triggers the release path that zeros power.
pub fn update_mouse_attractor(
    pointer: Res<'_, PointerState>,
    mouse_buttons: Res<'_, bevy::input::ButtonInput<bevy::input::mouse::MouseButton>>,
    touches: Res<'_, bevy::input::touch::Touches>,
    #[cfg(feature = "hand-tracking-gestures")] hands: Res<
        '_,
        wc_core::input::state::HandTrackingState,
    >,
    #[cfg(feature = "hand-tracking-gestures")] mut last_pinch: ResMut<'_, LastPinchState>,
    window: Single<'_, '_, &Window>,
    mut state: ResMut<'_, MouseAttractorState>,
) {
    let mouse_just_pressed = mouse_buttons.just_pressed(bevy::input::mouse::MouseButton::Left);
    let mouse_just_released = mouse_buttons.just_released(bevy::input::mouse::MouseButton::Left);
    let touch_just_pressed = touches.iter_just_pressed().next().is_some();
    let touch_just_released = touches.iter_just_released().next().is_some();

    #[cfg(feature = "hand-tracking-gestures")]
    let (hand_just_pressed, hand_just_released) = {
        let right_now = hands
            .right()
            .is_some_and(|h| h.pinch_strength >= PINCH_PRESS_THRESHOLD);
        let left_now = hands
            .left()
            .is_some_and(|h| h.pinch_strength >= PINCH_PRESS_THRESHOLD);
        let right_pressed_edge = right_now && !last_pinch.right_pinched;
        let left_pressed_edge = left_now && !last_pinch.left_pinched;
        let right_released_edge = !right_now && last_pinch.right_pinched;
        let left_released_edge = !left_now && last_pinch.left_pinched;
        last_pinch.right_pinched = right_now;
        last_pinch.left_pinched = left_now;
        (
            right_pressed_edge || left_pressed_edge,
            right_released_edge || left_released_edge,
        )
    };
    #[cfg(not(feature = "hand-tracking-gestures"))]
    let (hand_just_pressed, hand_just_released) = (false, false);

    let just_pressed = mouse_just_pressed || touch_just_pressed || hand_just_pressed;
    let just_released = mouse_just_released || touch_just_released || hand_just_released;

    if let Some(cursor_window) = pointer.primary {
        let w = window.width();
        let h = window.height();
        let wx = cursor_window.x - w * 0.5;
        let wy = -(cursor_window.y - h * 0.5);
        state.position = [wx, wy];

        if just_pressed {
            state.power = MOUSE_POWER_PRESS;
        }
    }
    // v4 parity: explicit release events zero the attractor. The geometric
    // decay alone never reaches zero (asymptotic to floor), so without this,
    // a released attractor would stay visible forever. Release detection is
    // independent of `pointer.primary` so an off-screen release still zeros.
    if just_released {
        state.power = 0.0;
    }
}

/// Decay the mouse attractor power each frame regardless of input state.
///
/// v4 parity: `power = floor + (power - floor) * 0.9` per frame. The decay
/// is asymptotic — it never reaches zero. Power can only become exactly
/// zero on explicit release, handled by [`update_mouse_attractor`]. This
/// system runs whether the button is held or not; while held, power
/// approaches floor; after release (which zeros power directly), this
/// system early-returns on the `power <= 0.0` guard.
pub fn decay_mouse_attractor(mut state: ResMut<'_, MouseAttractorState>) {
    if state.power <= 0.0 {
        return;
    }
    state.power = MOUSE_POWER_FLOOR + (state.power - MOUSE_POWER_FLOOR) * MOUSE_POWER_DECAY;
}
