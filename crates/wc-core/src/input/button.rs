//! Hand-derived discrete buttons.
//!
//! Bridges the analog `pinch_strength` and `grab_strength` per-hand values to
//! a Bevy `ButtonInput<HandButton>` resource, so sketches can use
//! `pinch.just_pressed(HandButton::LeftPinch)` with the same idioms they use
//! for mouse buttons.
//!
//! ## Thresholds
//!
//! Pinch and grab are continuous in `[0.0, 1.0]`. We declare a button "pressed"
//! when the strength crosses [`PRESS_THRESHOLD`] from below, and "released"
//! when it falls below [`RELEASE_THRESHOLD`]. The hysteresis gap prevents
//! flicker around the boundary.

use bevy::reflect::Reflect;

/// Strength above which a pinch/grab is considered pressed.
pub const PRESS_THRESHOLD: f32 = 0.8;
/// Strength below which a pressed pinch/grab is considered released.
pub const RELEASE_THRESHOLD: f32 = 0.5;

/// Discrete hand-derived buttons.
///
/// Exposed as `Res<ButtonInput<HandButton>>` so the gesture layer can use
/// `pinch.just_pressed(HandButton::LeftPinch)` with the same idioms used for
/// mouse buttons. Any future *action* binding (e.g., hand-gesture hotkeys)
/// would extend the in-house `crate::lifecycle::action_map`, which is
/// currently keyboard-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Reflect)]
pub enum HandButton {
    /// Pinch gesture on the left hand (thumb–index proximity).
    LeftPinch,
    /// Pinch gesture on the right hand.
    RightPinch,
    /// Grab gesture on the left hand (fist closure).
    LeftGrab,
    /// Grab gesture on the right hand.
    RightGrab,
}
