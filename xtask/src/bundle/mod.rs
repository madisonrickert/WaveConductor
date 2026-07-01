//! `cargo xtask bundle-*` — release packaging, one submodule per target platform.
//!
//! Each `run()` builds the release binary (unless `--skip-build`) and assembles a
//! self-contained layout for its platform:
//!
//! - [`mac`] → a `WaveConductor.app` bundle (`Info.plist`, generated `.icns`).
//! - [`linux`] → a flat staging dir with the binary, `libLeapC.so.6`, and assets.
//! - [`windows`] → a flat staging dir with the `.exe`, `LeapC.dll`, and assets.
//!
//! [`common`] holds the file-copy / size / build primitives shared by all three;
//! the platform layout differences live in each sibling module. The release
//! workflow (`.github/workflows/release.yml`) runs the matching subcommand on each
//! native runner and archives the result. Code-signing / notarization is out of
//! scope on every platform.

pub mod common;
pub mod linux;
pub mod mac;
pub mod windows;
