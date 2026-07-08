//! `cargo xtask bundle-windows` — build the release binary and assemble a
//! self-contained Windows distribution directory.
//!
//! ## Layout
//!
//! ```text
//! target/dist/windows-x86_64/WaveConductor/
//!   waveconductor.exe    (release binary)
//!   LeapC.dll            (vendored Leap runtime; loaded from the .exe directory)
//!   assets/              (workspace assets/, recursive copy)
//!   RUN.txt              (launch notes)
//! ```
//!
//! Windows resolves a dependent DLL from the directory of the loading `.exe`, so
//! `LeapC.dll` sits next to `waveconductor.exe` (the same adjacent-file scheme
//! `crates/waveconductor/build.rs` uses in the plain `target/release` layout).
//! Only `x86_64` is vendored. The binary is unsigned; `SmartScreen` warns on first
//! launch. This subcommand is meant to run on a Windows runner (that is where a
//! Windows release binary is produced); the staging logic itself is portable and
//! is unit-tested on any host.

use std::path::Path;

use clap::Args as ClapArgs;

use super::common::{self, StageReport};

/// vendor/leapc subdirectory + runtime library for the sole supported target.
const TARGET_DIR: &str = "windows-x86_64";
const LEAP_LIB: &str = "LeapC.dll";

/// Launch notes dropped next to the binary. User-facing, so no em dashes.
const RUN_NOTES: &str = "WaveConductor (v5 alpha)\r\n\
    \r\n\
    Run:  waveconductor.exe\r\n\
    \r\n\
    Requires a WebGPU-capable GPU (DirectX 12). Hand tracking is optional; the\r\n\
    Ultraleap Gemini tracking service is the recommended input on Windows. This\r\n\
    is an unsigned pre-release build, so SmartScreen may warn on first launch\r\n\
    (click More info, then Run anyway).\r\n\
    \r\n\
    Requires the Microsoft Visual C++ 2015-2022 x64 Redistributable (install it\r\n\
    from Microsoft if the app fails to start). The MSI installer bundles it\r\n\
    automatically.\r\n";

/// Arguments for the bundle-windows subcommand.
#[derive(ClapArgs)]
pub struct Args {
    /// Skip `cargo build -p waveconductor --release` and use an existing binary.
    #[arg(long)]
    pub skip_build: bool,

    /// Emit machine-readable JSON instead of the human summary.
    #[arg(long)]
    pub json: bool,
}

/// The Windows release binary file name (`waveconductor.exe`).
fn exe_name() -> String {
    format!("{}.exe", common::BIN_NAME)
}

/// Execute the bundle-windows subcommand.
pub fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let root = common::workspace_root();

    if !args.skip_build {
        common::build_release(&root)?;
    }

    let arch = std::env::consts::ARCH;
    if arch != "x86_64" {
        return Err(format!(
            "bundle-windows: only x86_64 is vendored (host arch is '{arch}'); \
             add a vendor/leapc/windows-{arch}/ tree to support it"
        )
        .into());
    }

    let binary = root.join("target").join("release").join(exe_name());
    if !binary.exists() {
        return Err(format!(
            "bundle-windows: release binary not found at {}; run without --skip-build",
            binary.display()
        )
        .into());
    }
    let leap_lib = root
        .join("vendor")
        .join("leapc")
        .join(TARGET_DIR)
        .join(LEAP_LIB);
    let assets = root.join("assets");
    let staging_root = root.join("target").join("dist").join(TARGET_DIR);

    let report = assemble(&binary, &leap_lib, &assets, &staging_root)?;
    report_out(&report, args.json);
    Ok(())
}

