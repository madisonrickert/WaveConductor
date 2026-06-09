//! Concrete [`super::provider::HandTrackingProvider`] implementations.
//!
//! - [`mock`] — scripted-frame playback, used by tests and as a simulator
//!   source in the [`super::provider::ProviderRegistry`].
//! - [`leap_native`] — native `LeapC` FFI provider; stub in this plan, real
//!   implementation in a future plan.
//! - [`websocket`] — WebSocket-based provider (used for web target and as a
//!   native dev fallback); stub in this plan.

pub mod leap_native;
pub mod mock;
pub mod websocket;

/// In-process MediaPipe webcam provider (palm→landmark ONNX via `ort`/CoreML).
/// Compiled only when the `hand-tracking-mediapipe` feature is enabled.
#[cfg(feature = "hand-tracking-mediapipe")]
pub mod mediapipe;
