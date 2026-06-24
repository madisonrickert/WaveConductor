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
/// The timeline is built so the assertion genuinely depends on the rewind: the
/// timer is marked fresh at ~61 s, then the clock advances only ~1 s while the
/// action is processed (no keyboard input here, so `reset_on_interaction` never
/// re-marks the timer). Absent the rewind, `idle_for` would be ~1 s, far below
/// the 60 s total threshold; only `rewind_past_screensaver` can push it to
/// exactly 60 s. (Confirmed by neutering the rewind and watching this fail.)
///
/// Two timing quirks shape the setup: Bevy's first `app.update()` does not
/// advance the clock (it only sets the baseline), so an extra settle tick is
/// needed; and `Time<Virtual>` caps each tick at `max_delta = 250 ms` by
/// default, so we raise it to 70 s to let one tick jump past 60 s.
#[test]
fn direct_action_input_rewinds_timer() {
    use bevy::time::{TimeUpdateStrategy, Virtual};
    use std::time::Duration;
    use wc_core::lifecycle::action_map::{ActionInput, ActionPhase};
    use wc_core::lifecycle::actions::WaveConductorAction;
    use wc_core::lifecycle::idle::InteractionTimer;

    let mut app = lifecycle_test_app();
    // Raise the virtual-time max_delta cap so a tick can jump past 60 s.
    app.world_mut()
        .resource_mut::<Time<Virtual>>()
        .set_max_delta(Duration::from_secs(70));

    // Bevy's first `app.update()` establishes the time baseline and does not
    // advance the clock; ManualDuration only takes effect from the second tick.
    // Tick once to settle (clock stays ~0), then tick to ~61 s so the clock is
    // past the 60 s total threshold (otherwise `rewind_past_screensaver` would
    // saturate `now - 60 s` to zero and the rewind would be unobservable).
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(61)));
    app.update(); // settle: time.elapsed() ≈ 0 s
    app.update(); // time.elapsed() ≈ 61 s

    {
        let now = app.world().resource::<Time>().elapsed();
        let mut timer = app.world_mut().resource_mut::<InteractionTimer>();
        timer.idle_threshold = Duration::from_secs(30);
        timer.screensaver_threshold = Duration::from_secs(30);
        timer.mark(now); // freshly interacted at ~61 s
    }

    // From here advance only 1 s per tick. The timer marked at ~61 s therefore
    // accrues just ~1 s of natural idle by the assertion (far under the 60 s
    // threshold), so only the rewind can carry idle_for across the threshold.
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(1)));

    // Precondition: before the action, the freshly-marked timer is well under
    // the threshold. This is the state the rewind must overturn.
    {
        let now = app.world().resource::<Time>().elapsed();
        let t = app.world().resource::<InteractionTimer>();
        assert!(
            t.idle_for(now) < t.idle_threshold + t.screensaver_threshold,
            "precondition: timer must not already be past threshold"
        );
    }

    // Inject ActionInput directly — no keyboard press, no producer needed.
    app.world_mut().write_message(ActionInput {
        action: WaveConductorAction::StartScreensaver,
        phase: ActionPhase::Pressed,
    });
    app.update(); // time → ~62 s; skip_to_screensaver rewinds last_interaction to ~2 s

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
/// `press_key` calls must precede the single chord `app.update()`.
///
/// This test is non-trivial without restructuring: the chord's key press makes
/// `reset_on_interaction` mark the timer fresh (`idle_for` = 0) on the same frame,
/// so only the rewind can carry `idle_for` to the threshold. Bevy's first
/// `app.update()` only sets the time baseline (clock stays ~0); the chord update
/// is the second tick, which jumps the clock to ~65 s (we raise
/// `Time<Virtual>::max_delta` first, since the default cap is 250 ms) so
/// `rewind_past_screensaver` produces a non-zero `last_interaction`.
#[test]
fn shift_s_chord_arms_screensaver_skip_and_rewinds_timer() {
    use bevy::time::{TimeUpdateStrategy, Virtual};
    use std::time::Duration;
    use wc_core::lifecycle::idle::InteractionTimer;

    let mut app = lifecycle_test_app();
    // Raise virtual-time max_delta cap so the chord tick can jump past 60 s.
    app.world_mut()
        .resource_mut::<Time<Virtual>>()
        .set_max_delta(Duration::from_secs(70));
    // Settle tick: the first update only sets the time baseline, so the clock
    // stays ~0 here; ManualDuration(65 s) takes effect on the next (chord) tick.
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(65)));
    app.update(); // settle: time.elapsed() ≈ 0 s

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
    app.update(); // chord tick: clock jumps to ~65 s
                  // After this frame: reset_on_interaction marks the timer at ~65 s, then
                  // skip_to_screensaver reads ActionInput{StartScreensaver, Pressed} and
                  // rewinds the timer to ~5 s so idle_for(65 s) = 60 s.

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

