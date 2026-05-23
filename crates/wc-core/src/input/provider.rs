//! [`HandTrackingProvider`] strategy trait and [`ActiveProvider`] resource.
//!
//! The trait is the internal seam between concrete providers (mock, Leap,
//! WebSocket, future `MediaPipe`) and the rest of the input subsystem. Sketches
//! never touch this trait.

use std::time::Duration;

use bevy::prelude::*;

use super::providers::mock::MockProvider;
use super::state::{HandTrackingError, HandTrackingFrame, HandTrackingStatus};

/// Strategy trait implemented by every concrete hand-tracking provider.
///
/// One provider is active at a time; selected at app startup and installed as
/// the [`ActiveProvider`] resource. The plugin's
/// [`crate::input::systems::poll_active_provider`] system calls
/// [`Self::poll`] once per `PreUpdate` tick to drain whatever frames the
/// provider has produced since the last poll.
pub trait HandTrackingProvider: Send + Sync + 'static {
    /// Start the provider. Must be called before [`Self::poll`] returns
    /// meaningful results. Returns an error if the provider cannot acquire
    /// its hardware / transport.
    fn start(&mut self) -> Result<(), HandTrackingError>;

    /// Stop the provider. After this returns, [`Self::status`] should report
    /// [`HandTrackingStatus::Disconnected`].
    fn stop(&mut self);

    /// Drain any frames produced since the last call and append them to
    /// `out`. Called once per `PreUpdate` tick by the plugin.
    ///
    /// `now` is the Bevy main-thread elapsed time, supplied so providers can
    /// stamp frames consistently when their own clock is unavailable (e.g.,
    /// mock provider in tests).
    fn poll(&mut self, now: Duration, out: &mut Messages<HandTrackingFrame>);

    /// Current lifecycle status; read by the UI status indicator.
    fn status(&self) -> HandTrackingStatus;
}

/// Resource holding the currently-installed [`HandTrackingProvider`].
///
/// Boxed for trait-object polymorphism. The binary swaps the default mock
/// provider for a real one (Leap, WebSocket, `MediaPipe`) at startup based on
/// app configuration.
#[derive(Resource)]
pub struct ActiveProvider {
    /// The boxed provider implementation.
    pub(crate) inner: Box<dyn HandTrackingProvider>,
}

impl Default for ActiveProvider {
    /// Defaults to an empty [`MockProvider`] so tests and headless builds work
    /// out of the box.
    fn default() -> Self {
        Self {
            inner: Box::new(MockProvider::default()),
        }
    }
}

impl ActiveProvider {
    /// Wrap a provider impl as the active provider. Calls [`HandTrackingProvider::start`] eagerly
    /// and logs the result.
    #[must_use]
    pub fn new<P: HandTrackingProvider>(mut provider: P) -> Self {
        if let Err(err) = provider.start() {
            tracing::warn!(?err, "active provider failed to start");
        }
        Self {
            inner: Box::new(provider),
        }
    }
}
