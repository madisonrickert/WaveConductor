//! End-to-end input integration tests for the Line sketch.
//!
//! Synthesizes keyboard, mouse, and touch events via `common::input`
//! helpers and asserts on the resulting state without bypassing any
//! production code path other than seeding [`PointerState`] (the
//! `sketches_test_app` harness does not include `pointer_merge_system`,
//! so we set the pointer directly so `update_mouse_attractor` can
//! observe a non-`None` cursor).

#![allow(
    clippy::expect_used,
    reason = "expect with a clear message is appropriate in test code"
)]

mod common;
use common::input::{press_left, release_left, tap_key};
use common::{arm_idle_timeline, sketches_test_app};

use bevy::input::keyboard::KeyCode;
use bevy::math::Vec2;
use bevy::prelude::*;
use wc_core::input::pointer::{PointerSource, PointerState};
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
    // leafwing + Bevy state propagation takes a few frames: one to fold
    // the synthetic KeyboardInput into ButtonInput<KeyCode>, one for
    // leafwing's ActionState tick, one for nav::handle_navigation_actions
    // to set NextState, and one for the OnEnter(AppState::Line) schedule
    // to fire. Four updates is comfortable headroom.
    for _ in 0..4 {
        app.update();
    }
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Line,
        "Digit1 keyboard nav should enter AppState::Line",
    );
}

/// Place the pointer at a fixed window-space position so
/// `update_mouse_attractor` observes `pointer.primary == Some(...)` on the
/// next frame. `sketches_test_app()` does not run `pointer_merge_system`,
/// so we set `PointerState` directly. Production code consumes
/// `PointerState` whether it was produced by the merge system or inserted
/// by hand, so this exercises the same downstream path.
fn seed_pointer(app: &mut App, x: f32, y: f32) {
    app.world_mut().insert_resource(PointerState {
        primary: Some(Vec2::new(x, y)),
        source: PointerSource::Mouse,
    });
}

#[test]
fn left_press_activates_mouse_attractor() {
    let mut app = sketches_test_app();
    app.update();
    enter_line(&mut app);

    seed_pointer(&mut app, 640.0, 360.0);

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

    seed_pointer(&mut app, 640.0, 360.0);

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

    seed_pointer(&mut app, 640.0, 360.0);

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

    seed_pointer(&mut app, 640.0, 360.0);

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
