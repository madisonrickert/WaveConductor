//! Shared idle-timeline arming helper for wc-core and wc-sketches lifecycle
//! tests.
//!
//! Canonical home for [`arm_idle_timeline`] so both crate test suites share
//! a single implementation:
//!
//! - `wc-core` integration tests import via `common::lifecycle::arm_idle_timeline`.
//! - `wc-sketches` integration tests import via a `#[path]` re-export in
//!   `crates/wc-sketches/tests/common/mod.rs`.

#![allow(
    dead_code,
    reason = "Test fixtures may be unused by some integration test binaries."
)]

use std::time::Duration;

use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;

/// Configure an app so its idle-transition tests can advance time
/// deterministically over a handful of update ticks.
///
/// Installs `TimeUpdateStrategy::ManualDuration(80 ms)`, marks the interaction
/// timer at `Time::elapsed()` so `idle_for` starts at zero, and shrinks
/// `idle_threshold` to 50 ms (with `screensaver_threshold` bumped to 60 s
/// so accumulated ticks during the test don't overshoot into Screensaver).
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
    timer.screensaver_threshold = Duration::from_mins(1);
}
