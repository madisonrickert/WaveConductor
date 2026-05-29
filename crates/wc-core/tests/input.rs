//! Integration tests for `HandTrackingPlugin`.

use std::time::Duration;

use bevy::input::{ButtonInput, InputPlugin};
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use bevy::window::WindowResolution;
use wc_core::input::{
    button::HandButton,
    gesture::HandGestureEvent,
    hand::{Chirality, Hand, LandmarkIndex, LANDMARK_COUNT},
    pointer::{PointerSource, PointerState},
    provider::{ProviderId, ProviderRegistry, ProviderRole},
    providers::mock::MockProvider,
    state::{HandTrackingFrame, HandTrackingState},
    HandTrackingPlugin,
};

// Distinct default ID per chirality so a frame containing both hands keys
// to two distinct `(provider, raw_id)` slots in the entity table —
// `sync_hand_entities` (Plan 11.6 Phase 6) treats colliding keys as "same
// hand, update in place", which would silently collapse two hands into one
// in the multi-hand test fixtures.
fn fake_hand(chirality: Chirality, pinch: f32, grab: f32) -> Hand {
    let id = match chirality {
        Chirality::Left => 1,
        Chirality::Right => 2,
    };
    Hand {
        id,
        chirality,
        palm_position: Vec3::ZERO,
        palm_normal: Vec3::Y,
        palm_velocity: Vec3::ZERO,
        pinch_strength: pinch,
        grab_strength: grab,
        landmarks: [Vec3::ZERO; LANDMARK_COUNT],
    }
}

fn frame(hands: impl IntoIterator<Item = Hand>, at_ms: u64) -> HandTrackingFrame {
    HandTrackingFrame {
        provider: ProviderId::Mock,
        hands: hands.into_iter().collect(),
        timestamp: Duration::from_millis(at_ms),
    }
}

fn test_app_with_mock(mock: MockProvider) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_plugins(InputPlugin);
    app.add_plugins(StatesPlugin);
    app.add_plugins(HandTrackingPlugin);
    let mut registry = ProviderRegistry::default();
    registry.register(ProviderId::Mock, ProviderRole::Simulator, Box::new(mock));
    app.insert_resource(registry);
    app
}

#[test]
fn empty_provider_produces_empty_state() {
    let mut app = test_app_with_mock(MockProvider::with_frames([]));
    app.update();
    let state = app.world().resource::<HandTrackingState>();
    assert_eq!(state.active_hand_count(), 0);
}

#[test]
fn mock_frame_populates_state() {
    let mut app = test_app_with_mock(MockProvider::with_frames([frame(
        [fake_hand(Chirality::Right, 0.0, 0.0)],
        100,
    )]));
    app.update();
    let state = app.world().resource::<HandTrackingState>();
    assert_eq!(state.active_hand_count(), 1);
    assert!(state.right().is_some());
    assert!(state.left().is_none());
}

#[test]
fn pinch_crossing_threshold_presses_button() {
    let mut app = test_app_with_mock(MockProvider::with_frames([
        // Frame 1: pinch below press threshold → not pressed.
        frame([fake_hand(Chirality::Right, 0.5, 0.0)], 100),
        // Frame 2: pinch above press threshold → pressed.
        frame([fake_hand(Chirality::Right, 0.9, 0.0)], 200),
    ]));
    app.update();
    {
        let buttons = app.world().resource::<ButtonInput<HandButton>>();
        assert!(!buttons.pressed(HandButton::RightPinch));
    }
    app.update();
    let buttons = app.world().resource::<ButtonInput<HandButton>>();
    assert!(buttons.pressed(HandButton::RightPinch));
    assert!(buttons.just_pressed(HandButton::RightPinch));
}

