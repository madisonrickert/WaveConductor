//! Concrete [`super::provider::HandTrackingProvider`] implementations.
//!
//! - [`mock`] — scripted-frame playback, used by tests and the default
//!   `ActiveProvider`.
//! - [`leap_native`] — native `LeapC` FFI provider; stub in this plan, real
//!   implementation in a future plan.
//! - [`websocket`] — WebSocket-based provider (used for web target and as a
//!   native dev fallback); stub in this plan.

pub mod leap_native;
pub mod mock;
pub mod websocket;
