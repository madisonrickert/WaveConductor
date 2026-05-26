//! End-to-end input integration tests for the Line sketch.
//!
//! Synthesizes keyboard, mouse, and touch events via `common::input`
//! helpers and asserts on the resulting state. Plan 8 Phase 0 wired
//! `pointer_merge_system` to consume `CursorMoved` messages, so the
//! harness runs the merge system and synthetic events flow end-to-end —
//! no resource-poking shortcuts.

#![allow(
    clippy::expect_used,
    reason = "expect with a clear message is appropriate in test code"
)]

mod common;
use common::input::{move_pointer, press_left, release_left, tap_key};
use common::{arm_idle_timeline, sketches_test_app};

use bevy::input::keyboard::KeyCode;
use bevy::math::Vec2;
use bevy::prelude::*;
use wc_core::lifecycle::state::{AppState, SketchActivity};
use wc_sketches::line::compute::LineSimParams;
use wc_sketches::line::systems::MouseAttractorState;

/// Drive the keyboard-nav action that selects Line.
///
/// The lifecycle nav binding is configured in
/// `crates/wc-core/src/lifecycle/actions.rs::default_input_map()` — `Digit1`
/// maps to `WaveConductorAction::SelectLine`. If that binding changes, the
/// `AppState::Line` assertion below will fail with a clear message.
fn enter_line(app: &mut App) {
    tap_key(app, KeyCode::Digit1);
    // leafwing + Bevy state propagation takes a few frames: one to fold the
    // synthetic KeyboardInput into ButtonInput<KeyCode> + tick leafwing's
    // ActionState, one for nav::handle_navigation_actions to set NextState,
    // and one for the OnEnter(AppState::Line) schedule to fire. Three updates
    // is sufficient (was four; trimmed in Plan 10 Phase 0).
    for _ in 0..3 {
        app.update();
    }
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Line,
        "Digit1 keyboard nav should enter AppState::Line",
    );
}

#[test]
fn left_press_activates_mouse_attractor() {
    let mut app = sketches_test_app();
    app.update();
    enter_line(&mut app);

    move_pointer(&mut app, 640.0, 360.0, Vec2::ZERO);
    // One update folds CursorMoved into PointerState via pointer_merge_system.
    app.update();

    let before = app.world().resource::<MouseAttractorState>().power;
    // Bit-for-bit equality with 0.0: MouseAttractorState::default() seeds power
    // to exactly 0.0 and no system has had cause to mutate it yet.
    #[allow(
        clippy::float_cmp,
        reason = "bit-for-bit baseline check: default power must be exactly 0.0"
    )]
    {
        assert_eq!(before, 0.0, "attractor inactive before any input");
    }

    press_left(&mut app);
    app.update();

    let after = app.world().resource::<MouseAttractorState>().power;
    assert!(
        after > 0.0,
        "left press should raise MouseAttractorState.power above zero, got {after}"
    );
}

#[test]
fn sim_params_records_one_attractor_after_press() {
    let mut app = sketches_test_app();
    app.update();
    enter_line(&mut app);

    move_pointer(&mut app, 640.0, 360.0, Vec2::ZERO);
    app.update();

    press_left(&mut app);
    app.update();

    let sim = app
        .world()
        .get_resource::<LineSimParams>()
        .expect("LineSimParams should exist after entering Line");
    assert_eq!(
        sim.params.attractor_count, 1,
        "one mouse attractor should be active immediately after press"
    );
}

#[test]
fn release_starts_attractor_decay() {
    let mut app = sketches_test_app();
    app.update();
    enter_line(&mut app);

    move_pointer(&mut app, 640.0, 360.0, Vec2::ZERO);
    app.update();

    press_left(&mut app);
    app.update();
    let peak = app.world().resource::<MouseAttractorState>().power;
    assert!(peak > 0.0, "expected non-zero peak after press, got {peak}");

    release_left(&mut app);
    app.update();
    app.update(); // one decay tick after release

    let decayed = app.world().resource::<MouseAttractorState>().power;
    assert!(
        decayed < peak,
        "power should decay after release: peak={peak}, decayed={decayed}"
    );
}

#[test]
fn decay_holds_active_via_idle_veto() {
    let mut app = sketches_test_app();
    app.update();
    enter_line(&mut app);

    move_pointer(&mut app, 640.0, 360.0, Vec2::ZERO);
    app.update();

    press_left(&mut app);
    app.update();
    release_left(&mut app);
    app.update();

    // Power is non-zero but decaying. Drive idle threshold past expiration.
    arm_idle_timeline(&mut app);
    for _ in 0..3 {
        app.update();
    }

    let activity = app.world().resource::<State<SketchActivity>>();
    assert_eq!(
        *activity.get(),
        SketchActivity::Active,
        "Line idle veto should hold Active while attractor decays"
    );
}

