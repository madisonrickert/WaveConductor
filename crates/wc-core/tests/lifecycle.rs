//! Integration tests for `LifecyclePlugin`.
//!
//! Each test stands up a headless `App` with `MinimalPlugins` plus the lifecycle
//! plugin and drives it through realistic sequences using leafwing's physical
//! key input injection and manual time advancement.

// Note: the hand-tracking branch of `reset_on_interaction` is exercised by
// the `hand_tracking_frame_resets_interaction_timer` test in `tests/input.rs`,
// which is currently `#[ignore]`'d pending richer test infrastructure in
// Plan 6. Until that lands, the hand-tracking interaction-reset path has
// no active integration coverage. If you're modifying `reset_on_interaction`,
// also verify the input test re-enables cleanly.

use std::time::Duration;

use bevy::prelude::*;
use leafwing_input_manager::prelude::*;
use wc_core::lifecycle::{
    idle::InteractionTimer,
    state::{AppState, SketchActivity},
    LifecyclePlugin,
};

fn test_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    // StatesPlugin is required for the StateTransition schedule used by init_state / add_sub_state.
    // MinimalPlugins does not include it; DefaultPlugins does.
    app.add_plugins(bevy::state::app::StatesPlugin);
    app.add_plugins(bevy::input::InputPlugin); // Needed for ButtonInput resources used by leafwing.
    app.add_plugins(LifecyclePlugin);
    app
}

/// Helper: inject a physical key press into the world, run one update tick,
/// then inject a key release. This mirrors leafwing's own test patterns for
/// driving `ActionState` through physical input rather than direct state mutation,
/// which is required because leafwing's `update_action_state` system overwrites
/// any direct `ActionState::press()` calls during the same frame.
///
/// Uses leafwing's `Buttonlike::press(world)` API which internally writes to
/// `Messages<KeyboardInput>`.
fn press_key(app: &mut App, key: KeyCode) {
    key.press(app.world_mut());
    app.update();
    key.release(app.world_mut());
}

#[test]
fn defaults_to_home_state() {
    let mut app = test_app();
    app.update();
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Home
    );
}

#[test]
fn select_line_transitions_into_line_state() {
    let mut app = test_app();
    app.update();
    press_key(&mut app, KeyCode::Digit1);
    // Pending transitions resolve on the next update tick.
    app.update();
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Line
    );
}

#[test]
fn navigate_home_returns_to_home() {
    let mut app = test_app();
    app.update();
    press_key(&mut app, KeyCode::Digit2); // Select Flame
    app.update();
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Flame
    );
    press_key(&mut app, KeyCode::Escape); // Navigate Home
    app.update();
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Home
    );
}

#[test]
fn next_and_prev_cycle_through_sketches() {
    let mut app = test_app();
    app.update();
    // Home → next (X key) → Line
    press_key(&mut app, KeyCode::KeyX);
    app.update();
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Line
    );
    // Line → next → Flame
    press_key(&mut app, KeyCode::KeyX);
    app.update();
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Flame
    );
    // Wrap around: 5 nexts from Flame should land back on Flame.
    for _ in 0..5 {
        press_key(&mut app, KeyCode::KeyX);
        app.update();
    }
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Flame
    );
    // Prev from Flame → Line (Z key)
    press_key(&mut app, KeyCode::KeyZ);
    app.update();
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Line
    );
}

#[test]
fn idle_transitions_after_threshold() {
    let mut app = test_app();
    // Configure a short idle threshold so the test is fast.
    app.world_mut()
        .resource_mut::<InteractionTimer>()
        .idle_threshold = Duration::from_millis(50);
    app.world_mut()
        .resource_mut::<InteractionTimer>()
        .screensaver_threshold = Duration::from_millis(50);

    // Navigate to Line sketch so SketchActivity sub-state activates.
    app.update();
    press_key(&mut app, KeyCode::Digit1);
    app.update(); // StateTransition resolves → AppState::Line, SketchActivity::Active
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Line
    );
    assert_eq!(
        *app.world().resource::<State<SketchActivity>>().get(),
        SketchActivity::Active,
    );

    // Advance time in controlled steps so `idle_for` crosses the 50 ms
    // threshold without relying on wall-clock duration.
    //
    // Strategy: record `now` as the interaction baseline, then switch to
    // ManualDuration(80 ms) so each subsequent update advances elapsed by
    // 80 ms, making `idle_for = 80 ms > 50 ms` on the very next frame.
    //
    // NOTE: In Bevy 0.18, `Time<()>` (the generic clock) is overwritten each
    // frame by `update_virtual_time` which derives it from `Time<Virtual>` and
    // `Time<Real>`. Direct `Time::advance_by` is therefore NOT the right way to
    // control elapsed time in tests. Instead we use `TimeUpdateStrategy::ManualDuration`
    // to configure how much the `Time<Real>` advances each frame; virtual time
    // follows suit via `update_virtual_time`.
    //
    // Because `reset_on_interaction` now calls `.read()` (not `.is_empty()`),
    // the cursor advances past the already-processed release event, so the
    // timer is not disturbed during the "idle" updates below.
    {
        let now = app.world().resource::<Time>().elapsed();
        app.world_mut().resource_mut::<InteractionTimer>().mark(now);
    }
    app.world_mut()
        .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            Duration::from_millis(80),
        ));

    // Two updates: first queues the Idle transition, second resolves it.
    app.update(); // advance_activity: idle_for(80 ms) > 50 ms → NextState(Idle) queued
    app.update(); // StateTransition resolves → SketchActivity::Idle

    assert_eq!(
        *app.world().resource::<State<SketchActivity>>().get(),
        SketchActivity::Idle,
    );
}
