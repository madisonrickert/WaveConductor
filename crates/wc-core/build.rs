//! Build-time linker setup for the vendored native SDKs on Windows: `LeapC`
//! (hand tracking) and, behind the `obsbot-camera-control` feature, the OBSBOT
//! `libdev` camera-control SDK.
//!
//! ## `LeapC`
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
//! ## OBSBOT `libdev` (`obsbot-camera-control` feature)
//!
//! libdev's API is C++ (classes, `std::string`, `std::function`), so bindgen
//! cannot consume it. `vendor/libdev/shim/obsbot_shim.{h,cpp}` is a
//! hand-written extern "C" facade; this script compiles it with the `cc` crate
//! and links the vendored import library. Windows-only by design: the Rust
//! module (`wc_core::input::obsbot`) compiles a no-op facade elsewhere, so
//! CI's `--all-features` on Linux/macOS runners never touches a C++ toolchain
//! or a libdev binary here. The runtime DLLs (`libdev.dll`, `w32-pthreads.dll`)
//! are staged into `target/<profile>/` and `target/<profile>/deps/` so both the
//! app exe and workspace test binaries resolve them via adjacent-file /
//! link-search-path discovery (the same convention as `LeapC.dll`, which
//! `crates/waveconductor/build.rs` stages for the app).
//!
//! No-op on non-Windows targets, which resolve `LeapC` via the `.cargo/config.toml`
//! rpath stanzas + `leaprs`'s default path as before (and compile no OBSBOT
//! shim at all).

// A build script signals failure by panicking — the sanctioned way to fail a
// build when a setup-time invariant (a vendored file that must exist, a copy
// that must succeed) is violated. Scope the allow to this compilation unit;
// runtime code keeps the strict lints.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "build scripts fail the build by panicking; setup-time invariants, not runtime paths"
)]

fn main() {
    #[cfg(target_os = "windows")]
    {
        let leapc_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../vendor/leapc/windows-x86_64");
        println!("cargo:rerun-if-changed=../../vendor/leapc/windows-x86_64/LeapC.lib");
        println!("cargo:rustc-link-search=native={}", leapc_dir.display());
    }

    // OBSBOT libdev shim — Windows + `obsbot-camera-control` only. The feature
    // check is an env probe (not a cfg) because build scripts see features via
    // CARGO_FEATURE_*; the cfg(windows) above/below refers to the *host*, which
    // equals the target for every supported build of this project.
    #[cfg(target_os = "windows")]
    if std::env::var_os("CARGO_FEATURE_OBSBOT_CAMERA_CONTROL").is_some() {
        build_obsbot_shim();
    }
}

/// Compile `vendor/libdev/shim/obsbot_shim.cpp`, link `libdev.lib`, and stage
/// the runtime DLLs beside every binary the workspace produces in this target
/// directory.
#[cfg(target_os = "windows")]
fn build_obsbot_shim() {
    let libdev_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/libdev");
    let lib_dir = libdev_root.join("windows/win64-release");

    println!("cargo:rerun-if-changed=../../vendor/libdev/shim/obsbot_shim.cpp");
    println!("cargo:rerun-if-changed=../../vendor/libdev/shim/obsbot_shim.h");
    println!("cargo:rerun-if-changed=../../vendor/libdev/windows/win64-release/libdev.lib");

    // CRT contract: libdev.dll ships as a release-CRT (/MD) binary and its API
    // passes MSVC STL types (std::string/std::function/std::shared_ptr) across
    // the DLL boundary. Rust's MSVC target also always links the release CRT.
    // `.debug(false)` keeps the shim off the debug CRT / _ITERATOR_DEBUG_LEVEL
    // path in debug profiles so the STL object layouts match on both sides.
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .debug(false)
        .file(libdev_root.join("shim/obsbot_shim.cpp"))
        .include(libdev_root.join("include"))
        .include(libdev_root.join("shim"))
        // C++ exceptions stay inside the shim (every entry point is
        // try/catch-wrapped); MSVC still needs unwind semantics enabled.
        .flag_if_supported("/EHsc")
        // dev.hpp contains UTF-8 comments; keep MSVC from guessing a codepage.
        .flag_if_supported("/utf-8")
        .compile("obsbot_shim");

    // The import library for libdev.dll. `cc` already emitted the link
    // directives for the static shim itself.
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=libdev");

    // Stage the runtime DLLs. Test binaries run from target/<profile>/deps and
    // the app exe from target/<profile>; Windows resolves DLLs adjacent to the
    // exe (and cargo/nextest additionally put the link-search dir above on
    // PATH, which covers stray layouts). OUT_DIR is
    // `target/<profile>/build/<crate>-<hash>/out`, so the profile dir is the
    // 4th ancestor.
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    let profile_dir = std::path::Path::new(&out_dir)
        .ancestors()
        .nth(3)
        .expect("OUT_DIR has at least 4 ancestors")
        .to_path_buf();
    for dll in ["libdev.dll", "w32-pthreads.dll"] {
        println!("cargo:rerun-if-changed=../../vendor/libdev/windows/win64-release/{dll}");
        let src = lib_dir.join(dll);
        for dst_dir in [profile_dir.clone(), profile_dir.join("deps")] {
            std::fs::create_dir_all(&dst_dir).expect("create DLL staging dir");
            stage_dll(&src, &dst_dir.join(dll));
        }
    }
}

/// Copy a runtime DLL into a staging dir, tolerating the Windows quirk that a
/// loaded DLL cannot be overwritten: when a same-size copy is already in place
/// (the vendored DLLs never change in place), or the copy fails but a previous
/// staging left a usable file, the build proceeds — a running app/test process
/// holding the DLL must not wedge every parallel build. A missing destination
/// still fails the build, because binaries would crash at load.
#[cfg(target_os = "windows")]
fn stage_dll(src: &std::path::Path, dst: &std::path::Path) {
    let same_len = matches!(
        (std::fs::metadata(src), std::fs::metadata(dst)),
        (Ok(s), Ok(d)) if s.len() == d.len()
    );
    if same_len {
        return;
    }
    if let Err(err) = std::fs::copy(src, dst) {
        assert!(
            dst.exists(),
            "Failed to copy {} to {}: {}",
            src.display(),
            dst.display(),
            err
        );
        println!(
            "cargo:warning=could not refresh {} ({}); keeping the existing staged copy",
            dst.display(),
            err
        );
    }
}
