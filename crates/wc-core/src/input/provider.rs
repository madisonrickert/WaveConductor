//! [`HandTrackingProvider`] strategy trait and [`ProviderRegistry`] resource.
//!
//! The trait is the internal seam between concrete providers (mock, Leap,
//! WebSocket, future `MediaPipe`) and the rest of the input subsystem. Sketches
//! never touch this trait.
//!
//! Multiple providers can be registered simultaneously in the
//! [`ProviderRegistry`]. `poll_all_providers` drains each one per tick and
//! stamps every emitted [`super::state::HandTrackingFrame`] with the
//! originating [`ProviderId`].

use std::time::Duration;

use bevy::prelude::*;

use super::state::{HandTrackingError, HandTrackingFrame};

/// Identifies a provider in the registry. Plan 11.6 only uses `Leap` and
/// `Mock`; the other variants exist so frame provenance and fusion can
/// distinguish providers once future plans implement them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProviderId {
    /// Native `LeapC` FFI provider.
    Leap,
    /// Scripted-frame mock provider used by tests + auto-fallback.
    Mock,
    /// Future: `WebSocket` bridge for the wasm32 web build.
    WebSocket,
    /// Future: `MediaPipe` webcam provider.
    MediaPipe,
}

impl ProviderId {
    /// Short human-readable label for the dev panel.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ProviderId::Leap => "Leap",
            ProviderId::Mock => "Mock",
            ProviderId::WebSocket => "WebSocket",
            ProviderId::MediaPipe => "MediaPipe",
        }
    }
}

/// What kind of source a provider is. Primary providers' frames win over
/// Simulator providers' frames during fusion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderRole {
    /// Real hand-tracking source (Leap, `MediaPipe`).
    Primary,
    /// Synthetic source (mock for tests, mouse-as-hand for future demos,
    /// recorded playback).
    Simulator,
}

/// One slot in the [`ProviderRegistry`].
pub struct RegisteredProvider {
    /// Identity of the provider in this slot.
    pub id: ProviderId,
    /// Role (Primary vs Simulator) for fusion precedence.
    pub role: ProviderRole,
    /// The boxed provider implementation.
    pub inner: Box<dyn HandTrackingProvider>,
}

/// Strategy trait implemented by every concrete hand-tracking provider.
///
/// Providers are registered with the [`ProviderRegistry`] resource at
/// startup. `poll_all_providers` runs each provider's [`Self::poll`] once
/// per `PreUpdate` tick and stamps each emitted frame with the provider's
/// [`ProviderId`].
pub trait HandTrackingProvider: Send + Sync + 'static {
    /// Start the provider. Must be called before [`Self::poll`] returns
    /// meaningful results. Returns an error if the provider cannot acquire
    /// its hardware / transport.
    fn start(&mut self) -> Result<(), HandTrackingError>;

    /// Stop the provider cleanly.
    fn stop(&mut self);

    /// Drain frames produced since the last call into `out`. Called once
    /// per `PreUpdate` tick.
    ///
    /// `now` is the Bevy main-thread elapsed time, supplied so providers
    /// can stamp frames consistently when their own clock is unavailable.
    /// Providers do NOT set the `provider:` field — `poll_all_providers`
    /// stamps it after this call returns.
    fn poll(&mut self, now: Duration, out: &mut Messages<HandTrackingFrame>);

    /// Multi-axis snapshot of the provider's lifecycle and health.
    /// Updated each `poll()`.
    fn status(&self) -> crate::input::state::ProviderStatus;

    /// Provider-level diagnostic metadata for the dev panel. Updated each
    /// `poll()` (or `start()` for static fields like SDK version).
    fn diagnostics(&self) -> crate::input::state::ProviderDiagnostics;

    /// The inference backend this provider is *actually* running on, if the
    /// concept applies to it and it has started (e.g. `MediaPipe`'s
    /// `"ort/CoreML"`, `"ort/CPU"`, or the degraded mixed `"ort/CoreML+CPU"`).
    /// `None` on providers with no inference stage (Leap, mock) and before the
    /// provider's sessions exist.
    ///
    /// A `&'static str` by design, not a `String`: this is read by the settings
    /// panel every frame it is open (see
    /// `settings::panel_user::provider_status`), so it must cost a pointer copy —
    /// no allocation, no lock. `diagnostics()` carries the same value as a
    /// `Backend` metric, but only as a by-clone snapshot of the whole diagnostics
    /// struct.
    ///
    /// Default returns `None`; only providers that run inference override.
    fn backend_label(&self) -> Option<&'static str> {
        None
    }

    /// Downcast helper for systems that need to call typed methods on a
    /// concrete provider type (e.g., `apply_leap_background_setting`).
    ///
    /// Default returns `None`; only providers with typed-method needs override.
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }
}