/// Assemble the self-contained staging directory from explicit inputs.
///
/// Split out from [`run`] so tests can drive it with synthetic files on any host
/// (the copies are portable even though a real Windows binary is only produced on
/// a Windows runner). Idempotent: any prior staging directory is removed first.
fn assemble(
    binary: &Path,
    leap_lib: &Path,
    assets_src: &Path,
    staging_root: &Path,
) -> Result<StageReport, Box<dyn std::error::Error>> {
    let app_dir = staging_root.join(common::APP_NAME);
    if app_dir.exists() {
        std::fs::remove_dir_all(&app_dir).map_err(|e| {
            format!(
                "bundle-windows: cannot remove prior staging at {}: {e}",
                app_dir.display()
            )
        })?;
    }
    std::fs::create_dir_all(&app_dir)?;

    // Release binary (`.exe`; Windows needs no executable mode bit).
    let dst_bin = app_dir.join(exe_name());
    std::fs::copy(binary, &dst_bin).map_err(|e| {
        format!(
            "bundle-windows: cannot copy binary {} -> {}: {e}",
            binary.display(),
            dst_bin.display()
        )
    })?;

    // Vendored Leap runtime next to the .exe (adjacent-DLL resolution).
    common::copy_leap_lib(leap_lib, &app_dir.join(LEAP_LIB))?;

    // Stage the ONNX Runtime DirectML DLLs that ORT's build script drops next to
    // the release binary (present only when `hand-tracking-mediapipe` is compiled
    // on Windows). Matched by known name so the exact provider-shared filename —
    // which varies by ORT build — is tolerated. Best-effort per file; the report
    // lists what was staged so CI can assert coverage. `LeapC.dll` is handled
    // above and deliberately excluded here.
    let bin_dir = binary.parent().unwrap_or_else(|| Path::new("."));
    let mut runtime_dlls = Vec::new();
    let entries = std::fs::read_dir(bin_dir).map_err(|e| {
        format!(
            "bundle-windows: cannot read binary dir {}: {e}",
            bin_dir.display()
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("bundle-windows: cannot read dir entry: {e}"))?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let lower = name.to_ascii_lowercase();
        let is_ort = lower.starts_with("onnxruntime")
            && Path::new(&lower).extension().is_some_and(|e| e == "dll");
        let is_directml = lower == "directml.dll";
        if is_ort || is_directml {
            std::fs::copy(entry.path(), app_dir.join(file_name.as_os_str()))
                .map_err(|e| format!("bundle-windows: cannot stage runtime dll {name}: {e}"))?;
            runtime_dlls.push(name.into_owned());
        }
    }
    runtime_dlls.sort();

    // Recursive assets/ copy.
    let dst_assets = app_dir.join("assets");
    let asset_count = common::copy_dir_all(assets_src, &dst_assets).map_err(|e| {
        format!(
            "bundle-windows: cannot copy assets {} -> {}: {e}",
            assets_src.display(),
            dst_assets.display()
        )
    })?;

    std::fs::write(app_dir.join("RUN.txt"), RUN_NOTES.as_bytes())?;

    let size_bytes = common::dir_size(&app_dir)?;
    Ok(StageReport {
        dir: app_dir,
        size_bytes,
        asset_count,
        runtime_dlls,
    })
}

/// Print the human or JSON summary for an assembled staging directory.
fn report_out(report: &StageReport, json: bool) {
    let dir = report.dir.display();
    if json {
        let dlls = report
            .runtime_dlls
            .iter()
            .map(|d| format!("\"{d}\""))
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "{{\"dir\":\"{dir}\",\"size_bytes\":{},\"asset_count\":{},\"runtime_dlls\":[{dlls}]}}",
            report.size_bytes, report.asset_count
        );
    } else {
        let mib_whole = report.size_bytes / (1024 * 1024);
        let mib_frac = (report.size_bytes % (1024 * 1024)) * 10 / (1024 * 1024);
        println!("Windows staging assembled: {dir}");
        println!("  size          {mib_whole}.{mib_frac} MiB");
        println!("  asset files   {}", report.asset_count);
        println!("  runtime dlls  {}", report.runtime_dlls.join(", "));
        println!();
        println!("Archive with (PowerShell):  Compress-Archive -Path target/dist/windows-x86_64/WaveConductor/* -DestinationPath WaveConductor-windows-x86_64.zip");
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "expect is appropriate in test scaffolding"
)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Dependency-free unique scratch directory under the system temp dir.
    fn unique_tmp() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("wc-bundle-windows-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn exe_name_appends_extension() {
        assert_eq!(exe_name(), "waveconductor.exe");
    }

    #[test]
    fn assemble_lays_out_exe_dll_assets_and_notes() {
        let tmp = unique_tmp();

        let binary = tmp.join("waveconductor.exe");
        std::fs::write(&binary, b"PE-ish binary bytes").expect("write fake exe");
        let leap = tmp.join("LeapC.dll");
        std::fs::write(&leap, b"leap dll bytes").expect("write fake dll");
        let assets = tmp.join("assets");
        std::fs::create_dir_all(assets.join("shaders")).expect("mk assets");
        std::fs::write(assets.join("a.txt"), b"a").expect("asset a");
        std::fs::write(assets.join("shaders").join("b.wgsl"), b"b").expect("asset b");

        let staging_root = tmp.join("dist");
        let report = assemble(&binary, &leap, &assets, &staging_root).expect("assemble");

        let app = staging_root.join("WaveConductor");
        assert!(app.join("waveconductor.exe").is_file(), "exe staged");
        assert!(app.join("LeapC.dll").is_file(), "dll staged");
        assert!(app.join("assets").join("a.txt").is_file(), "top asset");
        assert!(
            app.join("assets").join("shaders").join("b.wgsl").is_file(),
            "nested asset"
        );
        assert!(app.join("RUN.txt").is_file(), "run notes");
        assert_eq!(report.asset_count, 2, "two asset files copied");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn assemble_stages_ort_dlls_from_binary_dir() {
        let tmp = unique_tmp();

        let binary = tmp.join("waveconductor.exe");
        std::fs::write(&binary, b"PE-ish binary bytes").expect("write fake exe");
        // ORT drops these next to the release binary; DirectML.dll casing varies.
        std::fs::write(tmp.join("onnxruntime.dll"), b"ort").expect("ort dll");
        std::fs::write(tmp.join("onnxruntime_providers_shared.dll"), b"ort2").expect("ort shared");
        std::fs::write(tmp.join("DirectML.dll"), b"dml").expect("dml dll");
        // Mixed-case ORT DLL to assert the `is_ort` case-folding, not just inspect it.
        std::fs::write(tmp.join("OnnxRuntime_Providers_Cuda.DLL"), b"ort3").expect("ort cuda dll");
        // An unrelated DLL that must NOT be staged.
        std::fs::write(tmp.join("random.dll"), b"nope").expect("random dll");
        let leap = tmp.join("LeapC.dll");
        std::fs::write(&leap, b"leap dll bytes").expect("write fake dll");
        let assets = tmp.join("assets");
        std::fs::create_dir_all(&assets).expect("mk assets");
        std::fs::write(assets.join("a.txt"), b"a").expect("asset a");

        let staging_root = tmp.join("dist");
        let report = assemble(&binary, &leap, &assets, &staging_root).expect("assemble");

        let app = staging_root.join("WaveConductor");
        assert!(app.join("onnxruntime.dll").is_file(), "ort staged");
        assert!(
            app.join("onnxruntime_providers_shared.dll").is_file(),
            "ort shared staged"
        );
        assert!(app.join("DirectML.dll").is_file(), "directml staged");
        assert!(
            app.join("OnnxRuntime_Providers_Cuda.DLL").is_file(),
            "mixed-case ort dll staged"
        );
        assert!(!app.join("random.dll").exists(), "unrelated dll not staged");
        assert_eq!(
            report.runtime_dlls,
            vec![
                "DirectML.dll".to_string(),
                "OnnxRuntime_Providers_Cuda.DLL".to_string(),
                "onnxruntime.dll".to_string(),
                "onnxruntime_providers_shared.dll".to_string(),
            ],
            "sorted staged dll list"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }
}
