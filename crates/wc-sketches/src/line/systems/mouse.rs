//! Pointer-driven mouse attractor lifecycle.
//!
//! - [`update_mouse_attractor`] tracks pointer button transitions and updates
//!   [`MouseAttractorState`] (power = [`MOUSE_POWER_PRESS`] on press, position
//!   follows the cursor every frame).
//! - [`decay_mouse_attractor`] decays the attractor's power geometrically each
//!   frame so the pull fades smoothly after release, matching v4's
//!   `MOUSE_ATTRACTOR_POWER_DECAY_SPEED` of `0.9`.

use bevy::prelude::*;
use wc_core::input::pointer::PointerState;

/// Lifecycle state for the mouse attractor — power that activates on click and
/// decays geometrically while held or after release. Matches v4's behavior:
/// `power=10` on press; each frame `power = floor + (power - floor) * 0.9`
/// down to `power < floor + epsilon`, then zero.
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
/// v4 `MOUSE_ATTRACTOR_POWER_DECAY_FLOOR = 2.0`. Power below
/// [`MOUSE_POWER_FLOOR`] + [`MOUSE_POWER_DECAY_EPSILON`] zeros.
pub const MOUSE_POWER_FLOOR: f32 = 2.0;
/// v4 `enableMouseAttractor`: `power = 10` on click.
pub const MOUSE_POWER_PRESS: f32 = 10.0;
/// Tolerance below which a decaying mouse attractor is treated as zero.
/// Floor + one centipower; arbitrary cutoff small enough to be invisible.
pub const MOUSE_POWER_DECAY_EPSILON: f32 = 1e-2;

/// Pinch strength at which a hand counts as "pressed" (analogous to a finger
/// touching the screen). Leap Motion's `pinch_strength` ranges `[0, 1]`; this
/// threshold gives a comfortable "pinched" pose without false-triggering on
/// half-closed hands.
#[cfg(feature = "hand-tracking-gestures")]
pub const PINCH_PRESS_THRESHOLD: f32 = 0.85;

/// Tracks last-frame pinch state per chirality so we can detect press *edges*
/// (transition from below-threshold to above), not just "is currently
/// pinched." Without edge detection, holding a pinched fist would re-trigger
/// `MOUSE_POWER_PRESS` every frame (the bug Plan 7 Phase C fixed for mouse).
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
/// source pressed" sets `power = MOUSE_POWER_PRESS`. Held with a stationary
/// input decays to floor; positional updates just move the attractor.
///
/// Hand-tracking gesture (feature-gated, since `HandTrackingState` has no
/// writer until Plan 12+ lands a provider): pinch strength ≥
/// [`PINCH_PRESS_THRESHOLD`] = 0.85 counts as pressed. [`LastPinchState`]
/// tracks per-chirality edges so a held pinch doesn't re-trigger every frame.
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
    let touch_just_pressed = touches.iter_just_pressed().next().is_some();

    #[cfg(feature = "hand-tracking-gestures")]
    let hand_just_pressed = {
        let right_now = hands
            .right()
            .is_some_and(|h| h.pinch_strength >= PINCH_PRESS_THRESHOLD);
        let left_now = hands
            .left()
            .is_some_and(|h| h.pinch_strength >= PINCH_PRESS_THRESHOLD);
        let right_edge = right_now && !last_pinch.right_pinched;
        let left_edge = left_now && !last_pinch.left_pinched;
        last_pinch.right_pinched = right_now;
        last_pinch.left_pinched = left_now;
        right_edge || left_edge
    };
    #[cfg(not(feature = "hand-tracking-gestures"))]
    let hand_just_pressed = false;

    let just_pressed = mouse_just_pressed || touch_just_pressed || hand_just_pressed;

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
}

/// Decays the mouse attractor power each frame regardless of input state.
///
/// v4 runs this in the sketch's `animate()` regardless of idle state, so the
/// attractor's visual decay completes even after the user has stopped
/// interacting. Plan 8 will add the visual mesh; here only the physical power
/// matters.
pub fn decay_mouse_attractor(mut state: ResMut<'_, MouseAttractorState>) {
    if state.power <= 0.0 {
        return;
    }
    state.power = MOUSE_POWER_FLOOR + (state.power - MOUSE_POWER_FLOOR) * MOUSE_POWER_DECAY;
    if state.power < MOUSE_POWER_FLOOR + MOUSE_POWER_DECAY_EPSILON {
        state.power = 0.0;
    }
}
