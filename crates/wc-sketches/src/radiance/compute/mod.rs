//! Render-world compute for the Radiance aura: GPU POD mirrors
//! ([`sim_params`]), the dispatch pipeline (`pipeline`), and the
//! silhouette-edge storage-buffer upload (`edge_upload`).

pub mod sim_params;

// `pipeline` and `edge_upload` consume `wc_core::input::body` (`EdgePoint`,
// `SilhouetteEdges`, `MAX_EDGE_POINTS`), which wc-core gates behind this
// feature (camera-independent, CI-testable headless). The `cargo doc` gate
// builds default features only, so these modules must be absent there — see
// `Cargo.toml`'s `body-tracking-mediapipe` forwarding feature, and
// `radiance::systems::mod` for the identical precedent.
#[cfg(feature = "body-tracking-mediapipe")]
pub mod edge_upload;
#[cfg(feature = "body-tracking-mediapipe")]
pub mod pipeline;
