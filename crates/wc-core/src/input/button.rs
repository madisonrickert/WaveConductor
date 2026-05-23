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
/// Used as the key type for `Res<ButtonInput<HandButton>>` and as a binding
/// source in leafwing `InputMap`s once future sketches need them.
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

// leafwing 0.20 Buttonlike integration.
//
// leafwing-input-manager allows custom Buttonlike types to be bound in
// `InputMap`s. The blessed pattern for "resource-backed" custom buttons in
// 0.20 is to:
//
//   1. Register the type with `app.register_buttonlike_input::<HandButton>()`
//      (or equivalent), so leafwing's `CentralInputStore` queries the right
//      backing resource.
//   2. Implement `Buttonlike` to look up the pressed-state via the registered
//      input store.
//
// Plan 6 (Line sketch) will be the first consumer that needs leafwing
// HandButton bindings, so the full integration is deferred there. For Plan 3
// we only ensure `Res<ButtonInput<HandButton>>` is populated correctly — the
// `Buttonlike` derivation can be added once leafwing 0.20's exact registration
// API has been validated against a real binding.
//
// If you need to ship the Buttonlike impl in this plan, consult
// `cargo doc -p leafwing-input-manager --open` and look for the
// `register_buttonlike_input` or equivalent method on `App`, and the trait
// requirements for `Buttonlike` (the exact method signatures changed between
// 0.16 and 0.20).
