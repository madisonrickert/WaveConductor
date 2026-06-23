//! Platform-level runtime utilities for `WaveConductor`.
//!
//! Helpers that are not tied to a specific subsystem (audio, input, lifecycle)
//! but are consumed across crates. Currently provides the asset-root resolver
//! used by the binary, the `MediaPipe` provider, and the visual-testing harness
//! to locate `assets/` at runtime across dev, release, and `.app` bundle
//! deployments.

pub mod assets;
