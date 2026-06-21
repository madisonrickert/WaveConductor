//! Synthetic input event helpers for integration tests.
//!
//! Bevy's input layer reads from `Message` buses populated by `InputPlugin`'s
//! winit integration. In tests, we synthesize the same messages directly,
//! then advance one frame so `ButtonInput<MouseButton>`, `ButtonInput<KeyCode>`,
//! and `Touches` reflect the synthesized state. Production input flows that
//! consume those resources (the project's `PointerState`, the in-house
//! `ActionInput` pipeline, the Line sketch's mouse-attractor lifecycle) run
//! end-to-end without poking their internal resources directly.
//!
//! ## When to use what
//!
//! - **Pointer-driven sketches** — [`press_left`], [`release_left`],
//!   [`move_pointer`]. Writes `MouseButtonInput` + `CursorMoved` so
//!   `Res<ButtonInput<MouseButton>>` and `PointerState.primary` update.
//! - **Keyboard navigation** — [`tap_key`], [`press_key`] /
//!   [`release_key`]. Writes `KeyboardInput`; `emit_action_input` picks it up
//!   next frame.
//! - **Touch UIs** — [`touch_start`], [`touch_move`], [`touch_end`].
//!
//! All helpers take `&mut App` and only write messages. **Call
//! `app.update()` (at least once) after each helper to let Bevy's input
//! systems process the synthesized events** before asserting on the resource
//! state.
//!
//! ## Window requirement
//!
//! Each helper attaches its event to the first `Window` entity it finds in the
//! world. Tests that consume these helpers must build their app with a Window
//! already spawned — `sketches_test_app()` in
//! `crates/wc-sketches/tests/common/mod.rs` does this. wc-core's
//! `lifecycle_test_app()` does not spawn a Window, so the synthetic helpers
//! panic there until one is added explicitly.

#![allow(
    dead_code,
    reason = "Helpers may be unused by some integration test binaries."
)]
#![allow(
    clippy::expect_used,
    reason = "expect with a clear message is appropriate in test fixtures"
)]

use bevy::input::keyboard::{Key, KeyboardInput, NativeKey};
use bevy::input::mouse::{MouseButton, MouseButtonInput, MouseMotion};
use bevy::input::touch::{ForceTouch, TouchInput, TouchPhase};
use bevy::input::ButtonState;
use bevy::math::Vec2;
use bevy::prelude::*;
use bevy::window::CursorMoved;

/// The first `Window` entity in the app. Synthetic helpers attach events to
/// this window so production code that filters by window id finds them.
///
/// Panics if no `Window` entity exists. Tests that use these helpers must
/// build their app with a `Window` already spawned — `sketches_test_app()`
/// in `wc-sketches/tests/common/mod.rs` does this.
fn primary_window(app: &mut App) -> Entity {
    let world = app.world_mut();
    let mut query = world.query_filtered::<Entity, With<Window>>();
    query
        .iter(world)
        .next()
        .expect("synthetic input helpers require a Window entity")
}

/// Write a `MouseButtonInput { Pressed }` for the given button.
///
/// Call `app.update()` after this to let Bevy's `mouse_button_input_system`
/// fold it into `Res<ButtonInput<MouseButton>>`.
pub fn press_button(app: &mut App, button: MouseButton) {
    let window = primary_window(app);
    app.world_mut().write_message(MouseButtonInput {
        button,
        state: ButtonState::Pressed,
        window,
    });
}

/// Write a `MouseButtonInput { Released }` for the given button.
///
/// Call `app.update()` after this to let Bevy's `mouse_button_input_system`
/// fold it into `Res<ButtonInput<MouseButton>>`.
pub fn release_button(app: &mut App, button: MouseButton) {
    let window = primary_window(app);
    app.world_mut().write_message(MouseButtonInput {
        button,
        state: ButtonState::Released,
        window,
    });
}

/// Convenience: press the left mouse button.
pub fn press_left(app: &mut App) {
    press_button(app, MouseButton::Left);
}

/// Convenience: release the left mouse button.
pub fn release_left(app: &mut App) {
    release_button(app, MouseButton::Left);
}

