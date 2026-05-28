//! Stub native `LeapC` provider.
//!
//! Compiles on all targets but is no-op. A real implementation using the
//! `leaprs` crate (`LeapC` bindings) lands in a future plan after Plan 6 (Line
//! sketch) demonstrates the need for real Leap data.
//!
//! ## Why a stub instead of nothing
//!
//! The plugin's `ProviderRegistry` accepts any `Box<dyn HandTrackingProvider>`.
//! Until the real Leap impl exists, code that wants to *select* this provider
//! at startup needs something to construct. The stub satisfies that surface,
//! returns [`crate::input::state::ProviderStatus::default()`], and logs a clear
//! warning when `start` is called, so misconfigurations surface immediately at
//! runtime.

use std::time::Duration;

use bevy::prelude::*;

use crate::input::provider::HandTrackingProvider;
use crate::input::state::{HandTrackingError, HandTrackingFrame, ProviderDiagnostics, ProviderStatus};

/// Stub `LeaprsProvider`. Real implementation deferred to a future plan.
#[derive(Default)]
pub struct LeaprsProvider;

impl HandTrackingProvider for LeaprsProvider {
    fn start(&mut self) -> Result<(), HandTrackingError> {
        tracing::warn!(
            "LeaprsProvider is a stub in this plan; install a real provider \
             (see docs/superpowers/specs/2026-05-22-bevy-rewrite-design.md §5.3)"
        );
        Err(HandTrackingError::Unavailable(
            "LeaprsProvider is not implemented in Plan 3".into(),
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