#[test]
fn attractor_visual_spawns_on_press_and_despawns_on_release() {
    use wc_sketches::line::attractor_visuals::AttractorVisual;

    let mut app = sketches_test_app();
    app.update();
    enter_line(&mut app);
    move_pointer(&mut app, 640.0, 360.0, Vec2::ZERO);
    app.update();

    let before = app
        .world_mut()
        .query::<&AttractorVisual>()
        .iter(app.world())
        .count();
    assert_eq!(before, 0, "no visual before press");

    press_left(&mut app);
    app.update();
    app.update();
    let after_press = app
        .world_mut()
        .query::<&AttractorVisual>()
        .iter(app.world())
        .count();
    assert_eq!(after_press, 1, "one visual after press");

    release_left(&mut app);
    // Power decays geometrically toward MOUSE_POWER_FLOOR=2.0 by ×0.9 each
    // tick; zeroes when power < floor + 1e-2. Starting from ~8.48 (two
    // decay ticks past peak 10.0), the excess 6.48 × 0.9^n needs to fall
    // below 0.01 — n > log(0.01/6.48)/log(0.9) ≈ 61. 80 ticks gives a
    // comfortable safety margin.
    for _ in 0..80 {
        app.update();
    }
    let after_decay = app
        .world_mut()
        .query::<&AttractorVisual>()
        .iter(app.world())
        .count();
    assert_eq!(after_decay, 0, "visual despawned after power reaches zero");
}

#[test]
fn power_zero_lets_state_transition_to_idle() {
    let mut app = sketches_test_app();
    app.update();
    enter_line(&mut app);

    // Force the attractor straight to zero so the veto returns false.
    app.world_mut().resource_mut::<MouseAttractorState>().power = 0.0;

    arm_idle_timeline(&mut app);
    for _ in 0..3 {
        app.update();
    }

    let activity = app.world().resource::<State<SketchActivity>>();
    assert_eq!(
        *activity.get(),
        SketchActivity::Idle,
        "with veto inactive and idle threshold crossed, state should become Idle"
    );
}

#[test]
fn touch_press_activates_mouse_attractor() {
    use common::input::{move_pointer, touch_start};

    let mut app = sketches_test_app();
    app.update();
    enter_line(&mut app);

    move_pointer(&mut app, 640.0, 360.0, Vec2::ZERO);
    app.update();

    let pre = app.world().resource::<MouseAttractorState>().power;
    #[allow(
        clippy::float_cmp,
        reason = "bit-for-bit baseline check: default power must be exactly 0.0"
    )]
    {
        assert_eq!(pre, 0.0, "attractor inactive before any input");
    }

    touch_start(&mut app, 1, 640.0, 360.0);
    app.update();

    let post = app.world().resource::<MouseAttractorState>().power;
    // `decay_mouse_attractor` runs in the same `.chain()` as
    // `update_mouse_attractor`, so power decays one tick from MOUSE_POWER_PRESS
    // before we can read it. Any non-zero value confirms the press fired.
    assert!(
        post > 0.0,
        "expected non-zero attractor power after touch_start; got {post}"
    );
}

#[cfg(feature = "hand-tracking-gestures")]
#[test]
fn hand_pinch_activates_mouse_attractor() {
    use bevy::math::Vec3;
    use std::time::Duration;
    use wc_core::input::hand::{Chirality, Hand, LANDMARK_COUNT};
    use wc_core::input::state::{HandTrackingFrame, HandTrackingState};
    use wc_sketches::line::systems::mouse::PINCH_PRESS_THRESHOLD;

    let mut app = sketches_test_app();
    app.update();
    enter_line(&mut app);

    let landmarks = [Vec3::ZERO; LANDMARK_COUNT];
    let hand = Hand {
        id: 1,
        chirality: Chirality::Right,
        palm_position: Vec3::ZERO,
        palm_normal: Vec3::Y,
        palm_velocity: Vec3::ZERO,
        pinch_strength: PINCH_PRESS_THRESHOLD + 0.05,
        grab_strength: 0.0,
        landmarks,
    };

    let frame = HandTrackingFrame {
        hands: smallvec::smallvec![hand],
        timestamp: Duration::from_millis(0),
    };
    app.world_mut()
        .resource_mut::<HandTrackingState>()
        .ingest(&frame);

    app.update();

    let post = app.world().resource::<MouseAttractorState>().power;
    // `decay_mouse_attractor` runs in the same `.chain()` as
    // `update_mouse_attractor`, so power decays one tick from MOUSE_POWER_PRESS
    // before we can read it. Any non-zero value confirms the pinch press fired.
    assert!(
        post > 0.0,
        "expected non-zero attractor power after hand pinch edge; got {post}"
    );
}