/// Move the pointer to `(x, y)` in window pixel coordinates (top-left origin,
/// +y down).
///
/// Writes `CursorMoved` (which Plan 8 Phase 0 wired into
/// `pointer_merge_system`'s mouse-source path) and `MouseMotion` (which
/// Bevy's idle-detection consumes), and updates `Window::cursor_position`
/// so subsequent ticks without a fresh `CursorMoved` still observe the
/// pointer (matches winit's persistent cursor position in production).
/// `from` supplies the previous position so the motion delta is correct;
/// pass `Vec2::ZERO` if unknown.
pub fn move_pointer(app: &mut App, x: f32, y: f32, from: Vec2) {
    let window_entity = primary_window(app);
    let position = Vec2::new(x, y);
    let delta = position - from;
    app.world_mut().write_message(CursorMoved {
        window: window_entity,
        position,
        delta: Some(delta),
    });
    if delta != Vec2::ZERO {
        app.world_mut().write_message(MouseMotion { delta });
    }
    if let Some(mut window) = app.world_mut().get_mut::<Window>(window_entity) {
        window.set_cursor_position(Some(position));
    }
}

/// Write a `KeyboardInput { Pressed }` for `key_code`.
///
/// `logical_key` is set to `Key::Unidentified(NativeKey::Unidentified)`
/// because tests don't usually care about the logical key vs scancode
/// distinction. Bevy's `keyboard_input_system` only reads `key_code`.
pub fn press_key(app: &mut App, key_code: KeyCode) {
    let window = primary_window(app);
    app.world_mut().write_message(KeyboardInput {
        key_code,
        logical_key: Key::Unidentified(NativeKey::Unidentified),
        state: ButtonState::Pressed,
        text: None,
        repeat: false,
        window,
    });
}

/// Write a `KeyboardInput { Released }` for `key_code`.
pub fn release_key(app: &mut App, key_code: KeyCode) {
    let window = primary_window(app);
    app.world_mut().write_message(KeyboardInput {
        key_code,
        logical_key: Key::Unidentified(NativeKey::Unidentified),
        state: ButtonState::Released,
        text: None,
        repeat: false,
        window,
    });
}

/// Press + release a key, with one `app.update()` in between so the press is
/// visible as a `ButtonInput<KeyCode>` `just_pressed` edge on the frame that
/// `PreUpdate`'s `emit_action_input` producer runs. Without the interleaved
/// update, the press and release collapse into a single frame; `just_pressed`
/// is never true and no `ActionInput` is emitted.
///
/// Caller is still responsible for calling `app.update()` once more afterward
/// to let consumers that read `ActionInput` messages observe the result.
pub fn tap_key(app: &mut App, key_code: KeyCode) {
    press_key(app, key_code);
    app.update();
    release_key(app, key_code);
}

/// Write a `TouchInput { Started }` for finger `touch_id` at window-space
/// `(x, y)`.
pub fn touch_start(app: &mut App, touch_id: u64, x: f32, y: f32) {
    let window = primary_window(app);
    app.world_mut().write_message(TouchInput {
        phase: TouchPhase::Started,
        position: Vec2::new(x, y),
        window,
        force: Some(ForceTouch::Normalized(1.0)),
        id: touch_id,
    });
}

/// Write a `TouchInput { Moved }` for finger `touch_id` at window-space
/// `(x, y)`.
pub fn touch_move(app: &mut App, touch_id: u64, x: f32, y: f32) {
    let window = primary_window(app);
    app.world_mut().write_message(TouchInput {
        phase: TouchPhase::Moved,
        position: Vec2::new(x, y),
        window,
        force: Some(ForceTouch::Normalized(1.0)),
        id: touch_id,
    });
}

/// Write a `TouchInput { Ended }` for finger `touch_id` at window-space
/// `(x, y)`.
pub fn touch_end(app: &mut App, touch_id: u64, x: f32, y: f32) {
    let window = primary_window(app);
    app.world_mut().write_message(TouchInput {
        phase: TouchPhase::Ended,
        position: Vec2::new(x, y),
        window,
        force: None,
        id: touch_id,
    });
}
