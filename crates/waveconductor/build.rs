//! Build-time copy of the vendored LeapC.dll next to the produced .exe on
//! Windows. macOS and Linux use rpath baked into the binary via
//! `.cargo/config.toml`'s `rustflags`; Windows uses adjacent-file discovery
//! at runtime, so the DLL has to sit in the same directory as the .exe.
//!
//! Also re-asserts the `objc_exception` static-lib link on macOS when the
//! webcam (`hand-tracking-mediapipe`) feature is active — see
//! [`relink_objc_exception_for_dynamic_linking`].
//!
//! No-op on non-Windows / non-macOS targets.

fn main() {
    println!("cargo:rerun-if-changed=../../vendor/leapc/windows-x86_64/LeapC.dll");

    #[cfg(target_os = "windows")]
    {
        let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR set by cargo");
        // OUT_DIR is `target/<profile>/build/<crate>-<hash>/out`. The target
        // dir is four ancestors up.
        let target_dir = std::path::Path::new(&out_dir)
            .ancestors()
            .nth(3)
            .expect("OUT_DIR has at least 4 ancestors")
            .to_path_buf();

        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../vendor/leapc/windows-x86_64/LeapC.dll");

        let dst = target_dir.join("LeapC.dll");

        std::fs::copy(&src, &dst).unwrap_or_else(|err| {
            panic!(
                "Failed to copy LeapC.dll from {} to {}: {}",
                src.display(),
                dst.display(),
                err
            );
        });
    }

    relink_objc_exception_for_dynamic_linking();
}

/// Re-assert the `objc_exception` static library on the final binary link.
///
/// nokhwa's macOS camera backend uses the old `objc` crate, whose
/// `objc_exception` shim (a `links = "objc_exception"` crate) provides the C
/// symbol `_RustObjCExceptionTryCatch`. `bevy/dynamic_linking` (what
/// `cargo rund` uses) drops that crate's `cargo:rustc-link-lib` directive from
/// the final binary link — leaving the symbol undefined and the link failing —
/// while keeping only the `-L` search path. The plain (static) build links it
/// fine. We `-force_load` the archive from the binary's own build script so the
/// symbol resolves regardless of `dynamic_linking` and link order.
///
/// In the plain build the archive is also linked normally; ld dedups, so this
/// is harmless. Gated to macOS + the webcam feature, so it is a no-op everywhere
/// else (the lib only exists once nokhwa is built).
fn relink_objc_exception_for_dynamic_linking() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let webcam_feature = std::env::var_os("CARGO_FEATURE_HAND_TRACKING_MEDIAPIPE").is_some();
    if target_os != "macos" || !webcam_feature {
        return;
    }

    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        return;
    };
    // OUT_DIR = target/<profile>/build/<crate>-<hash>/out → the shared build
    // dir (holding every crate's build output) is two ancestors up.
    let Some(build_dir) = std::path::Path::new(&out_dir).ancestors().nth(2) else {
        return;
    };

    // objc_exception is a transitive dep built before this script runs; its
    // hash is unstable, so locate its compiled archive by prefix.
    let Ok(entries) = std::fs::read_dir(build_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let is_objc_exception = dir
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("objc_exception-"));
        if !is_objc_exception {
            continue;
        }
        // The crate compiles its C shim into `libexception.a` (lib name
        // "exception"), not `libobjc_exception.a`.
        let lib = dir.join("out/libexception.a");
        if lib.exists() {
            // A plain `-lobjc_exception` is placed early on the binary link and,
            // under `-dead_strip`, ld won't pull the archive member that defines
            // `_RustObjCExceptionTryCatch` because nothing references it *yet* at
            // that point. `-force_load` loads the archive's members
            // unconditionally, order-independently. In the plain (static) build
            // the archive is also linked normally; ld dedups the duplicate.
            println!(
                "cargo:rustc-link-arg-bins=-Wl,-force_load,{}",
                lib.display()
            );
            return;
        }
    }
}
