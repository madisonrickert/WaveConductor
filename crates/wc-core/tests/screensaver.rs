//! Integration tests for the screensaver / attract-mode framework (Plan 11.8).
//!
//! Exercises the framework end-to-end against the real lifecycle plugin (state
//! machine + `ScreensaverPlugin`) under `MinimalPlugins`:
//!
//! - `ScreensaverActive` marker is inserted on entering `Screensaver` and
//!   removed on leaving.
//! - The fade envelope ramps up in `Screensaver` and back down on return to
//!   `Active`.
//! - The framework does zero attract work outside `Screensaver` (no marker, fade
//!   at 0).
//!
//! `MinimalPlugins` omits `EguiPlugin` and `WinitPlugin`, so the caption overlay
//! and present-rate throttle systems are inert (they early-return when their
//! resources/contexts are absent) — the lifecycle, fade, and marker logic are
//! still fully exercised. These complement the unit tests colocated in the
//! framework modules.

use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use wc_core::lifecycle::screensaver::fade::ScreensaverFade;
use wc_core::lifecycle::screensaver::ScreensaverActive;
use wc_core::lifecycle::state::{AppState, SketchActivity};
use wc_core::lifecycle::LifecyclePlugin;

/// Build a minimal app exercising the lifecycle + screensaver plugins.
fn test_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_plugins(bevy::input::InputPlugin);
    app.add_plugins(StatesPlugin);
    // `WinitSettings` is normally inserted by `WinitPlugin`; insert a default so
    // the present-rate throttle system's `ResMut<WinitSettings>` param resolves
    // under MinimalPlugins (which omits WinitPlugin). Referenced via the
    // `bevy::winit` re-export (the `winit_config` submodule itself is private).
    app.insert_resource(bevy::winit::WinitSettings::default());
    app.add_plugins(LifecyclePlugin);
    app
}

/// Transition into Line, then drive the sub-state to `target` through the real
/// idle state machine (`advance_activity`), settling a few frames.
///
/// We don't set `NextState<SketchActivity>` by hand: `advance_activity` runs
/// every `Update` and would overwrite a manual write from the idle timer. To
/// reach `Screensaver` we instead collapse both idle thresholds to zero so the
/// timer naturally targets it (the same lever `apply_force_screensaver` uses in
/// capture); for `Active` we leave the default 30 s thresholds so the timer
/// holds `Active`. This exercises the production transition path end-to-end.
fn enter_line_activity(app: &mut App, target: SketchActivity) {
    use std::time::Duration;
    use wc_core::lifecycle::idle::InteractionTimer;

    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Line);
    app.update(); // OnEnter(Line); SketchActivity defaults to Active

    if target == SketchActivity::Screensaver {
        let mut timer = app.world_mut().resource_mut::<InteractionTimer>();
        timer.idle_threshold = Duration::ZERO;
        timer.screensaver_threshold = Duration::ZERO;
    }
    // A few frames so `advance_activity` transitions and the Update-schedule
    // framework systems observe the new state.
    for _ in 0..3 {
        app.update();
    }
}

/// Return Line to `Active` from the screensaver, as a fresh interaction would:
/// restore the default idle thresholds and mark "interacted now" so the idle
/// timer targets `Active` again. Then settle a few frames.
fn return_to_active(app: &mut App) {
    use std::time::Duration;
    use wc_core::lifecycle::idle::InteractionTimer;

    let now = app.world().resource::<Time>().elapsed();
    let mut timer = app.world_mut().resource_mut::<InteractionTimer>();
    timer.idle_threshold = Duration::from_secs(30);
    timer.screensaver_threshold = Duration::from_secs(30);
    timer.mark(now);
    for _ in 0..3 {
        app.update();
    }
}

#[test]
fn screensaver_active_marker_inserted_on_enter() {
    let mut app = test_app();
    assert!(
        app.world().get_resource::<ScreensaverActive>().is_none(),
        "marker must be absent at Home"
    );
    enter_line_activity(&mut app, SketchActivity::Screensaver);
    assert!(
        app.world().get_resource::<ScreensaverActive>().is_some(),
        "ScreensaverActive must be inserted on entering Screensaver"
    );
}

