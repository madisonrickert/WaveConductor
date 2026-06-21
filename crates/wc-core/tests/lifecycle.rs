//! Integration tests for `LifecyclePlugin`.
//!
//! Each test stands up a headless `App` with `MinimalPlugins` plus the lifecycle
//! plugin and drives it through realistic sequences using physical key input
//! injection and manual time advancement.

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
use wc_core::lifecycle::state::{AppState, SketchActivity};

use common::input::press_key as send_press;
use common::input::release_key as send_release;

/// Inject a physical key press, run one update tick (so the `PreUpdate` producer
/// emits the action and the Update consumers act), then release.
fn press_key(app: &mut App, key: KeyCode) {
    send_press(app, key);
    app.update();
    send_release(app, key);
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

/// Directly injecting `ActionInput{StartScreensaver, Pressed}` via
/// `write_message` must rewind the timer through `skip_to_screensaver`.
///
/// This test bypasses `emit_action_input` to isolate whether
/// `skip_to_screensaver` correctly reads `MessageReader<ActionInput>`.
///
/// The clock is advanced to 65 s of virtual time before the chord so
/// `rewind_past_screensaver` (which does `now.saturating_sub(threshold)`) has
/// room to produce a non-zero `last_interaction` and `idle_for` can reach the
/// 60 s total threshold.
///
/// Note: `Time<Virtual>` caps each tick at `max_delta = 250 ms` by default.
/// We raise it to 70 s so `ManualDuration(65 s)` can advance the full amount
/// in a single `app.update()`.
#[test]
fn direct_action_input_rewinds_timer() {
    use bevy::time::{TimeUpdateStrategy, Virtual};
    use std::time::Duration;
    use wc_core::lifecycle::action_map::{ActionInput, ActionPhase};
    use wc_core::lifecycle::actions::WaveConductorAction;
    use wc_core::lifecycle::idle::InteractionTimer;

    let mut app = lifecycle_test_app();
    // Raise the virtual-time max_delta cap so one tick can jump 65 s.
    app.world_mut()
        .resource_mut::<Time<Virtual>>()
        .set_max_delta(Duration::from_secs(70));
    // Advance virtual clock to 65 s (Home state; advance_activity is a no-op).
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(65)));
    app.update(); // time.elapsed() ≈ 65 s

    {
        let now = app.world().resource::<Time>().elapsed();
        let mut timer = app.world_mut().resource_mut::<InteractionTimer>();
        timer.idle_threshold = Duration::from_secs(30);
        timer.screensaver_threshold = Duration::from_secs(30);
        timer.mark(now); // freshly interacted at ~65 s
    }

    // Inject ActionInput directly — no keyboard press, no producer needed.
    app.world_mut().write_message(ActionInput {
        action: WaveConductorAction::StartScreensaver,
        phase: ActionPhase::Pressed,
    });
    app.update(); // time advances to ~130 s; skip_to_screensaver rewinds to ~70 s

    let now = app.world().resource::<Time>().elapsed();
    let idle_time = app.world().resource::<InteractionTimer>().idle_for(now);
    let total_threshold = {
        let t = app.world().resource::<InteractionTimer>();
        t.idle_threshold + t.screensaver_threshold
    };
    assert!(
        idle_time >= total_threshold,
        "direct ActionInput must rewind timer; idle_for={idle_time:?}, threshold={total_threshold:?}"
    );
}

/// The Shift+S chord must arm the screensaver skip and rewind the
/// [`InteractionTimer`] past both thresholds within the same frame.
///
/// The `action_map` producer requires both `ShiftLeft` (held) and `KeyS`
/// (just-pressed) to be observed in the same `PreUpdate` tick, so both
/// `press_key` calls must precede the single `app.update()`.
///
/// The clock is advanced to 65 s of virtual time before the chord (raising
/// `Time<Virtual>::max_delta` first, since the default cap is 250 ms) so
/// `rewind_past_screensaver` has room to produce a non-zero `last_interaction`.
#[test]
fn shift_s_chord_arms_screensaver_skip_and_rewinds_timer() {
    use bevy::time::{TimeUpdateStrategy, Virtual};
    use std::time::Duration;
    use wc_core::lifecycle::idle::InteractionTimer;

    let mut app = lifecycle_test_app();
    // Raise virtual-time max_delta cap so one ManualDuration(65 s) tick fully advances.
    app.world_mut()
        .resource_mut::<Time<Virtual>>()
        .set_max_delta(Duration::from_secs(70));
    // Advance virtual clock to 65 s (Home state; advance_activity is a no-op).
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(65)));
    app.update(); // time.elapsed() ≈ 65 s

    // Set generous thresholds and mark the timer fresh.
    {
        let now = app.world().resource::<Time>().elapsed();
        let mut timer = app.world_mut().resource_mut::<InteractionTimer>();
        timer.idle_threshold = Duration::from_secs(30);
        timer.screensaver_threshold = Duration::from_secs(30);
        timer.mark(now);
    }

    // Precondition: timer is fresh.
    let now = app.world().resource::<Time>().elapsed();
    let total_threshold = {
        let t = app.world().resource::<InteractionTimer>();
        t.idle_threshold + t.screensaver_threshold
    };
    assert!(
        app.world().resource::<InteractionTimer>().idle_for(now) < total_threshold,
        "precondition: timer must not be rewound before the chord"
    );

    // Press Shift+S: both keys injected before the update so the PreUpdate
    // `emit_action_input` producer sees Shift held when S is just-pressed.
    send_press(&mut app, KeyCode::ShiftLeft);
    send_press(&mut app, KeyCode::KeyS);
    app.update();
    // After this frame: reset_on_interaction marks the timer at ~130 s, then
    // skip_to_screensaver reads ActionInput{StartScreensaver, Pressed} and
    // rewinds the timer to ~70 s so idle_for(130 s) = 60 s.

    let now = app.world().resource::<Time>().elapsed();
    let idle_time = app.world().resource::<InteractionTimer>().idle_for(now);
    let total_threshold = {
        let t = app.world().resource::<InteractionTimer>();
        t.idle_threshold + t.screensaver_threshold
    };
    assert!(
        idle_time >= total_threshold,
        "Shift+S must rewind the timer past both thresholds; idle_for={idle_time:?}, threshold={total_threshold:?}"
    );
}