#[test]
fn pinch_release_below_release_threshold() {
    let mut app = test_app_with_mock(MockProvider::with_frames([
        // Press.
        frame([fake_hand(Chirality::Right, 0.9, 0.0)], 100),
        // Stays above release threshold — still pressed.
        frame([fake_hand(Chirality::Right, 0.6, 0.0)], 200),
        // Drops below release threshold — released.
        frame([fake_hand(Chirality::Right, 0.3, 0.0)], 300),
    ]));
    app.update();
    {
        let buttons = app.world().resource::<ButtonInput<HandButton>>();
        assert!(buttons.pressed(HandButton::RightPinch));
    }
    app.update();
    {
        let buttons = app.world().resource::<ButtonInput<HandButton>>();
        assert!(
            buttons.pressed(HandButton::RightPinch),
            "hysteresis: should still be pressed"
        );
        assert!(
            !buttons.just_pressed(HandButton::RightPinch),
            "just_pressed must be false on a held frame (relies on ButtonInput::clear() edge semantics)",
        );
    }
    app.update();
    let buttons = app.world().resource::<ButtonInput<HandButton>>();
    assert!(!buttons.pressed(HandButton::RightPinch));
    assert!(buttons.just_released(HandButton::RightPinch));
}

#[test]
fn grab_crossing_threshold_presses_button() {
    let mut app = test_app_with_mock(MockProvider::with_frames([
        // Frame 1: grab below press threshold → not pressed.
        frame([fake_hand(Chirality::Right, 0.0, 0.5)], 100),
        // Frame 2: grab above press threshold → pressed.
        frame([fake_hand(Chirality::Right, 0.0, 0.9)], 200),
    ]));
    app.update();
    {
        let buttons = app.world().resource::<ButtonInput<HandButton>>();
        assert!(!buttons.pressed(HandButton::RightGrab));
    }
    app.update();
    let buttons = app.world().resource::<ButtonInput<HandButton>>();
    assert!(buttons.pressed(HandButton::RightGrab));
    assert!(buttons.just_pressed(HandButton::RightGrab));
}

#[test]
fn two_hand_frame_sets_both_sides() {
    let mut app = test_app_with_mock(MockProvider::with_frames([frame(
        [
            fake_hand(Chirality::Left, 0.0, 0.0),
            fake_hand(Chirality::Right, 0.0, 0.0),
        ],
        100,
    )]));
    app.update();
    let state = app.world().resource::<HandTrackingState>();
    assert_eq!(state.active_hand_count(), 2);
    assert!(
        state.left().is_some(),
        "left() should find the Left chirality hand"
    );
    assert!(
        state.right().is_some(),
        "right() should find the Right chirality hand"
    );
}

#[test]
fn gesture_events_emitted_for_press_and_release() {
    let mut app = test_app_with_mock(MockProvider::with_frames([
        frame([fake_hand(Chirality::Left, 0.9, 0.0)], 100),
        frame([fake_hand(Chirality::Left, 0.0, 0.0)], 200),
    ]));
    // Frame 1: press.
    app.update();
    {
        let msgs = app.world().resource::<Messages<HandGestureEvent>>();
        let cursor_seen: Vec<_> = msgs.iter_current_update_messages().copied().collect();
        let saw_press = cursor_seen.iter().any(|e| {
            matches!(
                e,
                HandGestureEvent::Pressed {
                    button: HandButton::LeftPinch,
                    ..
                }
            )
        });
        assert!(
            saw_press,
            "expected a Pressed(LeftPinch) event in {cursor_seen:?}"
        );
    }
    // Frame 2: release.
    app.update();
    let msgs = app.world().resource::<Messages<HandGestureEvent>>();
    let cursor_seen: Vec<_> = msgs.iter_current_update_messages().copied().collect();
    let saw_release = cursor_seen.iter().any(|e| {
        matches!(
            e,
            HandGestureEvent::Released {
                button: HandButton::LeftPinch,
                ..
            }
        )
    });
    assert!(
        saw_release,
        "expected a Released(LeftPinch) event in {cursor_seen:?}"
    );
}

