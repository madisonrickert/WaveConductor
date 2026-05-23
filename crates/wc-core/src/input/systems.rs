//! `PreUpdate` systems for the hand-tracking plugin.
//!
//! Three systems, chained:
//!
//! 1. [`poll_active_provider`] — calls `provider.poll`, emitting raw frames.
//! 2. [`update_hand_tracking_state`] — folds raw frames into the
//!    [`HandTrackingState`] resource and [`ButtonInput<HandButton>`] resource.
//! 3. [`detect_gestures`] — examines previous-vs-current button state and
//!    emits [`HandGestureEvent`] for each transition.
//!
//! All three run in `PreUpdate` under the same `InputSystems` set Bevy uses
//! for its own input systems, so downstream `Update` consumers see fresh
//! state.

use bevy::input::ButtonInput;
use bevy::prelude::*;

use super::button::{HandButton, PRESS_THRESHOLD, RELEASE_THRESHOLD};
use super::gesture::HandGestureEvent;
use super::provider::ActiveProvider;
use super::state::{HandTrackingFrame, HandTrackingState};

/// Calls `provider.poll`, emitting frames into `Messages<HandTrackingFrame>`.
///
/// Runs first in the chain so subsequent systems see this frame's data.
pub fn poll_active_provider(
    time: Res<'_, Time>,
    mut provider: ResMut<'_, ActiveProvider>,
    mut frames: ResMut<'_, Messages<HandTrackingFrame>>,
) {
    provider.inner.poll(time.elapsed(), frames.as_mut());
}

/// Folds raw frames into the [`HandTrackingState`] resource and updates the
/// [`ButtonInput<HandButton>`] resource based on pinch/grab strength
/// crossings.
///
/// Hysteresis: a button is `press`'d when strength rises above
/// [`PRESS_THRESHOLD`], `release`'d when it falls below [`RELEASE_THRESHOLD`].
/// The gap prevents flicker around the boundary.
pub fn update_hand_tracking_state(
    mut reader: MessageReader<'_, '_, HandTrackingFrame>,
    mut state: ResMut<'_, HandTrackingState>,
    mut buttons: ResMut<'_, ButtonInput<HandButton>>,
) {
    // Clear last-frame edge state before processing new events.
    buttons.bypass_change_detection().clear();

    // Process all frames that arrived this tick (typically 1).
    for frame in reader.read() {
        state.ingest(frame);
    }

    // Update button state from the now-current HandTrackingState. We re-derive
    // every frame from continuous strengths rather than tracking edges in the
    // provider — this keeps the truth in one place.
    for hand in state.iter() {
        update_button(
            &mut buttons,
            pick_button(hand.chirality, false),
            hand.pinch_strength,
        );
        update_button(
            &mut buttons,
            pick_button(hand.chirality, true),
            hand.grab_strength,
        );
    }
}

/// Examines `ButtonInput<HandButton>::just_pressed` / `just_released` and
/// emits a [`HandGestureEvent`] for each.
pub fn detect_gestures(
    time: Res<'_, Time>,
    buttons: Res<'_, ButtonInput<HandButton>>,
    mut events: ResMut<'_, Messages<HandGestureEvent>>,
) {
    for button in buttons.get_just_pressed() {
        events.write(HandGestureEvent::Pressed {
            button: *button,
            at: time.elapsed(),
        });
    }
    for button in buttons.get_just_released() {
        events.write(HandGestureEvent::Released {
            button: *button,
            at: time.elapsed(),
        });
    }
}

// ---- helpers ----

fn pick_button(chirality: super::hand::Chirality, is_grab: bool) -> HandButton {
    use super::hand::Chirality;
    match (chirality, is_grab) {
        (Chirality::Left, false) => HandButton::LeftPinch,
        (Chirality::Right, false) => HandButton::RightPinch,
        (Chirality::Left, true) => HandButton::LeftGrab,
        (Chirality::Right, true) => HandButton::RightGrab,
    }
}

fn update_button(buttons: &mut ButtonInput<HandButton>, button: HandButton, strength: f32) {
    let was_pressed = buttons.pressed(button);
    if !was_pressed && strength >= PRESS_THRESHOLD {
        buttons.press(button);
    } else if was_pressed && strength < RELEASE_THRESHOLD {
        buttons.release(button);
    }
}
