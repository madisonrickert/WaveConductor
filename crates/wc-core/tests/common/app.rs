//! Shared `App` builders for wc-core integration tests.
//!
//! Eliminates the byte-identical `test_app()` / `build_app()` helpers that
//! accumulated across `tests/lifecycle.rs` and `tests/lifecycle_idle_veto.rs`.
//!
//! ## Layers
//!
//! - [`lifecycle_test_app`] — the minimum Bevy plumbing needed to exercise
//!   `LifecyclePlugin`: `MinimalPlugins`, `InputPlugin`, `StatesPlugin`,
//!   `LifecyclePlugin` itself. Used by lifecycle and idle-veto tests.
//! - `arm_idle_timeline` — canonical implementation now lives in
//!   [`super::lifecycle`]. Import from `common::lifecycle::arm_idle_timeline`.
//!   (Plan 11 Phase 0, carry-forward #39.)
//!
//! `sketches_test_app` (the wc-sketches variant with `AssetPlugin` /
//! `MeshPlugin` / `LinePlugin`) lives in `crates/wc-sketches/tests/common/mod.rs`
//! because it depends on `wc-sketches` types and Cargo's per-crate test
//! isolation prevents cross-crate `tests/common/` sharing.

#![allow(
    dead_code,
    reason = "Test fixtures may be unused by some integration test binaries."
)]

use bevy::prelude::*;
use bevy::state::app::StatesPlugin;

/// Build the standard wc-core lifecycle test app.
///
/// `MinimalPlugins + InputPlugin + StatesPlugin + LifecyclePlugin`. The
/// lifecycle plugin internally registers `InputManagerPlugin`,
/// `WaveConductorAction` map, `ActionState`, `InteractionTimer`,
/// `IdleVetoes`, and `HandTrackingFrame` message — no caller-side setup
/// needed.
///
/// A bare `Window` entity is spawned so `common::input` helpers (which call
/// `primary_window`) work without a full `WindowPlugin`. The window field in
/// synthetic `KeyboardInput` messages is only used for OS-level focus
/// routing; Bevy's `keyboard_input_system` updates `ButtonInput<KeyCode>`
/// from all events regardless of window.
pub fn lifecycle_test_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_plugins(bevy::input::InputPlugin);
    app.add_plugins(StatesPlugin);
    app.add_plugins(wc_core::lifecycle::LifecyclePlugin);
    // Satisfy `primary_window()` in `common::input` helpers.
    app.world_mut().spawn(bevy::window::Window::default());
    app
}
