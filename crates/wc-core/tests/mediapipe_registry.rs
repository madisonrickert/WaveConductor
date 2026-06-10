//! The `MediaPipe` provider registers in the `ProviderRegistry` and is selectable
//! as the primary provider — the integration seam the startup wiring relies on.
#![cfg(feature = "hand-tracking-mediapipe")]

use wc_core::input::provider::{ProviderId, ProviderRegistry, ProviderRole};
use wc_core::input::providers::mediapipe::{MediaPipeConfig, MediaPipeProvider};

#[test]
fn mediapipe_provider_registers_as_primary() {
    let mut registry = ProviderRegistry::default();
    registry.register(
        ProviderId::MediaPipe,
        ProviderRole::Primary,
        Box::new(MediaPipeProvider::new(MediaPipeConfig::default())),
    );

    assert!(registry.provider(ProviderId::MediaPipe).is_some());
    assert_eq!(registry.primary_id(), Some(ProviderId::MediaPipe));
}

#[test]
fn registering_mediapipe_does_not_disturb_other_slots() {
    use wc_core::input::providers::mock::MockProvider;

    let mut registry = ProviderRegistry::default();
    registry.register(
        ProviderId::Mock,
        ProviderRole::Simulator,
        Box::new(MockProvider::default()),
    );
    registry.register(
        ProviderId::MediaPipe,
        ProviderRole::Primary,
        Box::new(MediaPipeProvider::new(MediaPipeConfig::default())),
    );

    // Primary (MediaPipe) wins primary_id; the Mock simulator slot survives.
    assert_eq!(registry.primary_id(), Some(ProviderId::MediaPipe));
    assert!(registry.provider(ProviderId::Mock).is_some());
}
