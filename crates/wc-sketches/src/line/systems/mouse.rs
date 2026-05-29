//! Pointer-driven mouse attractor lifecycle.
//!
//! - [`update_mouse_attractor`] tracks pointer button transitions and updates
//!   [`MouseAttractorState`]: press rising edge sets `power = MOUSE_POWER_PRESS`,
//!   release falling edge immediately zeros power, and position follows the
//!   cursor every frame. Covers mouse and touch.
//! - [`decay_mouse_attractor`] decays the attractor's power geometrically each
//!   frame. While the button is held, power asymptotes toward
//!   `MOUSE_POWER_FLOOR = 2.0` but never reaches zero — only an explicit
//!   release event (from `update_mouse_attractor`) can zero the power.
//!   Matches v4's `MOUSE_ATTRACTOR_POWER_DECAY_SPEED` of `0.9`.

use bevy::prelude::*;
use wc_core::input::pointer::PointerState;
use wc_core::settings::EguiPointerCaptured;

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


/// Tracks pointer-button-equivalent transitions across mouse and touch, and
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
/// ## egui pointer-capture gating
///
/// Mouse and touch presses are gated by [`EguiPointerCaptured`]: a click
/// inside the Settings panel both tweaks the egui widget AND would otherwise
/// fire this handler, spawning a stray attractor under the slider. When egui
/// has captured the pointer, we suppress only the press edge — release
/// events and continuous position updates still fire so an attractor that
/// was activated outside the panel and then dragged over it still releases
/// cleanly.
///
/// Hand tracking is handled separately by Plan 11.6's `LineHandAttractor`
/// component on `TrackedHand` entities (see `crate::line::leap_attractors`).
pub fn update_mouse_attractor(
    pointer: Res<'_, PointerState>,
    mouse_buttons: Res<'_, bevy::input::ButtonInput<bevy::input::mouse::MouseButton>>,
    touches: Res<'_, bevy::input::touch::Touches>,
    egui_captured: Option<Res<'_, EguiPointerCaptured>>,
    window: Single<'_, '_, &Window>,
    mut state: ResMut<'_, MouseAttractorState>,
) {
    let mouse_just_pressed = mouse_buttons.just_pressed(bevy::input::mouse::MouseButton::Left);
    let mouse_just_released = mouse_buttons.just_released(bevy::input::mouse::MouseButton::Left);
    let touch_just_pressed = touches.iter_just_pressed().next().is_some();
    let touch_just_released = touches.iter_just_released().next().is_some();

    let pointer_captured = egui_captured.is_some_and(|c| c.0);
    let pointer_press_active = (mouse_just_pressed || touch_just_pressed) && !pointer_captured;
    let just_pressed = pointer_press_active;
    let just_released = mouse_just_released || touch_just_released;

    // Use the hand-independent `cursor` (mouse/touch only), NOT `primary` (which
    // is hijacked by a tracked hand). The hand drives its own `LineHandAttractor`
    // separately; reading `primary` here would drag the mouse attractor onto the
    // hand — and (before the projection fix) onto a wildly off-window position —
    // breaking the "mouse and hand attractors are independent" contract.
    if let Some(cursor_window) = pointer.cursor {
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
