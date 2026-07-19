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

/// Step the app's clock past a graceful `SketchSwitch` reload's fade legs and
/// resolve the pending `AppState` transitions.
///
/// A sketch-select key, `NavigateHome`, `NavigateNext`, or `NavigatePrev` now
/// begins a graceful `wc_core::lifecycle::reload::ReloadReason::SketchSwitch`
/// reload (see `nav::handle_navigation_actions`'s module doc) instead of
/// writing `NextState<AppState>` directly — the destination only becomes the
/// live `AppState` once `FadeOut -> Switch -> FadeIn` resolves. Installs
/// `TimeUpdateStrategy::ManualDuration` with a 500 ms step (comfortably past
/// `SKETCH_SWITCH_FADE_DURATION`'s 400 ms) and drives three updates — the
/// same three-tick walk `reload.rs`'s own phase-walk tests use — so each leg
/// resolves in a single tick regardless of real wall-clock speed.
///
/// Call once per key press before asserting the destination `AppState`; a
/// second press made *before* calling this would be ignored outright (a
/// reload already in flight — see the module doc's already-in-flight edge
/// case), so tests that press multiple nav keys in sequence must settle
/// between each one.
///
/// `Time<Virtual>`'s default `max_delta` (250 ms) would otherwise silently
/// clamp the 500 ms manual step below `SKETCH_SWITCH_FADE_DURATION`'s 400 ms,
/// stalling the fade forever (the same trap `shift_s_chord_arms_screensaver_skip_and_rewinds_timer`
/// below already works around by raising it before a big manual jump), so
/// this raises it for the walk and restores the 250 ms default afterward.
///
/// Restores `TimeUpdateStrategy::Automatic` before returning, so a test that
/// depends on the real wall clock afterward (e.g.
/// `shift_s_targets_screensaver_within_first_60s`, which deliberately keeps
/// `Time::elapsed()` in the low-millisecond range) is not left on a manual
/// clock it never asked for.
fn settle_sketch_switch(app: &mut App) {
    use bevy::time::{TimeUpdateStrategy, Virtual};

    app.insert_resource(TimeUpdateStrategy::ManualDuration(
        std::time::Duration::from_millis(500),
    ));
    app.world_mut()
        .resource_mut::<Time<Virtual>>()
        .set_max_delta(std::time::Duration::from_secs(1));
    app.update(); // FadeOut completes -> Switch (NextState(Home) queued, unless already Home)
    app.update(); // StateTransition resolves; Switch -> FadeIn (NextState(target) queued, unless already there)
    app.update(); // StateTransition resolves; FadeIn completes -> Idle
    app.insert_resource(TimeUpdateStrategy::Automatic);
    app.world_mut()
        .resource_mut::<Time<Virtual>>()
        .set_max_delta(std::time::Duration::from_millis(250)); // Bevy's own default
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
    // Digit1 begins a graceful SketchSwitch reload rather than an instant
    // transition; settle it before asserting the destination.
    settle_sketch_switch(&mut app);
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Line
    );
}

#[test]
fn navigate_home_returns_to_home() {
    let mut app = lifecycle_test_app();
    app.update();
    press_key(&mut app, KeyCode::Digit3); // Select Dots
    settle_sketch_switch(&mut app);
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Dots
    );
    press_key(&mut app, KeyCode::Escape); // Navigate Home
    settle_sketch_switch(&mut app);
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
    settle_sketch_switch(&mut app);
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Line
    );
    // Line → next → Flame
    press_key(&mut app, KeyCode::KeyX);
    settle_sketch_switch(&mut app);
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Flame
    );
    // Flame → next → Dots
    press_key(&mut app, KeyCode::KeyX);
    settle_sketch_switch(&mut app);
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Dots
    );
    // Wrap around: SKETCH_ORDER has 5 entries (Line, Flame, Dots, Cymatics,
    // Radiance — Waves is a de-routed seam, AUDIT.md T5), so 5 nexts from
    // Dots should land back on Dots. Each press must settle before the next
    // one, or it would be ignored as a reload-already-in-flight (see
    // `settle_sketch_switch`'s doc).
    for _ in 0..5 {
        press_key(&mut app, KeyCode::KeyX);
        settle_sketch_switch(&mut app);
    }
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Dots
    );
    // Prev from Dots → Flame (Z key)
    press_key(&mut app, KeyCode::KeyZ);
    settle_sketch_switch(&mut app);
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Flame
    );
}

