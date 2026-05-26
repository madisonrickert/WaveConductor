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
fn held_press_holds_active_via_idle_veto() {
    // v4 parity: a held button keeps power > 0 (asymptoting to floor=2),
    // so the idle veto fires and the sketch stays Active. Only an explicit
    // release can zero power.
    let mut app = sketches_test_app();
    app.update();
    enter_line(&mut app);

    move_pointer(&mut app, 640.0, 360.0, Vec2::ZERO);
    app.update();

    press_left(&mut app);
    app.update();

    // Held (no release). Drive idle threshold past expiration.
    arm_idle_timeline(&mut app);
    for _ in 0..3 {
        app.update();
    }

    let activity = app.world().resource::<State<SketchActivity>>();
    assert_eq!(
        *activity.get(),
        SketchActivity::Active,
        "Line idle veto should hold Active while button is held and attractor power > 0"
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
    // v4 parity: explicit release immediately zeros power (Plan 11 Phase E).
    // `despawn_attractor_visual` fires on the same tick power becomes zero.
    // One update is sufficient; we run a few more for robustness.
    for _ in 0..5 {
        app.update();
    }
    let after_decay = app
        .world_mut()
        .query::<&AttractorVisual>()
        .iter(app.world())
        .count();
    assert_eq!(after_decay, 0, "visual despawned after release zeros power");
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

// ---------------------------------------------------------------------------
// Plan 11 Phase E: regression tests for the two bugs fixed in this phase.
// ---------------------------------------------------------------------------

/// Verifies that a held press keeps power above zero past the point where the
/// old epsilon-zeroing would have fired (~64 frames).
///
/// v4 parity: `power = floor + (power - floor) * 0.9` per frame, asymptoting
/// toward `floor = 2.0` but never reaching zero while the button is held.
/// Pre-fix v5 zeroed power at `power < floor + 1e-2` (~63 frames in).
#[test]
fn held_press_keeps_power_above_zero_after_decay_period() {
    use wc_sketches::line::systems::MOUSE_POWER_FLOOR;

    let mut app = sketches_test_app();
    app.update();
    enter_line(&mut app);

    move_pointer(&mut app, 640.0, 360.0, Vec2::ZERO);
    press_left(&mut app);
    app.update();

    // 120 frames ≈ 2 s at 60 FPS. v4 parity: held with stationary input →
    // power asymptotes to floor=2, never reaches zero. Pre-fix v5 would zero
    // power around frame ~64 via the epsilon-zeroing in decay_mouse_attractor.
    for _ in 0..120 {
        app.update();
    }

    let power = app.world().resource::<MouseAttractorState>().power;
    assert!(
        power > 0.0,
        "held press should keep power > 0 (v4 asymptotes to floor); got power={power}"
    );
    // By 120 frames, power should be very close to floor.
    assert!(
        (power - MOUSE_POWER_FLOOR).abs() < 0.05,
        "expected power near floor={MOUSE_POWER_FLOOR} after 120 held frames; got {power}"
    );
}

/// Verifies that releasing the mouse button immediately zeros attractor power.
///
/// v4 parity: `pointerup` zeroes power. Pre-fix v5 had no release detection;
/// power could only reach zero via the epsilon-zeroing in `decay_mouse_attractor`,
/// which took ~64 held frames and didn't fire at all during an active hold.
#[test]
fn mouse_release_zeros_attractor_power() {
    let mut app = sketches_test_app();
    app.update();
    enter_line(&mut app);

    move_pointer(&mut app, 640.0, 360.0, Vec2::ZERO);
    press_left(&mut app);
    app.update();
    assert!(
        app.world().resource::<MouseAttractorState>().power > 0.0,
        "sanity: press should set power > 0"
    );

    release_left(&mut app);
    app.update();

    let power = app.world().resource::<MouseAttractorState>().power;
    #[allow(
        clippy::float_cmp,
        reason = "release path explicitly assigns power = 0.0; bit-for-bit check is correct"
    )]
    {
        assert_eq!(power, 0.0, "mouse release should zero power; got {power}");
    }
}

