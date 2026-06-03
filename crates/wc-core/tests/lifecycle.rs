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

mod common;
use common::app::lifecycle_test_app;
use common::lifecycle::arm_idle_timeline;

use bevy::prelude::*;
use leafwing_input_manager::prelude::*;
use wc_core::lifecycle::state::{AppState, SketchActivity};

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
    let mut app = lifecycle_test_app();
    app.update();
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Home
    );
}

#[test]
fn select_line_transitions_into_line_state() {
    let mut app = lifecycle_test_app();
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
    let mut app = lifecycle_test_app();
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
    let mut app = lifecycle_test_app();
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
    let mut app = lifecycle_test_app();

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

    // Arm the idle timeline: shrinks `idle_threshold` to 50 ms, sets
    // `screensaver_threshold` far enough out to avoid overshoot, marks the
    // interaction timer at `now`, and installs
    // `TimeUpdateStrategy::ManualDuration(80 ms)` so each subsequent update
    // advances elapsed time deterministically.
    //
    // NOTE: In Bevy 0.18, `Time<()>` (the generic clock) is overwritten each
    // frame by `update_virtual_time` which derives it from `Time<Virtual>` and
    // `Time<Real>`. Direct `Time::advance_by` is therefore NOT the right way to
    // control elapsed time in tests; `arm_idle_timeline` uses the correct
    // `TimeUpdateStrategy::ManualDuration` pattern.
    //
    // Because `reset_on_interaction` now calls `.read()` (not `.is_empty()`),
    // the cursor advances past the already-processed release event, so the
    // timer is not disturbed during the "idle" updates below.
    arm_idle_timeline(&mut app);

    // Two updates: first queues the Idle transition, second resolves it.
    app.update(); // advance_activity: idle_for(80 ms) > 50 ms → NextState(Idle) queued
    app.update(); // StateTransition resolves → SketchActivity::Idle

    assert_eq!(
        *app.world().resource::<State<SketchActivity>>().get(),
        SketchActivity::Idle,
    );
}

#[test]
fn empty_leap_frames_do_not_reset_idle_timer() {
    use bevy::prelude::*;
    use smallvec::SmallVec;
    use std::time::Duration;
    use wc_core::input::provider::ProviderId;
    use wc_core::input::state::HandTrackingFrame;
    use wc_core::input::synthetic::synthetic_hand_frame;
    use wc_core::lifecycle::idle::{reset_on_interaction, InteractionTimer};

    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.init_resource::<InteractionTimer>();
    // Register HandTrackingFrame as a message type so MessageReader<HandTrackingFrame>
    // can be used by reset_on_interaction.
    app.add_message::<HandTrackingFrame>();
    // Also register the other input messages reset_on_interaction reads, otherwise
    // those MessageReader parameters fail to initialize.
    app.add_message::<bevy::input::mouse::MouseMotion>();
    app.add_message::<bevy::input::mouse::MouseButtonInput>();
    app.add_message::<bevy::input::keyboard::KeyboardInput>();
    app.add_message::<bevy::input::touch::TouchInput>();
    app.add_systems(Update, reset_on_interaction);

    // Advance the clock so "last interaction" can be distinguished from ZERO.
    app.update();
    let baseline = app
        .world()
        .resource::<InteractionTimer>()
        .last_interaction();

    // An EMPTY tracking frame (service running, no hand) must NOT reset.
    {
        let mut msgs = app
            .world_mut()
            .resource_mut::<Messages<HandTrackingFrame>>();
        msgs.write(HandTrackingFrame {
            provider: ProviderId::Leap,
            hands: SmallVec::new(),
            timestamp: Duration::ZERO,
        });
    }
    app.update();
    assert_eq!(
        app.world()
            .resource::<InteractionTimer>()
            .last_interaction(),
        baseline,
        "empty Leap frame should not count as interaction",
    );

    // A HAND-bearing frame MUST reset.
    let now = app.world().resource::<Time>().elapsed();
    {
        let mut msgs = app
            .world_mut()
            .resource_mut::<Messages<HandTrackingFrame>>();
        msgs.write(synthetic_hand_frame(now));
    }
    app.update();
    assert!(
        app.world()
            .resource::<InteractionTimer>()
            .last_interaction()
            > baseline,
        "hand-bearing frame should count as interaction",
    );
}
