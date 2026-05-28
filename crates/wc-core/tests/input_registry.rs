//! Integration tests for the multi-provider `ProviderRegistry` + entity sync.

use std::time::Duration;

use bevy::input::InputPlugin;
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use wc_core::input::entity::TrackedHand;
use wc_core::input::hand::{Chirality, Hand, LANDMARK_COUNT};
use wc_core::input::provider::{ProviderId, ProviderRegistry, ProviderRole};
use wc_core::input::providers::mock::MockProvider;
use wc_core::input::state::HandTrackingFrame;
use wc_core::input::HandTrackingPlugin;

fn test_hand(id: u32, chirality: Chirality) -> Hand {
    Hand {
        id,
        chirality,
        palm_position: Vec3::new(0.0, 200.0, 0.0),
        palm_normal: Vec3::Y,
        palm_velocity: Vec3::ZERO,
        pinch_strength: 0.0,
        grab_strength: 0.0,
        landmarks: [Vec3::ZERO; LANDMARK_COUNT],
    }
}

fn frame_with(hands: Vec<Hand>, t_ms: u64) -> HandTrackingFrame {
    HandTrackingFrame {
        provider: ProviderId::Mock,
        hands: hands.into_iter().collect(),
        timestamp: Duration::from_millis(t_ms),
    }
}

fn make_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_plugins(InputPlugin)
        .add_plugins(StatesPlugin)
        .add_plugins(HandTrackingPlugin);
    app
}

#[test]
fn mock_provider_through_registry_spawns_one_tracked_hand_per_hand_in_frame() {
    let mut app = make_app();

    let mut registry = ProviderRegistry::default();
    let mock = MockProvider::with_frames([frame_with(vec![test_hand(1, Chirality::Right)], 10)]);
    registry.register(ProviderId::Mock, ProviderRole::Simulator, Box::new(mock));
    app.insert_resource(registry);

    // One tick: poll → fuse → sync.
    app.update();

    let world = app.world_mut();
    let count = world.query::<&TrackedHand>().iter(world).count();
    assert_eq!(count, 1, "expected one TrackedHand entity");
}

#[test]
fn tracked_hand_despawns_when_hand_leaves_frame_stream() {
    let mut app = make_app();

    let mut registry = ProviderRegistry::default();
    let mock = MockProvider::with_frames([
        frame_with(vec![test_hand(1, Chirality::Right)], 10),
        frame_with(vec![], 20), // hand 1 leaves
    ]);
    registry.register(ProviderId::Mock, ProviderRole::Simulator, Box::new(mock));
    app.insert_resource(registry);

    app.update(); // tick 1: spawn
    {
        let world = app.world_mut();
        let count_after_1 = world.query::<&TrackedHand>().iter(world).count();
        assert_eq!(count_after_1, 1);
    }

    app.update(); // tick 2: despawn
    {
        let world = app.world_mut();
        let count_after_2 = world.query::<&TrackedHand>().iter(world).count();
        assert_eq!(count_after_2, 0);
    }
}

#[test]
fn same_hand_id_across_frames_updates_in_place_no_respawn() {
    let mut app = make_app();

    let mut registry = ProviderRegistry::default();
    let mut h = test_hand(42, Chirality::Left);
    h.palm_position = Vec3::new(-100.0, 150.0, 0.0);
    let mut h2 = h.clone();
    h2.palm_position = Vec3::new(100.0, 250.0, 0.0);
    let mock = MockProvider::with_frames([
        frame_with(vec![h], 10),
        frame_with(vec![h2], 20),
    ]);
    registry.register(ProviderId::Mock, ProviderRole::Simulator, Box::new(mock));
    app.insert_resource(registry);

    app.update();
    {
        let world = app.world_mut();
        let count_after_1 = world.query::<&TrackedHand>().iter(world).count();
        assert_eq!(count_after_1, 1);
    }

    app.update();
    {
        let world = app.world_mut();
        let count = world.query::<&TrackedHand>().iter(world).count();
        assert_eq!(count, 1, "should still be one entity, updated in place");
    }
}
