//! 8-hour soak test for the Dots ("Fabric") sketch.
//!
//! Required per AGENTS.md before any release tag. The harness exists so
//! Madison can run it on demand against the tagged release candidate; it
//! is `#[ignore]`-d so normal CI does not block on it.
//!
//! ## What this test does
//!
//! Builds a `sketches_test_app` (the `MinimalPlugins` + `AssetPlugin` +
//! `LinePlugin` variant used by all wc-sketches integration tests), adds
//! `DotsPlugin`, enters Dots via `Digit3` keyboard nav, then drives a
//! synthetic interaction loop: a cursor sweep on every 60th update plus a
//! periodic left-button press / release cycle. After ~1.7M updates
//! (`8 * 60 * 60 * 60`, the count that would correspond to 8 hours at
//! 60 fps), it asserts the sketch is still in `AppState::Dots` and logs
//! wall-clock elapsed for the operator's records.
//!
//! ## Known limitation
//!
//! `sketches_test_app` runs `MinimalPlugins`, not `DefaultPlugins`. That
//! means the soak validates the simulation / audio / scheduling paths
//! over millions of frames — exactly the surface area that catches slow
//! leaks in `Vec` reuse, `Local<T>` scratch state, and CPU mirror
//! drift — but it does *not* exercise the GPU render path. A future
//! pass (Plan 11+) can extend `tests/common/mod.rs` with a
//! `sketches_test_app_with_default_plugins` variant so the soak also
//! covers `ParticleComputePlugin` / `RenderApp` / `DotsPostProcessPlugin`
//! thermal behavior end-to-end. Until then, the renderer is tested
//! separately by Madison running `cargo run -p waveconductor` for the
//! actual 8-hour wall-clock window.
//!
//! ## Running the soak
//!
//! ```text
//! cargo test --release -p wc-sketches --test dots_soak -- --ignored dots_soak_8h
//! ```
//!
//! The `--release` flag matters: a debug build's per-frame cost is large
//! enough that ~1.7M `app.update()` calls take far longer than 8 hours
//! and most of that time is spent in unoptimized math, not in the
//! production-shape execution we want to soak.

#![allow(
    clippy::expect_used,
    reason = "expect with a clear message is appropriate in test code"
)]

mod common;
use common::input::{move_pointer, press_left, release_left, tap_key};
use common::sketches_test_app;

use std::time::Instant;

use bevy::input::keyboard::KeyCode;
use bevy::math::Vec2;
use bevy::prelude::*;
use wc_core::lifecycle::state::AppState;
use wc_sketches::dots::DotsPlugin;

/// Number of `app.update()` ticks the soak drives.
///
/// `8 hours × 60 minutes × 60 seconds × 60 fps = 1_728_000` updates.
/// We gate on update count rather than wall-clock time because soak
/// stability testing wants to exercise the per-frame state machine a
/// consistent number of times across machines, not for a consistent
/// wall-clock duration.
const SOAK_UPDATES: usize = 8 * 60 * 60 * 60;

/// How often to nudge the synthetic cursor. Every 60 ticks (~1s of
/// simulated wall time at 60 fps) keeps the attractor visual and
/// `DotsMouseAttractorState.power` exercised without writing a `CursorMoved`
/// message on every tick — production input doesn't either.
const POINTER_TICK_PERIOD: usize = 60;

/// Period of the press / release cycle. 600 ticks (~10s) per full cycle
/// (press for the first 300, released for the remaining 300) keeps the
/// idle-veto path active and gives `DotsMouseAttractorState.power` time to
/// decay back through zero before the next press.
const PRESS_CYCLE_PERIOD: usize = 600;

#[test]
#[ignore = "8-hour soak; run via cargo test --release -p wc-sketches --test dots_soak -- --ignored dots_soak_8h"]
fn dots_soak_8h() {
    let mut app = sketches_test_app();
    // `sketches_test_app` includes `LinePlugin` but not `DotsPlugin`.
    // Add it here so Dots lifecycle, mouse-attractor, audio-envelope, and
    // screensaver systems are registered and gate-tested over the full run.
    app.add_plugins(DotsPlugin);
    app.update();

    // Enter Dots via Digit3 keyboard nav. `nav::handle_navigation_actions`
    // begins a graceful `ReloadReason::SketchSwitch` reload rather than an
    // instant `NextState` write, so `TimeUpdateStrategy::ManualDuration` at
    // 500 ms (past `SKETCH_SWITCH_FADE_DURATION`'s 400 ms) makes the same
    // three-update settle resolve the full walk (see `line_input.rs::enter_line`
    // in the sibling `line_input.rs` test binary for the detailed phase walk).
    tap_key(&mut app, KeyCode::Digit3);
    app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
        std::time::Duration::from_millis(500),
    ));
    // `Time<Virtual>`'s default `max_delta` (250 ms) would otherwise silently
    // clamp the 500 ms manual step below `SKETCH_SWITCH_FADE_DURATION`'s
    // 400 ms, stalling the fade forever.
    app.world_mut()
        .resource_mut::<Time<bevy::time::Virtual>>()
        .set_max_delta(std::time::Duration::from_secs(1));
    for _ in 0..3 {
        app.update();
    }
    app.insert_resource(bevy::time::TimeUpdateStrategy::Automatic);
    app.world_mut()
        .resource_mut::<Time<bevy::time::Virtual>>()
        .set_max_delta(std::time::Duration::from_millis(250)); // Bevy's own default
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Dots,
        "soak prerequisite: must enter AppState::Dots before the loop"
    );

    let start = Instant::now();
    for i in 0..SOAK_UPDATES {
        if i % POINTER_TICK_PERIOD == 0 {
            // Sweep the cursor across the 1280×720 test window. Window
            // coordinates are integers; the cast precision loss / sign
            // loss is harmless because i ≤ SOAK_UPDATES ≪ 2^31.
            #[allow(
                clippy::as_conversions,
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "synthetic-pointer counter; values are bounded and integer-shaped"
            )]
            let x = (i % 1280) as f32;
            #[allow(
                clippy::as_conversions,
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "synthetic-pointer counter; values are bounded and integer-shaped"
            )]
            let y = ((i / 1280) % 720) as f32;
            move_pointer(&mut app, x, y, Vec2::ZERO);
        }

        let phase = i % PRESS_CYCLE_PERIOD;
        if phase == 0 {
            press_left(&mut app);
        } else if phase == PRESS_CYCLE_PERIOD / 2 {
            release_left(&mut app);
        }

        app.update();
    }

    // Sketch must still be in Dots at the end — proves nothing in the
    // lifecycle drifted it out, and proves the app didn't deadlock or
    // silently lose its state stack to a panic captured by Bevy's
    // schedule.
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Dots,
        "soak postcondition: sketch must remain in AppState::Dots after {SOAK_UPDATES} ticks"
    );

    let elapsed = start.elapsed();
    tracing::info!(?elapsed, updates = SOAK_UPDATES, "Dots soak complete");
}