/// Regression: pressing `Shift+S` within the first 60 s of uptime must target
/// the screensaver immediately.
///
/// Before the force flag, `rewind_past_screensaver` saturated `now - 60 s` to
/// zero at low uptime, so `idle_for` never crossed the 60 s threshold and
/// `advance_activity` stayed on `Active` — the chord did nothing until the app
/// had been up a full minute. The flag set by the rewind now carries
/// `advance_activity` into `Screensaver` at any uptime.
///
/// This runs on the real clock (no `TimeUpdateStrategy`), so `Time::elapsed()`
/// stays in the low-millisecond range — squarely inside the previously-broken
/// `< 60 s` window the older `direct_action_input` / `shift_s_chord` tests
/// deliberately stepped past (they advance the clock to ~61 s+ first).
///
/// The assertion reads the `NextState<SketchActivity>` that `advance_activity`
/// queues in the chord frame rather than resolving the transition: actually
/// entering `Screensaver` runs the framework's `OnEnter` present-rate systems,
/// which need a `WinitSettings` resource the `MinimalPlugins` harness has no
/// winit backend to provide. The queued target is exactly what the force flag
/// changes, so it is the precise regression signal.
#[test]
fn shift_s_targets_screensaver_within_first_60s() {
    let mut app = lifecycle_test_app();
    app.update();

    // Navigate to a sketch so the `SketchActivity` sub-state exists.
    press_key(&mut app, KeyCode::Digit1);
    app.update(); // resolve → AppState::Line, SketchActivity::Active
    assert_eq!(
        *app.world().resource::<State<SketchActivity>>().get(),
        SketchActivity::Active,
    );
    // Precondition: nothing is steering activity away from Active yet.
    assert!(
        matches!(
            app.world().resource::<NextState<SketchActivity>>(),
            NextState::Unchanged
        ),
        "precondition: no pending activity transition before the chord"
    );

    // Chord: Shift held + S just-pressed in the same PreUpdate tick (the
    // producer requires both edges together), so both presses precede the
    // update.
    send_press(&mut app, KeyCode::ShiftLeft);
    send_press(&mut app, KeyCode::KeyS);
    app.update(); // emit StartScreensaver → skip arms + force flag → advance_activity queues Screensaver

    assert!(
        matches!(
            app.world().resource::<NextState<SketchActivity>>(),
            NextState::Pending(SketchActivity::Screensaver)
        ),
        "Shift+S within the first 60 s of uptime must queue the Screensaver transition; got {:?}",
        app.world().resource::<NextState<SketchActivity>>()
    );
}

/// When `Digit1` and `Digit2` are pressed in the same frame, the
/// lower-numbered sketch (`Line`, bound to `Digit1`) wins.
///
/// Both keys are pressed before the single `app.update()` so that
/// `emit_action_input` sees both `just_pressed` edges in the same `PreUpdate`
/// tick and emits both `ActionInput::SelectLine` and `ActionInput::SelectFlame`.
/// `handle_navigation_actions` processes them in action-order; because
/// `SelectLine` sorts before `SelectFlame` in `WaveConductorAction::ALL`, the
/// final `NextState` is `AppState::Line`.
#[test]
fn select_precedence_lower_sketch_wins_when_keys_same_frame() {
    let mut app = lifecycle_test_app();
    app.update();
    // Both keys pressed before the update — same PreUpdate tick.
    send_press(&mut app, KeyCode::Digit1);
    send_press(&mut app, KeyCode::Digit2);
    app.update(); // PreUpdate: both ActionInputs emitted; Update: NextState set
    send_release(&mut app, KeyCode::Digit1);
    send_release(&mut app, KeyCode::Digit2);
    // Pending transition resolves on the next update tick.
    app.update();
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Line,
        "when Digit1 and Digit2 are pressed in the same frame, Line (lower-numbered) must win"
    );
}