#[test]
#[allow(clippy::expect_used)] // `.expect` is acceptable in test assertions.
fn pointer_merge_priority_hand_over_mouse_when_window_present() {
    let mut app = test_app_with_mock(MockProvider::with_frames([frame(
        [{
            let mut h = fake_hand(Chirality::Right, 0.0, 0.0);
            // Index fingertip in Leap device millimetres at the centre of the
            // usable range: x = 0 mm (centre), y = 195 mm (mid of [40, 350]).
            // `pointer_merge_system` projects this via `palm_to_world`, so the
            // pointer lands at the window centre. (The landmark is in mm, NOT
            // NDC — treating it as NDC was the bug this projection fix corrects.)
            h.landmarks[LandmarkIndex::IndexTip.as_index()] = Vec3::new(0.0, 195.0, 0.0);
            h
        }],
        100,
    )]));

    // Spawn a primary window so pointer_merge_system has dimensions to project into.
    // pointer_merge_system queries all &Window entities; a single spawned Window
    // is sufficient — no WindowPlugin or PrimaryWindow marker is required.
    app.world_mut().spawn(Window {
        resolution: WindowResolution::new(800, 600),
        ..default()
    });

    app.update();

    let pointer = *app.world().resource::<PointerState>();
    assert_eq!(pointer.source, PointerSource::Hand);
    let pos = pointer
        .primary
        .expect("hand pointer should have a position");
    // Leap (0 mm, 195 mm) projects to the window center: x = 400, y = 300.
    assert!(
        (pos.x - 400.0).abs() < 0.5,
        "expected x near 400, got {pos:?}"
    );
    assert!(
        (pos.y - 300.0).abs() < 0.5,
        "expected y near 300, got {pos:?}"
    );
}

#[test]
fn pointer_merge_falls_through_to_none_when_no_sources() {
    // With an empty provider and no window, pointer_merge_system has no sources
    // and falls through to PointerSource::None. This also documents that the
    // hand branch is a no-op when HandTrackingState is empty (no hands tracked),
    // and the mouse branch is a no-op when there is no window — both expected
    // fallback behaviors.
    let mut app = test_app_with_mock(MockProvider::with_frames([]));
    app.update();

    let pointer = app.world().resource::<PointerState>();
    assert_eq!(pointer.source, PointerSource::None);
    assert_eq!(pointer.primary, None);
}

#[test]
#[ignore = "TODO Plan 6: re-enable once test infrastructure is richer; boxed-provider mutation and TimeUpdateStrategy semantics are fragile here"]
fn hand_tracking_frame_resets_interaction_timer() {
    use wc_core::lifecycle::idle::InteractionTimer;

    let mut app = test_app_with_mock(MockProvider::with_frames([frame(
        [fake_hand(Chirality::Right, 0.0, 0.0)],
        100,
    )]));
    // Add the lifecycle plugin so InteractionTimer exists.
    app.add_plugins(wc_core::lifecycle::LifecyclePlugin);

    // Tick once with no input.
    app.update();
    let initial = app
        .world()
        .resource::<InteractionTimer>()
        .idle_for(app.world().resource::<Time>().elapsed());

    // Advance time without input — idle_for should grow.
    {
        let mut strategy = app
            .world_mut()
            .resource_mut::<bevy::time::TimeUpdateStrategy>();
        *strategy = bevy::time::TimeUpdateStrategy::ManualDuration(Duration::from_millis(100));
    }
    app.update();

    // Write a hand-tracking frame directly into the Messages resource.
    {
        let mut msgs = app
            .world_mut()
            .resource_mut::<Messages<HandTrackingFrame>>();
        msgs.write(frame([fake_hand(Chirality::Right, 0.0, 0.0)], 300));
    }
    app.update();

    let post = app
        .world()
        .resource::<InteractionTimer>()
        .idle_for(app.world().resource::<Time>().elapsed());
    assert!(
        post < initial + Duration::from_millis(50),
        "expected idle_for to reset after hand-tracking frame; before={initial:?}, after={post:?}",
    );
}