#[test]
fn screensaver_active_marker_removed_on_exit() {
    let mut app = test_app();
    enter_line_activity(&mut app, SketchActivity::Screensaver);
    assert!(app.world().get_resource::<ScreensaverActive>().is_some());

    return_to_active(&mut app);
    assert!(
        app.world().get_resource::<ScreensaverActive>().is_none(),
        "marker must be removed on leaving Screensaver"
    );
}

#[test]
fn fade_rises_in_screensaver_and_falls_on_return() {
    // Drive the fade with a deterministic virtual clock rather than wall-clock
    // `app.update()` deltas (which are microseconds in a tight loop and make the
    // ramp magnitude flaky). We advance `Time<Virtual>` by a fixed step each
    // frame so the linear ramp is reproducible. The pure ramp math itself is
    // unit-tested in `fade.rs`; here we assert the *state-driven direction*:
    // rises while `Screensaver`, falls after returning to `Active`.
    use bevy::time::{Time, TimeUpdateStrategy, Virtual};
    use std::time::Duration;

    let mut app = test_app();
    // Fixed 100 ms per frame: ~15 frames spans the 1.5 s fade duration.
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
        100,
    )));

    enter_line_activity(&mut app, SketchActivity::Screensaver);
    for _ in 0..30 {
        app.update();
    }
    let lit = app.world().resource::<ScreensaverFade>().alpha();
    assert!(
        lit > 0.5,
        "fade should rise in screensaver, got {lit} (virt elapsed {:?})",
        app.world().resource::<Time<Virtual>>().elapsed()
    );

    // Return to Active; the fade ramps back toward 0 over the next frames.
    return_to_active(&mut app);
    for _ in 0..30 {
        app.update();
    }
    let dark = app.world().resource::<ScreensaverFade>().alpha();
    assert!(
        dark < lit,
        "fade should fall after leaving screensaver (lit={lit}, dark={dark})"
    );
}

#[test]
fn present_rate_throttles_in_screensaver_and_restores_prior_modes() {
    use bevy::winit::{UpdateMode, WinitSettings};
    use std::time::Duration;

    let mut app = test_app();
    // `test_app` inserts `WinitSettings::default()` (= `game()`): focused
    // `Continuous`, unfocused `reactive_low_power(1/60)`. The unfocused mode is
    // deliberately NOT `Continuous` — this is the regression the restore-path
    // test guards: a restore that *assumes* `Continuous` would clobber it.
    let prior_focused = app.world().resource::<WinitSettings>().focused_mode;
    let prior_unfocused = app.world().resource::<WinitSettings>().unfocused_mode;
    assert_ne!(
        prior_unfocused,
        UpdateMode::Continuous,
        "precondition: the baseline unfocused mode must differ from Continuous \
         for this test to detect a hard-coded restore"
    );

    // Entering the screensaver throttles the present rate: a reactive wait at
    // or below the 30 fps screensaver cap (the default tier is Cool).
    enter_line_activity(&mut app, SketchActivity::Screensaver);
    let throttled = app.world().resource::<WinitSettings>();
    assert!(
        matches!(
            throttled.focused_mode,
            UpdateMode::Reactive { wait, .. } if wait >= Duration::from_millis(33)
        ),
        "screensaver must switch to a reactive wait capping presents at <= 30 fps, got {:?}",
        throttled.focused_mode
    );

    // Leaving the screensaver restores the modes that were in effect *before*
    // it — exactly, both focused and unfocused — not an assumed Continuous.
    return_to_active(&mut app);
    let restored = app.world().resource::<WinitSettings>();
    assert_eq!(
        restored.focused_mode, prior_focused,
        "focused mode must be restored to its pre-screensaver value"
    );
    assert_eq!(
        restored.unfocused_mode, prior_unfocused,
        "unfocused mode must be restored to its pre-screensaver value \
         (not clobbered to Continuous)"
    );
}

#[test]
#[expect(
    clippy::float_cmp,
    reason = "fade alpha is exactly 0.0 when the screensaver never ramps"
)]
fn no_marker_or_fade_outside_screensaver() {
    let mut app = test_app();
    enter_line_activity(&mut app, SketchActivity::Active);
    for _ in 0..50 {
        app.update();
    }
    assert!(
        app.world().get_resource::<ScreensaverActive>().is_none(),
        "no marker while Active"
    );
    assert_eq!(
        app.world().resource::<ScreensaverFade>().alpha(),
        0.0,
        "fade stays at 0 while Active"
    );
}