/// Resource holding all currently-installed [`HandTrackingProvider`]s.
///
/// Replaces the singleton `ActiveProvider` from Plan 3. Multi-provider
/// support enables future fusion (Leap + `MediaPipe`), simulator sources,
/// and clean lifecycle (each provider can independently start/stop).
///
/// The binary populates this resource at startup via auto-selection
/// (see `crates/waveconductor/src/hand_providers.rs`).
/// Tests construct their own registry directly.
#[derive(Resource, Default)]
pub struct ProviderRegistry {
    providers: Vec<RegisteredProvider>,
}

impl ProviderRegistry {
    /// Register a provider. Idempotent on ID — re-registering the same
    /// ID replaces the previous entry (useful for tests).
    ///
    /// Calls `HandTrackingProvider::start` eagerly and logs the result.
    /// If `start` returns an error, the provider is still registered so
    /// `Res<ProviderRegistry>` is always populated; callers that need to
    /// confirm readiness should check the provider's `status()`.
    pub fn register(
        &mut self,
        id: ProviderId,
        role: ProviderRole,
        mut inner: Box<dyn HandTrackingProvider>,
    ) {
        if let Err(err) = inner.start() {
            tracing::warn!(?err, provider = id.label(), "provider failed to start");
        }
        if let Some(slot) = self.providers.iter_mut().find(|p| p.id == id) {
            *slot = RegisteredProvider { id, role, inner };
            return;
        }
        self.providers.push(RegisteredProvider { id, role, inner });
    }

    /// Remove a provider, stopping it first. Returns the removed slot, or
    /// `None` when the ID is not registered.
    ///
    /// [`Self::register`] auto-starts but nothing else in the registry
    /// auto-stops: dropping a provider does release its resources eventually
    /// (e.g. the `MediaPipe` worker handle joins in `Drop`), but the explicit
    /// `stop()` here makes teardown synchronous — by the time `remove`
    /// returns, a `MediaPipe` worker has been joined (camera released) and a
    /// Leap connection dropped, so a replacement provider registered next can
    /// immediately re-acquire the hardware.
    pub fn remove(&mut self, id: ProviderId) -> Option<RegisteredProvider> {
        let idx = self.providers.iter().position(|p| p.id == id)?;
        let mut slot = self.providers.remove(idx);
        slot.inner.stop();
        tracing::info!(provider = id.label(), "provider stopped and removed");
        Some(slot)
    }

    /// Stop and remove every provider, in registration order.
    ///
    /// Used by the live provider switch: the old registry must be fully torn
    /// down (workers joined, camera/device released — see [`Self::remove`])
    /// *before* the replacement providers start.
    pub fn shutdown_all(&mut self) {
        while let Some(id) = self.providers.first().map(|p| p.id) {
            self.remove(id);
        }
    }

