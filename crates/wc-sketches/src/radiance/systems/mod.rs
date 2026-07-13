//! Main-world Radiance systems: spawn/teardown, the per-frame sim baker,
//! activity sync, camera arbitration, and dev/debug drivers.

// Spawn/teardown allocates the particle buffer + billboard mesh (no body
// dependency) but also inserts `BodyTrackingRequest`/`MaskTexture`/
// `SilhouetteEdges` and builds `RadianceSilhouetteMaterial`, all of which
// live in modules wc-core/`radiance::render` gate behind this feature
// (camera-independent, CI-testable headless). The `cargo doc` gate builds
// default features only, so this module must be absent there — see
// `Cargo.toml`'s `body-tracking-mediapipe` forwarding feature, and
// `radiance::compute::mod`/`radiance::render` for the identical precedent.
// `RadianceRoot` itself (the marker `radiance::render`'s already-gated
// driver queries) therefore lives behind the same gate as its one spawn
// site; nothing outside this feature needs to name it.
#[cfg(feature = "body-tracking-mediapipe")]
pub mod spawn;

// Consumes `wc_core::input::body`, which wc-core gates behind this feature
// (camera-independent, CI-testable headless). The `cargo doc` gate builds
// default features only, so this module must be absent there — see
// `Cargo.toml`'s `body-tracking-mediapipe` forwarding feature.
#[cfg(feature = "body-tracking-mediapipe")]
pub mod sim_params;

// `SketchActivity` → activation-request sync. Consumes
// `wc_core::input::body::BodyTrackingRequest` (gated behind this feature)
// alongside the unconditional `wc_core::audio::input::AudioCaptureRequest`,
// so the whole module is gated identically to `spawn`/`sim_params` above —
// same `cargo doc` default-features-only rationale.
#[cfg(feature = "body-tracking-mediapipe")]
pub mod activity;

// Camera arbitration consumes only `wc_core::input::provider`, an
// unconditional module (no body/hand feature gate) — so this module builds
// in every configuration, including the default-features doc gate.
pub mod arbitration;
