//! Discrete hand gesture events.
//!
//! Emitted by [`crate::input::systems::detect_gestures`] when transitions
//! in [`crate::input::state::HandTrackingState`] cross the pinch/grab
//! thresholds defined in [`crate::input::button`].

use std::time::Duration;

use bevy::prelude::*;

use super::button::HandButton;

/// One discrete gesture moment.
///
/// Examples: a pinch just closed, a pinch just opened, a grab just closed.
/// Consumed by sketches that want to fire one-shot effects on gesture edges.
#[derive(Message, Debug, Clone, Copy, PartialEq)]
pub enum HandGestureEvent {
    /// `button` just transitioned from released → pressed.
    Pressed {
        /// The button that was pressed.
        button: HandButton,
        /// Time of the press event (Bevy elapsed time).
        at: Duration,
    },
    /// `button` just transitioned from pressed → released.
    Released {
        /// The button that was released.
        button: HandButton,
        /// Time of the release event (Bevy elapsed time).
        at: Duration,
    },
}
