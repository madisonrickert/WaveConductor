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

use std::time::Duration;

use bevy::input::keyboard::KeyCode;
use bevy::input::ButtonInput;
use bevy::math::Vec2;
use bevy::prelude::*;
use wc_core::input::button::HandButton;
use wc_core::input::gesture::HandGestureEvent;
use wc_core::input::hand::{Chirality, Hand, LANDMARK_COUNT};
use wc_core::input::provider::{ProviderId, ProviderRegistry, ProviderRole};
use wc_core::input::providers::mock::MockProvider;
use wc_core::input::state::{FusedHandFrame, HandTrackingFrame};
use wc_core::input::systems::{fuse_hand_frames, poll_all_providers, sync_hand_entities};
use wc_core::lifecycle::state::{AppState, SketchActivity};
use wc_sketches::line::compute::LineSimParams;
use wc_sketches::line::leap_attractors::{LineHandAttractor, LINE_HAND_DECAY_SPEED};
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

// ---------------------------------------------------------------------------
// Plan 11.6 Phase 12: LineHandAttractor integration tests.
//
// These tests wire the full provider → fuse → sync → observer →
// update_line_hand_attractors chain end-to-end.  `sketches_test_app` does
// NOT include `HandTrackingPlugin` (to avoid double-registering
// `pointer_merge_system`), so a local `hand_tracking_test_app` helper adds
// just the three pipeline systems that the attractor tests require.
// ---------------------------------------------------------------------------

/// Build a test app that includes the full hand-tracking pipeline on top of
/// the standard `sketches_test_app`.
///
/// Adds the three `PreUpdate` systems (`poll_all_providers`,
/// `fuse_hand_frames`, `sync_hand_entities`) plus the messages and resources
/// they depend on that `sketches_test_app` does not already provide.
///
/// `HandTrackingPlugin` is intentionally NOT used here because
/// `sketches_test_app` already registers `pointer_merge_system` under
/// `InputSystems`; adding the full plugin would duplicate that registration.
fn hand_tracking_test_app() -> App {
    let mut app = sketches_test_app();

    // Messages the pipeline reads / writes.  `add_message` is idempotent.
    app.add_message::<HandTrackingFrame>();
    app.add_message::<FusedHandFrame>();
    app.add_message::<HandGestureEvent>();

    // Resource consumed by `detect_gestures` (not added here, but
    // `ButtonInput<T>` must be present so `sync_hand_entities` can compile
    // its system params — it is transitively required through the type
    // bounds even though only `mirror_state_resource` reads it directly).
    app.init_resource::<ButtonInput<HandButton>>();

    // Empty registry; tests replace this with `insert_resource` before the
    // frames are needed.
    app.init_resource::<ProviderRegistry>();

    // Chain the three pipeline systems in `PreUpdate`, ordered after
    // `InputSystems` so Bevy's own input flush has already run.
    app.add_systems(
        PreUpdate,
        (poll_all_providers, fuse_hand_frames, sync_hand_entities).chain(),
    );

    app
}

/// Build a `Hand` with all fields set, with grab strength as the only
/// variable.  All landmarks are zeroed; palm normal faces up (+Y).
fn hand_with_grab(id: u32, chirality: Chirality, palm: Vec3, grab: f32) -> Hand {
    Hand {
        id,
        chirality,
        palm_position: palm,
        palm_normal: Vec3::Y,
        palm_velocity: Vec3::ZERO,
        pinch_strength: 0.0,
        grab_strength: grab,
        landmarks: [Vec3::ZERO; LANDMARK_COUNT],
    }
}

/// Build a [`HandTrackingFrame`] for the mock provider.
fn hand_frame(hands: Vec<Hand>, t_ms: u64) -> HandTrackingFrame {
    HandTrackingFrame {
        provider: ProviderId::Mock,
        hands: hands.into_iter().collect(),
        timestamp: Duration::from_millis(t_ms),
    }
}

/// Replace the registry in `app` with a `MockProvider` scripted from
/// `frames`.  Calls `registry.register`, which eagerly calls `start()`.
fn install_mock_with_frames(app: &mut App, frames: Vec<HandTrackingFrame>) {
    let mut registry = ProviderRegistry::default();
    let mock = MockProvider::with_frames(frames);
    registry.register(ProviderId::Mock, ProviderRole::Simulator, Box::new(mock));
    app.insert_resource(registry);
}

