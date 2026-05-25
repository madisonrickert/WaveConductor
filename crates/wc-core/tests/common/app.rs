//! Shared `App` builders for wc-core integration tests.
//!
//! Eliminates the byte-identical `test_app()` / `build_app()` helpers that
//! accumulated across `tests/lifecycle.rs` and `tests/lifecycle_idle_veto.rs`,
//! and provides a `WindowFixture`-spawning variant that
//! `wc-sketches` tests can re-import via their own `tests/common/` re-export.
//!
//! ## Layers
//!
//! - [`lifecycle_test_app`] — the minimum Bevy plumbing needed to exercise
//!   `LifecyclePlugin`: `MinimalPlugins`, `InputPlugin`, `StatesPlugin`,
//!   `LifecyclePlugin` itself. Used by lifecycle and idle-veto tests.
//! - [`arm_idle_timeline`] — install `TimeUpdateStrategy::ManualDuration` so
//!   `Time<Virtual>` advances deterministically; mark the interaction timer
//!   at `now`; shrink `idle_threshold` so subsequent updates can trip Idle
//!   in a small number of ticks. Mirrors the inline pattern previously
//!   duplicated across three test files (Plan 7 carry-forward #39).
//!
//! `sketches_test_app` (the wc-sketches variant with `AssetPlugin` /
//! `MeshPlugin` / `LinePlugin`) lives in `crates/wc-sketches/tests/common/mod.rs`
//! because it depends on `wc-sketches` types and Cargo's per-crate test
//! isolation prevents cross-crate `tests/common/` sharing.

#![allow(
    dead_code,
    reason = "Test fixtures may be unused by some integration test binaries."
)]

use std::time::Duration;

use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use bevy::time::TimeUpdateStrategy;

/// Build the standard wc-core lifecycle test app.
///
/// `MinimalPlugins + InputPlugin + StatesPlugin + LifecyclePlugin`. The
/// lifecycle plugin internally registers `InputManagerPlugin`,
/// `WaveConductorAction` map, `ActionState`, `InteractionTimer`,
/// `IdleVetoes`, and `HandTrackingFrame` message — no caller-side setup
/// needed.
pub fn lifecycle_test_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_plugins(bevy::input::InputPlugin);
    app.add_plugins(StatesPlugin);
    app.add_plugins(wc_core::lifecycle::LifecyclePlugin);
    app
}

/// Configure an app so its idle-transition tests can advance time
/// deterministically over a handful of update ticks.
///
/// Installs `TimeUpdateStrategy::ManualDuration(80 ms)`, marks the interaction
/// timer at `Time::elapsed()` so `idle_for` starts at zero, and shrinks
/// `idle_threshold` to 50 ms so two `app.update()` calls cross the threshold.
/// Caller is expected to call `app.update()` at least twice afterward to
/// observe the Idle transition.
///
/// **Required:** the app must already have `LifecyclePlugin` registered.
pub fn arm_idle_timeline(app: &mut App) {
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
        80,
    )));
    let now = app.world().resource::<Time>().elapsed();
    let mut timer = app
        .world_mut()
        .resource_mut::<wc_core::lifecycle::idle::InteractionTimer>();
    timer.mark(now);
    timer.idle_threshold = Duration::from_millis(50);
    timer.screensaver_threshold = Duration::from_secs(60);
}
