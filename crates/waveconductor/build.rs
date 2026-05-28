//! Build-time copy of the vendored LeapC.dll next to the produced .exe on
//! Windows. macOS and Linux use rpath baked into the binary via
//! `.cargo/config.toml`'s `rustflags`; Windows uses adjacent-file discovery
//! at runtime, so the DLL has to sit in the same directory as the .exe.
//!
//! No-op on non-Windows targets.

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
}
