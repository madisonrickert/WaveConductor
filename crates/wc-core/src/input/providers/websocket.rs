//! Stub WebSocket-based provider.
//!
//! Targets:
//! - Web build (wasm32): the only viable provider, since browsers cannot
//!   access Leap hardware directly.
//! - Native dev: a fallback for when the developer has an external Ultraleap
//!   WebSocket compatibility server running locally (the v4 setup).
//!
//! Compiles on all targets but is no-op. A real implementation lands in a
//! future plan after Plan 6 (Line sketch) demonstrates the need.

use std::time::Duration;

use bevy::prelude::*;

use crate::input::provider::HandTrackingProvider;
use crate::input::state::{
    HandTrackingError, HandTrackingFrame, ProviderDiagnostics, ProviderStatus,
};

/// Stub `WebSocketProvider`. Real implementation deferred to a future plan.
#[derive(Default)]
pub struct WebSocketProvider;

impl HandTrackingProvider for WebSocketProvider {
    fn start(&mut self) -> Result<(), HandTrackingError> {
        tracing::warn!(
            "WebSocketProvider is a stub in this plan; install a real provider \
             (see docs/superpowers/specs/2026-05-22-bevy-rewrite-design.md §5.3)"
        );
        Err(HandTrackingError::Unavailable(
            "WebSocketProvider is not implemented in Plan 3".into(),
        ))
    }

    fn stop(&mut self) {}

    fn poll(&mut self, _now: Duration, _out: &mut Messages<HandTrackingFrame>) {}

    fn status(&self) -> ProviderStatus {
        ProviderStatus::default()
    }

    fn diagnostics(&self) -> ProviderDiagnostics {
        ProviderDiagnostics::default()
    }
}
