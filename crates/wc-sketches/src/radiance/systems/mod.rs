//! Main-world Radiance systems: spawn/teardown, the per-frame sim baker,
//! activity sync, camera arbitration, and dev/debug drivers.

// `RadianceRoot` is a plain marker component (no body-type dependency), so
// unlike `sim_params` below it is unconditional; the material driver
// (`radiance::render::drive_radiance_materials`) queries it under the same
// feature gate as `sim_params`, but the marker itself must exist in every
// build.
pub mod spawn;

// Consumes `wc_core::input::body`, which wc-core gates behind this feature
// (camera-independent, CI-testable headless). The `cargo doc` gate builds
// default features only, so this module must be absent there — see
// `Cargo.toml`'s `body-tracking-mediapipe` forwarding feature.
#[cfg(feature = "body-tracking-mediapipe")]
pub mod sim_params;