/// A single grab-strength=0.9 right-hand frame repeated 200 times ramps
/// `LineHandAttractor::power` above a detectable threshold.
///
/// v4 formula: `wanted = grab^1.5 * 5^((-z + 350) / 160)`.
/// With palm at `(0, 200, 0)`: `depth_modulator` = 5^(350/160) ≈ 14.3,
/// `grab_component` = 0.9^1.5 ≈ 0.854, wanted ≈ 12.2.
/// EMA at attack = 0.005: after 200 ticks, power ≈ 12.2 * (1 - 0.995^200)
/// ≈ 12.2 * 0.632 ≈ 7.7.  The assertion threshold of 0.1 is very
/// conservative.
#[test]
fn one_hand_grab_yields_non_zero_power_after_many_ticks() {
    let mut app = hand_tracking_test_app();
    app.update();
    enter_line(&mut app);

    let h = hand_with_grab(1, Chirality::Right, Vec3::new(0.0, 200.0, 0.0), 0.9);
    let frames: Vec<_> = (0..200u64).map(|t| hand_frame(vec![h.clone()], 10 * t)).collect();
    install_mock_with_frames(&mut app, frames);

    for _ in 0..200 {
        app.update();
    }

    let world = app.world_mut();
    let attractor_powers: Vec<f32> = world
        .query::<&LineHandAttractor>()
        .iter(world)
        .map(|a| a.power)
        .collect();
    assert_eq!(attractor_powers.len(), 1, "exactly one hand attractor");
    assert!(
        attractor_powers[0] > 0.1,
        "power = {}, expected > 0.1 after ramp",
        attractor_powers[0]
    );
}

/// Two hands with distinct IDs each get their own `LineHandAttractor`
/// component — the attractor model is per-hand, not a shared singleton.
#[test]
fn two_hands_yield_two_independent_attractors() {
    let mut app = hand_tracking_test_app();
    app.update();
    enter_line(&mut app);

    let h1 = hand_with_grab(1, Chirality::Right, Vec3::new(100.0, 200.0, 0.0), 0.9);
    let h2 = hand_with_grab(2, Chirality::Left, Vec3::new(-100.0, 200.0, 0.0), 0.7);
    let frames: Vec<_> = (0..30u64)
        .map(|t| hand_frame(vec![h1.clone(), h2.clone()], 10 * t))
        .collect();
    install_mock_with_frames(&mut app, frames);

    for _ in 0..30 {
        app.update();
    }
    let world = app.world_mut();
    let count = world.query::<&LineHandAttractor>().iter(world).count();
    assert_eq!(count, 2, "expected two independent LineHandAttractors");
}

/// After a ramp phase (grab=0.9 × 200 ticks), dropping grab to 0.0 causes
/// power to decay geometrically by `LINE_HAND_DECAY_SPEED` per tick.
///
/// The generous ±0.5 absolute tolerance accounts for EMA settling and the
/// small number of extra ticks that `enter_line` introduces before the decay
/// phase begins.  The assertion only fails if the decay constant changes.
#[test]
fn release_decays_power_geometrically() {
    let mut app = hand_tracking_test_app();
    app.update();
    enter_line(&mut app);

    let mut frames = Vec::<HandTrackingFrame>::new();
    for t in 0..200u64 {
        let h = hand_with_grab(1, Chirality::Right, Vec3::new(0.0, 200.0, 0.0), 0.9);
        frames.push(hand_frame(vec![h], 10 * t));
    }
    for t in 200..220u64 {
        let h = hand_with_grab(1, Chirality::Right, Vec3::new(0.0, 200.0, 0.0), 0.0);
        frames.push(hand_frame(vec![h], 10 * t));
    }
    install_mock_with_frames(&mut app, frames);

    // Ramp phase.
    for _ in 0..200 {
        app.update();
    }
    let p_at_release = app
        .world_mut()
        .query::<&LineHandAttractor>()
        .iter(app.world())
        .next()
        .map(|a| a.power)
        .expect("LineHandAttractor present after ramp");
    assert!(p_at_release > 0.05, "p_at_release={p_at_release}");

    // Decay phase: 10 ticks of grab=0 → power *= LINE_HAND_DECAY_SPEED each tick.
    for _ in 0..10 {
        app.update();
    }
    let p_after_decay = app
        .world_mut()
        .query::<&LineHandAttractor>()
        .iter(app.world())
        .next()
        .map(|a| a.power)
        .expect("LineHandAttractor still present after decay");
    let expected = p_at_release * LINE_HAND_DECAY_SPEED.powi(10);

    // Generous tolerance — the per-tick decay multiplier compounds, so a
    // small drift from extra updates compounds geometrically too. 0.5 is
    // wide enough that the test only fails if the decay constant changed.
    assert!(
        (p_after_decay - expected).abs() < 0.5,
        "p_after_decay={p_after_decay} expected={expected} (p_at_release={p_at_release})"
    );
}

