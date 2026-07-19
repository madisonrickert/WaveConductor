//! Platform facade for OBSBOT device IO.
//!
//! The vendored libdev SDK binary is linked on **Windows only** (the
//! deployment target; `vendor/libdev` also ships Linux/macOS binaries, but
//! linking them would drag a C++ toolchain + runtime-library staging into
//! every CI runner for hardware only the Windows box has). Following the
//! `lifecycle/thermal/platform/` convention:
//!
//! - `windows` — the real backend: extern "C" bindings to
//!   `vendor/libdev/shim/obsbot_shim.h` plus the dedicated worker thread that
//!   owns all device IO (SDK setters can block for a device round-trip, so
//!   they must never run on the Bevy schedule).
//! - `stub` — everywhere else: [`spawn_worker`] returns `None`, nothing
//!   links, and the module stays compile-checked by every platform's
//!   `--all-features` build.
//!
//! Both export the same two names, so `super` code is platform-agnostic:
//! `spawn_worker(take_control) -> Option<WorkerHandle>` and the
//! [`WorkerHandle`] type with `send` / `try_recv_status`.

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "windows")]
pub use windows::{spawn_worker, WorkerHandle};

#[cfg(not(target_os = "windows"))]
pub mod stub;

#[cfg(not(target_os = "windows"))]
pub use stub::{spawn_worker, WorkerHandle};
