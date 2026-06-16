//! Coarse hand-tracking activation state for UI cues.
//!
//! [`HandTrackingActivation`] is a single resource the chrome reads to tell the
//! operator whether tracking is live, still coming up, or silently absent —
//! covering states the raw provider [`ServiceConnection`] cannot express on its
//! own. Two of those are why this exists at all:
//!
//! - During Auto's Leap device-grace window the Leap service reports
//!   `Connected` with no device attached, so a service-only cue reads "fine"
//!   while there are no hands.
//! - When Auto exhausts its candidates and installs the silent mock, the mock
//!   reports `Connected`/`Attached`, so a service-only cue again reads "fine"
//!   while there is no real tracking.
//!
//! The binary's provider systems own the watch state, so they publish this
//! resource (see `hand_providers::publish_hand_activation`); wc-core only
//! defines it and inits it to [`HandTrackingActivation::Inactive`] so the panel
//! always has a value to read, including in headless tests and feature-off
//! builds where nothing writes it.

use bevy::prelude::*;

use crate::input::provider::{ProviderId, ProviderRegistry};
use crate::input::state::ServiceConnection;

/// Coarse activation state of hand tracking, for UI cues.
///
/// Deliberately lossy: it answers "should the operator expect hands right now,
/// and if not, why" — not the full multi-axis provider status (that lives in
/// the dev panel). The settings panel maps it to a single status row.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HandTrackingActivation {
    /// No tracking expected: provider `Off` (empty registry) or the
    /// `hand-tracking-gestures` feature is compiled out. The chrome shows
    /// nothing.
    #[default]
    Inactive,
    /// A provider is starting, or Auto is still probing a candidate (a watch is
    /// pending) — tracking is momentarily absent but expected to arrive.
    Settling,
    /// A real tracking provider is connected and streaming.
    Active,
    /// Auto exhausted its candidates and is running the silent mock: the app
    /// runs cleanly but there is no hand tracking.
    FellBackToMock,
    /// The selected provider failed or is unreachable and will not recover
    /// without intervention.
    Failed,
}

/// Map a provider's [`ServiceConnection`] to an activation state.
///
/// Pure and total; the registry-level and watch-level logic composes this.
#[must_use]
pub fn activation_from_service(service: ServiceConnection) -> HandTrackingActivation {
    match service {
        ServiceConnection::NotStarted | ServiceConnection::Connecting => {
            HandTrackingActivation::Settling
        }
        ServiceConnection::Connected => HandTrackingActivation::Active,
        ServiceConnection::Errored
        | ServiceConnection::ServiceMissing
        | ServiceConnection::Disconnected => HandTrackingActivation::Failed,
    }
}

/// Derive the activation state from the registry alone (no watch state).
///
/// The choice-independent half: when a watch is still pending the publisher
/// reports [`HandTrackingActivation::Settling`] directly; otherwise it calls
/// this. An empty registry (provider `Off`) is [`HandTrackingActivation::Inactive`];
/// a mock primary is [`HandTrackingActivation::FellBackToMock`] (the mock reports
/// `Connected`, so it must be caught by identity, not by service state).
#[must_use]
pub fn activation_from_registry(registry: &ProviderRegistry) -> HandTrackingActivation {
    let Some(primary) = registry.primary_id() else {
        return HandTrackingActivation::Inactive;
    };
    if matches!(primary, ProviderId::Mock) {
        return HandTrackingActivation::FellBackToMock;
    }
    activation_from_service(registry.primary_status().service)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::provider::ProviderRole;
    use crate::input::providers::mock::MockProvider;

    #[test]
    fn service_maps_to_activation() {
        use HandTrackingActivation::{Active, Failed, Settling};
        assert_eq!(
            activation_from_service(ServiceConnection::NotStarted),
            Settling
        );
        assert_eq!(
            activation_from_service(ServiceConnection::Connecting),
            Settling
        );
        assert_eq!(
            activation_from_service(ServiceConnection::Connected),
            Active
        );
        assert_eq!(activation_from_service(ServiceConnection::Errored), Failed);
        assert_eq!(
            activation_from_service(ServiceConnection::ServiceMissing),
            Failed
        );
        assert_eq!(
            activation_from_service(ServiceConnection::Disconnected),
            Failed
        );
    }

    #[test]
    fn empty_registry_is_inactive() {
        let registry = ProviderRegistry::default();
        assert_eq!(
            activation_from_registry(&registry),
            HandTrackingActivation::Inactive
        );
    }

    #[test]
    fn mock_primary_reads_as_fell_back_to_mock() {
        // A mock reports Connected/Attached, so without the identity check it
        // would read as Active — the whole point of FellBackToMock.
        let mut registry = ProviderRegistry::default();
        registry.register(
            ProviderId::Mock,
            ProviderRole::Primary,
            Box::new(MockProvider::default()),
        );
        assert_eq!(
            activation_from_registry(&registry),
            HandTrackingActivation::FellBackToMock
        );
    }
}