    /// Iterate over registered providers.
    pub fn iter(&self) -> impl Iterator<Item = &RegisteredProvider> + '_ {
        self.providers.iter()
    }

    /// Iterate mutably (used by polling).
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut RegisteredProvider> + '_ {
        self.providers.iter_mut()
    }

    /// Look up a provider by ID.
    #[must_use]
    pub fn provider(&self, id: ProviderId) -> Option<&RegisteredProvider> {
        self.providers.iter().find(|p| p.id == id)
    }

    /// The slot every `primary_*` accessor below reports on: the primary
    /// provider, or — when none is registered — the first simulator, so a
    /// simulator-only registry still has something to show.
    fn primary_slot(&self) -> Option<&RegisteredProvider> {
        self.providers
            .iter()
            .find(|p| p.role == ProviderRole::Primary)
            .or_else(|| {
                self.providers
                    .iter()
                    .find(|p| p.role == ProviderRole::Simulator)
            })
    }

    /// Status of the primary provider (or, if none, the first simulator).
    /// What the status LED reads.
    #[must_use]
    pub fn primary_status(&self) -> crate::input::state::ProviderStatus {
        self.primary_slot()
            .map_or_else(crate::input::state::ProviderStatus::default, |p| {
                p.inner.status()
            })
    }

    /// Diagnostics of the primary provider, for the dev panel.
    #[must_use]
    pub fn primary_diagnostics(&self) -> crate::input::state::ProviderDiagnostics {
        self.primary_slot()
            .map_or_else(crate::input::state::ProviderDiagnostics::default, |p| {
                p.inner.diagnostics()
            })
    }

    /// Inference backend the primary provider is actually running on (see
    /// [`HandTrackingProvider::backend_label`]), for the settings panel's
    /// "Running:" row under the inference-backend dropdown. `None` when no
    /// provider is registered, or when the primary one runs no inference (Leap,
    /// mock) or has not started its sessions yet.
    ///
    /// Allocation- and lock-free (a `&'static str` copy), so the panel may read
    /// it on every frame it is open.
    #[must_use]
    pub fn primary_backend_label(&self) -> Option<&'static str> {
        self.primary_slot()?.inner.backend_label()
    }

    /// ID of the primary provider, for the dev panel label.
    #[must_use]
    pub fn primary_id(&self) -> Option<ProviderId> {
        self.primary_slot().map(|p| p.id)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;

    /// Provider that records `stop()` calls, for registry lifecycle tests.
    struct StopSpy {
        stops: Arc<AtomicUsize>,
    }

    impl HandTrackingProvider for StopSpy {
        fn start(&mut self) -> Result<(), HandTrackingError> {
            Ok(())
        }

        fn stop(&mut self) {
            self.stops.fetch_add(1, Ordering::SeqCst);
        }

        fn poll(&mut self, _now: Duration, _out: &mut Messages<HandTrackingFrame>) {}

        fn status(&self) -> crate::input::state::ProviderStatus {
            crate::input::state::ProviderStatus::default()
        }

        fn diagnostics(&self) -> crate::input::state::ProviderDiagnostics {
            crate::input::state::ProviderDiagnostics::default()
        }
    }

    fn spy(
        registry: &mut ProviderRegistry,
        id: ProviderId,
        role: ProviderRole,
    ) -> Arc<AtomicUsize> {
        let stops = Arc::new(AtomicUsize::new(0));
        registry.register(
            id,
            role,
            Box::new(StopSpy {
                stops: Arc::clone(&stops),
            }),
        );
        stops
    }

    #[test]
    fn remove_stops_the_provider_and_takes_it_out() {
        let mut registry = ProviderRegistry::default();
        let stops = spy(&mut registry, ProviderId::Mock, ProviderRole::Simulator);

        let removed = registry.remove(ProviderId::Mock);
        assert!(removed.is_some());
        assert_eq!(stops.load(Ordering::SeqCst), 1, "remove() must call stop()");
        assert!(registry.provider(ProviderId::Mock).is_none());
    }

    #[test]
    fn remove_unknown_id_is_none_and_touches_nothing() {
        let mut registry = ProviderRegistry::default();
        let stops = spy(&mut registry, ProviderId::Mock, ProviderRole::Simulator);

        assert!(registry.remove(ProviderId::Leap).is_none());
        assert_eq!(stops.load(Ordering::SeqCst), 0);
        assert!(registry.provider(ProviderId::Mock).is_some());
    }

    #[test]
    fn shutdown_all_stops_every_provider_and_empties_the_registry() {
        let mut registry = ProviderRegistry::default();
        let a = spy(&mut registry, ProviderId::Leap, ProviderRole::Primary);
        let b = spy(&mut registry, ProviderId::Mock, ProviderRole::Simulator);

        registry.shutdown_all();
        assert_eq!(a.load(Ordering::SeqCst), 1);
        assert_eq!(b.load(Ordering::SeqCst), 1);
        assert_eq!(registry.iter().count(), 0);
        assert!(registry.primary_id().is_none());
    }
}
