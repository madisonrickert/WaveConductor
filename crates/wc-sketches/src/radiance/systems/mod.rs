//! Main-world Radiance systems: spawn/teardown, the per-frame sim baker,
//! activity sync, camera arbitration, and dev/debug drivers.

// Consumes `wc_core::input::body`, which wc-core gates behind this feature
// (camera-independent, CI-testable headless). The `cargo doc` gate builds
// default features only, so this module must be absent there — see
// `Cargo.toml`'s `body-tracking-mediapipe` forwarding feature.
#[cfg(feature = "body-tracking-mediapipe")]
pub mod sim_params;
