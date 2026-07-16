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

// A build script signals failure by panicking — that is the sanctioned way to
// fail a build when a setup-time invariant (an env var Cargo guarantees, a
// vendored file that must exist) is violated. `expect`/`panic!` here are
// therefore correct, not the runtime foot-guns the workspace
// `expect_used`/`panic` lints guard against. These only surface under the
// `-D warnings` gate on Windows, where the `#[cfg(target_os = "windows")]`
// blocks below actually compile; scope the allow to this build-script
// compilation unit so runtime code keeps the strict lints.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "build scripts fail the build by panicking; setup-time invariants, not runtime paths"
)]

fn main() {
    println!("cargo:rerun-if-changed=../../vendor/leapc/windows-x86_64/LeapC.dll");

    // The vendored-LeapC *link search path* is emitted by `wc-core`'s build
    // script (the crate that owns the `leaprs` dependency), so it applies to the
    // app binary, wc-core's own test/example binaries, and every dependent at
    // once. This build script only handles the two app-binary-specific steps
    // below: staging `LeapC.dll` next to the produced `.exe` for runtime
    // adjacent-DLL discovery, and embedding the icon + version resource.
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
