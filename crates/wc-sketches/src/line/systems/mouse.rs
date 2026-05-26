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

/// Tracks pointer button transitions and updates [`MouseAttractorState`].
///
/// Matches v4: only `just_pressed` sets `power = MOUSE_POWER_PRESS`. Held with
/// a stationary mouse decays to floor; mousemove just updates position. The
/// previous behavior of re-asserting `power = MOUSE_POWER_PRESS` every frame
/// the button was held masked the decay system and is intentionally removed.
pub fn update_mouse_attractor(
    pointer: Res<'_, PointerState>,
    mouse_buttons: Res<'_, bevy::input::ButtonInput<bevy::input::mouse::MouseButton>>,
    touches: Res<'_, bevy::input::touch::Touches>,
    window: Single<'_, '_, &Window>,
    mut state: ResMut<'_, MouseAttractorState>,
) {
    let just_pressed = mouse_buttons.just_pressed(bevy::input::mouse::MouseButton::Left)
        || touches.iter_just_pressed().next().is_some();
    // Any active touch counts as "held"; iter() is non-consuming.
    // (Held is read for future signal hooks but no longer affects power.)
    let _held = mouse_buttons.pressed(bevy::input::mouse::MouseButton::Left)
        || touches.iter().next().is_some();

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