#[test]
fn idle_transitions_after_threshold() {
    let mut app = lifecycle_test_app();

    // Navigate to Line sketch so SketchActivity sub-state activates. Digit1
    // begins a graceful SketchSwitch reload rather than an instant
    // transition; settle it before asserting the destination.
    app.update();
    press_key(&mut app, KeyCode::Digit1);
    settle_sketch_switch(&mut app);
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
/// This runs on the real clock, so `Time::elapsed()` stays low — squarely
/// inside the previously-broken `< 60 s` window the older
/// `direct_action_input` / `shift_s_chord` tests deliberately stepped past
/// (they advance the clock to ~61 s+ first). The Digit1 press that reaches
/// `AppState::Line` now settles a graceful `SketchSwitch` reload via
/// `settle_sketch_switch`, which advances the clock by ~1.5 s (three legs at
/// a 500 ms manual step) before restoring `TimeUpdateStrategy::Automatic` —
/// still comfortably inside the 60 s window this test is named for, just no
/// longer sub-millisecond by the time the chord fires.
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

    // Navigate to a sketch so the `SketchActivity` sub-state exists. Digit1
    // begins a graceful SketchSwitch reload; settle it (restores the real
    // clock afterward — see `settle_sketch_switch`'s doc — so the elapsed-time
    // assertions below still exercise the low-uptime `< 60 s` window this
    // test is named for).
    press_key(&mut app, KeyCode::Digit1);
    settle_sketch_switch(&mut app);
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

/// Regression test for the message-reader drain bug this codebase has
/// actually shipped once — see the "peek" / reader-cursor warning above
/// `reset_on_interaction` in `crates/wc-core/src/lifecycle/idle.rs`.
/// `debounce_window_resize` must drain BOTH its `WindowResized` and
/// `WindowScaleFactorChanged` readers on every frame — including a frame
/// where both message kinds arrive together and no `WindowResizeSettled` is
/// emitted.
///
/// The regression this guards against is combining the two reads with a
/// short-circuiting `||`, e.g.
/// `resized.read().count() > 0 || scale_changed.read().count() > 0`.
/// Whichever operand sits on the right only gets evaluated — and its reader
/// drained — when the left operand is `false`. A frame carrying BOTH message
/// kinds is exactly the combination that trips this regardless of which
/// operand a future edit puts on which side, because the left operand is
/// `true` and short-circuits the right one.
///
/// This test cannot peek `debounce_window_resize`'s private `Local` reader
/// cursors directly, so it observes the one thing a caller CAN see: the
/// timing of the emitted `WindowResizeSettled`. A single frame writes both a
/// `WindowResized` and a `WindowScaleFactorChanged` message together. With
/// both readers correctly drained, the settle fires the first frame
/// `RESIZE_DEBOUNCE` (250 ms) after that frame. Under the short-circuit
/// regression, the un-drained reader's message survives Bevy's one-frame
/// message double-buffering and gets phantom-observed as a brand-new event on
/// the very next frame, which *rearms* the debounce timer a full tick late —
/// delaying the settle by one more tick. The assertion below lands in the gap
/// between those two timings: it holds against the correct implementation and
/// fails against the regression (verified by deliberately reintroducing the
/// short-circuit and watching this test fail).
#[test]
fn window_resize_debounce_drains_both_readers_every_frame() {
    use bevy::ecs::entity::Entity;
    use bevy::time::TimeUpdateStrategy;
    use bevy::window::{WindowResized, WindowScaleFactorChanged};
    use std::time::Duration;
    use wc_core::lifecycle::window_resize::{debounce_window_resize, WindowResizeSettled};

    // Small enough to stay well under `Time<Virtual>::max_delta`'s default
    // 250 ms cap — RESIZE_DEBOUNCE is also 250 ms, so a step anywhere near
    // that value risks the cap silently truncating a frame's delta (see the
    // `direct_action_input_rewinds_timer` / `shift_s_chord_...` comments
    // above for the same trap with the 60 s idle thresholds).
    const STEP: Duration = Duration::from_millis(60);

    /// Counts `WindowResizeSettled` messages via its own independent reader,
    /// so the test can observe emission without reaching into
    /// `debounce_window_resize`'s private `Local` state.
    #[derive(Resource, Default)]
    struct SettleCount(u32);

    fn count_settles(
        mut reader: MessageReader<'_, '_, WindowResizeSettled>,
        mut count: ResMut<'_, SettleCount>,
    ) {
        count.0 += u32::try_from(reader.read().count()).unwrap_or(u32::MAX);
    }

    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    // `WindowPlugin` (which normally registers these) is not part of this
    // minimal app; register them directly, mirroring what `WindowPlugin`
    // does upstream so `debounce_window_resize`'s `MessageReader` params
    // resolve.
    app.add_message::<WindowResized>();
    app.add_message::<WindowScaleFactorChanged>();
    app.add_message::<WindowResizeSettled>();
    app.init_resource::<SettleCount>();
    app.add_systems(Update, (debounce_window_resize, count_settles).chain());
    app.insert_resource(TimeUpdateStrategy::ManualDuration(STEP));

    // Bevy's first `app.update()` only establishes the time baseline (the
    // clock does not advance yet); `ManualDuration` takes effect from the
    // second tick onward.
    app.update();

    // Frame 1 (t ~= STEP = 60 ms): write BOTH message kinds together — the
    // one combination that trips a short-circuiting `||` regardless of
    // operand order. This arms the debounce timer at t ~= 60 ms; a single
    // arming frame never emits.
    app.world_mut().write_message(WindowResized {
        window: Entity::PLACEHOLDER,
        width: 800.0,
        height: 600.0,
    });
    app.world_mut().write_message(WindowScaleFactorChanged {
        window: Entity::PLACEHOLDER,
        scale_factor: 2.0,
    });
    app.update();
    assert_eq!(
        app.world().resource::<SettleCount>().0,
        0,
        "a single arming frame must not emit immediately"
    );

    // Five more quiet frames (t ~= 120, 180, 240, 300, 360 ms), no new
    // messages written. Correct implementation: both readers were already
    // emptied on frame 1, so the timer stays armed at t ~= 60 ms and the
    // settle fires once elapsed time crosses 60 ms + 250 ms = 310 ms — the
    // first such frame is t ~= 360 ms. Short-circuit regression: the
    // un-drained reader's stale message phantom-fires exactly once more on
    // the very next frame (t ~= 120 ms), rearming the timer there instead; its
    // deadline becomes 120 ms + 250 ms = 370 ms, which this loop does not
    // reach.
    for _ in 0..5 {
        app.update();
    }

    assert_eq!(
        app.world().resource::<SettleCount>().0,
        1,
        "WindowResizeSettled must have fired exactly once by t ~= 360 ms; a \
         count of 0 here means a reader went un-drained on the frame both \
         events arrived together, phantom-rearming the timer and delaying \
         the settle past this window"
    );
}

/// When `Digit1` and `Digit3` are pressed in the same frame, the
/// select-action that sorts first (`Line`, bound to `Digit1`) wins.
///
/// Both keys are pressed before the single `app.update()` so that
/// `emit_action_input` sees both `just_pressed` edges in the same `PreUpdate`
/// tick and emits both `ActionInput::SelectLine` and `ActionInput::SelectDots`.
/// `handle_navigation_actions` processes them in action-order; because
/// `SelectLine` sorts before `SelectDots` in `WaveConductorAction::ALL`, the
/// final `NextState` is `AppState::Line`.
#[test]
fn select_precedence_lower_sketch_wins_when_keys_same_frame() {
    let mut app = lifecycle_test_app();
    app.update();
    // Both keys pressed before the update — same PreUpdate tick.
    send_press(&mut app, KeyCode::Digit1);
    send_press(&mut app, KeyCode::Digit3);
    app.update(); // PreUpdate: both ActionInputs emitted; Update: graceful reload armed for Line
    send_release(&mut app, KeyCode::Digit1);
    send_release(&mut app, KeyCode::Digit3);
    // Settle the graceful reload before asserting the destination.
    settle_sketch_switch(&mut app);
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Line,
        "when Digit1 and Digit3 are pressed in the same frame, SelectLine (which sorts first) must win"
    );
}
