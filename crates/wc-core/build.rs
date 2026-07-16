//! Build-time linker setup for the vendored `LeapC` library on Windows.
//!
//! `wc-core` is the crate that owns the `leaprs` dependency (behind the
//! `hand-tracking-gestures` feature). `leaprs`'s own build script emits the
//! `-lLeapC` link directive plus a search path derived from `LEAPSDK_LIB_PATH`,
//! but it resolves that path *relative to its own crate directory in the cargo
//! registry* — and the workspace `.cargo/config.toml` sets `LEAPSDK_LIB_PATH`
//! (non-forced) to the macOS vendor dir as the primary-platform default. On a
//! fresh Windows checkout that default points at a directory with no
//! `LeapC.lib`, so every binary that links `leaprs` — the app, wc-core's own
//! test and example binaries, and dependents like wc-sketches — fails to link
//! with unresolved `Leap*` externals.
//!
//! Emitting an absolute, host-correct search path here fixes all of them at
//! once: a build script's `rustc-link-search` is inherited by every binary that
//! transitively links this crate, exactly as `leaprs`'s own directives are. The
//! path is built from `CARGO_MANIFEST_DIR`, so it is independent of where the
//! repo is cloned. We deliberately emit *only* the search path, not
//! `-lLeapC` — `leaprs` already emits the lib when its feature is enabled, so
//! this stays a no-op extra `-L` when hand tracking is compiled out.
//!
//! No-op on non-Windows targets, which resolve `LeapC` via the `.cargo/config.toml`
//! rpath stanzas + `leaprs`'s default path as before.

fn main() {
    #[cfg(target_os = "windows")]
    {
        let leapc_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../vendor/leapc/windows-x86_64");
        println!("cargo:rerun-if-changed=../../vendor/leapc/windows-x86_64/LeapC.lib");
        println!("cargo:rustc-link-search=native={}", leapc_dir.display());
    }
}
