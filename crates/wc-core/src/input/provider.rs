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
/// (see `crates/waveconductor/src/main.rs::install_hand_tracking_providers`).
/// Tests construct their own registry directly.
#[derive(Resource, Default)]
pub struct ProviderRegistry {
    providers: Vec<RegisteredProvider>,
}

impl ProviderRegistry {
    /// Register a provider. Idempotent on ID — re-registering the same
    /// ID replaces the previous entry (useful for tests).
    ///
    /// Calls [`HandTrackingProvider::start`] eagerly and logs the result.
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

    /// Status of the primary provider (or, if none, the first simulator).
    /// What the status LED reads.
    #[must_use]
    pub fn primary_status(&self) -> crate::input::state::ProviderStatus {
        self.providers
            .iter()
            .find(|p| p.role == ProviderRole::Primary)
            .or_else(|| {
                self.providers
                    .iter()
                    .find(|p| p.role == ProviderRole::Simulator)
            })
            .map_or_else(crate::input::state::ProviderStatus::default, |p| {
                p.inner.status()
            })
    }

    /// Diagnostics of the primary provider, for the dev panel.
    #[must_use]
    pub fn primary_diagnostics(&self) -> crate::input::state::ProviderDiagnostics {
        self.providers
            .iter()
            .find(|p| p.role == ProviderRole::Primary)
            .or_else(|| {
                self.providers
                    .iter()
                    .find(|p| p.role == ProviderRole::Simulator)
            })
            .map_or_else(crate::input::state::ProviderDiagnostics::default, |p| {
                p.inner.diagnostics()
            })
    }

    /// ID of the primary provider, for the dev panel label.
    #[must_use]
    pub fn primary_id(&self) -> Option<ProviderId> {
        self.providers
            .iter()
            .find(|p| p.role == ProviderRole::Primary)
            .or_else(|| {
                self.providers
                    .iter()
                    .find(|p| p.role == ProviderRole::Simulator)
            })
            .map(|p| p.id)
    }
}