/// Verifies that releasing a touch immediately zeros attractor power.
///
/// Touch uses `Touches::iter_just_released()` — a separate code path from
/// mouse `just_released`. Tested independently so both paths are covered.
#[test]
fn touch_release_zeros_attractor_power() {
    use common::input::{touch_end, touch_start};

    let mut app = sketches_test_app();
    app.update();
    enter_line(&mut app);

    move_pointer(&mut app, 640.0, 360.0, Vec2::ZERO);
    touch_start(&mut app, 1, 640.0, 360.0);
    app.update();
    assert!(
        app.world().resource::<MouseAttractorState>().power > 0.0,
        "sanity: touch should set power > 0"
    );

    touch_end(&mut app, 1, 640.0, 360.0);
    app.update();

    let power = app.world().resource::<MouseAttractorState>().power;
    #[allow(
        clippy::float_cmp,
        reason = "release path explicitly assigns power = 0.0; bit-for-bit check is correct"
    )]
    {
        assert_eq!(power, 0.0, "touch release should zero power; got {power}");
    }
}

/// Verifies that a pinch falling edge (relaxing below threshold) zeros power.
///
/// Exercises the `hand_just_released` path in `update_mouse_attractor`.
/// Pre-fix v5 only tracked rising edges in `LastPinchState`; the falling
/// edge was never detected, so a relaxing pinch left power decaying toward
/// floor instead of zeroing immediately.
#[cfg(feature = "hand-tracking-gestures")]
#[test]
fn hand_pinch_release_zeros_attractor_power() {
    use bevy::math::Vec3;
    use std::time::Duration;
    use wc_core::input::hand::{Chirality, Hand, LANDMARK_COUNT};
    use wc_core::input::state::{HandTrackingFrame, HandTrackingState};
    use wc_sketches::line::systems::mouse::PINCH_PRESS_THRESHOLD;

    let mut app = sketches_test_app();
    app.update();
    enter_line(&mut app);

    let landmarks = [Vec3::ZERO; LANDMARK_COUNT];

    // Rising edge: pinch above threshold.
    let hand_pressed = Hand {
        id: 1,
        chirality: Chirality::Right,
        palm_position: Vec3::ZERO,
        palm_normal: Vec3::Y,
        palm_velocity: Vec3::ZERO,
        pinch_strength: PINCH_PRESS_THRESHOLD + 0.05,
        grab_strength: 0.0,
        landmarks,
    };
    app.world_mut()
        .resource_mut::<HandTrackingState>()
        .ingest(&HandTrackingFrame {
            hands: smallvec::smallvec![hand_pressed],
            timestamp: Duration::from_millis(0),
        });
    app.update();
    assert!(
        app.world().resource::<MouseAttractorState>().power > 0.0,
        "sanity: pinch should set power > 0"
    );

    // Falling edge: pinch relaxed below threshold.
    let hand_relaxed = Hand {
        id: 1,
        chirality: Chirality::Right,
        palm_position: Vec3::ZERO,
        palm_normal: Vec3::Y,
        palm_velocity: Vec3::ZERO,
        pinch_strength: 0.0,
        grab_strength: 0.0,
        landmarks,
    };
    app.world_mut()
        .resource_mut::<HandTrackingState>()
        .ingest(&HandTrackingFrame {
            hands: smallvec::smallvec![hand_relaxed],
            timestamp: Duration::from_millis(100),
        });
    app.update();

    let power = app.world().resource::<MouseAttractorState>().power;
    #[allow(
        clippy::float_cmp,
        reason = "release path explicitly assigns power = 0.0; bit-for-bit check is correct"
    )]
    {
        assert_eq!(power, 0.0, "pinch release should zero power; got {power}");
    }
}
