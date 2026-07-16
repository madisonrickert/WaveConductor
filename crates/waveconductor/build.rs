//! Windows-only build-time steps: copying the vendored LeapC.dll next to the
//! produced .exe, and embedding the app icon + version resource.
//!
//! LeapC.dll: macOS and Linux use rpath baked into the binary via
//! `.cargo/config.toml`'s `rustflags`; Windows uses adjacent-file discovery
//! at runtime, so the DLL has to sit in the same directory as the .exe.
//!
//! Icon + version resource: embedded via `winresource` so the installed exe
//! shows a real icon in Explorer, the Start Menu, and Add/Remove Programs.
//!
//! No-op on non-Windows targets.

fn main() {
    println!("cargo:rerun-if-changed=../../vendor/leapc/windows-x86_64/LeapC.dll");

    // Point the linker at the vendored LeapC import library for this host.
    //
    // `leaprs`'s build script emits `-lLeapC` and a search path derived from
    // `LEAPSDK_LIB_PATH`, but it resolves that path *relative to its own crate
    // directory* in the registry, and the workspace `.cargo/config.toml` sets
    // the var (non-forced) to the macOS vendor dir as the primary-platform
    // default. On a fresh Windows checkout that default points at a directory
    // with no `LeapC.lib`, so the final binary fails to link with unresolved
    // `Leap*` externals. Emitting an absolute, host-correct search path here —
    // built from `CARGO_MANIFEST_DIR`, so it is independent of where the repo is
    // cloned — makes `cargo build` link out of the box with no manual env setup.
    // macOS keeps using the existing config default + `leaprs` path unchanged.
    #[cfg(target_os = "windows")]
    {
        let leapc_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../vendor/leapc/windows-x86_64");
        println!("cargo:rustc-link-search=native={}", leapc_dir.display());
        println!("cargo:rustc-link-lib=dylib=LeapC");
    }

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

    // Embed the app icon + version metadata into the Windows exe.
    #[cfg(target_os = "windows")]
    {
        println!("cargo:rerun-if-changed=assets/icon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "WaveConductor");
        res.set("FileDescription", "WaveConductor");
        res.set("CompanyName", "Madison Rickert");
        res.compile()
            .expect("embed Windows resources (icon + version) via winresource");
    }
}
