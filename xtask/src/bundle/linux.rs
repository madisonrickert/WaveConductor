//! `cargo xtask bundle-linux` — build the release binary and assemble a
//! self-contained Linux distribution directory.
//!
//! ## Layout
//!
//! ```text
//! target/dist/linux-x86_64/WaveConductor/
//!   waveconductor        (release binary, mode 0o755)
//!   libLeapC.so.6        (vendored Leap runtime)
//!   assets/              (workspace assets/, recursive copy)
//!   RUN.txt              (launch notes)
//! ```
//!
//! The binary's baked `$ORIGIN` RUNPATH (see `.cargo/config.toml`) resolves
//! `libLeapC.so.6` from its own directory, so the folder is self-contained and
//! relocatable without `LD_LIBRARY_PATH`. Only `x86_64` is vendored. There is no
//! Linux equivalent of code-signing, so none is attempted (a follow-up could add
//! an `AppImage` / `.desktop` wrapper).

use std::path::Path;

use clap::Args as ClapArgs;

use super::common::{self, StageReport};

/// vendor/leapc subdirectory + runtime library for the sole supported target.
const TARGET_DIR: &str = "linux-x86_64";
const LEAP_LIB: &str = "libLeapC.so.6";

/// Launch notes dropped next to the binary. User-facing, so no em dashes.
const RUN_NOTES: &str = "WaveConductor (v5 alpha)\n\
    \n\
    Run:  ./waveconductor\n\
    \n\
    Requires a WebGPU-capable GPU (Vulkan 1.2+). Hand tracking is optional and\n\
    needs the Ultraleap tracking service running on this machine. This is an\n\
    unsigned pre-release build.\n";

/// Arguments for the bundle-linux subcommand.
#[derive(ClapArgs)]
pub struct Args {
    /// Skip `cargo build -p waveconductor --release` and use an existing binary.
    #[arg(long)]
    pub skip_build: bool,

    /// Emit machine-readable JSON instead of the human summary.
    #[arg(long)]
    pub json: bool,
}

/// Execute the bundle-linux subcommand.
pub fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let root = common::workspace_root();

    if !args.skip_build {
        common::build_release(&root)?;
    }

    // Only x86_64 is vendored; fail loudly rather than stage a broken folder.
    let arch = std::env::consts::ARCH;
    if arch != "x86_64" {
        return Err(format!(
            "bundle-linux: only x86_64 is vendored (host arch is '{arch}'); \
             add a vendor/leapc/linux-{arch}/ tree to support it"
        )
        .into());
    }

    let binary = root.join("target").join("release").join(common::BIN_NAME);
    if !binary.exists() {
        return Err(format!(
            "bundle-linux: release binary not found at {}; run without --skip-build",
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
/// Split out from [`run`] so tests can drive it with synthetic files without a
/// real release build or vendor tree. Idempotent: any prior staging directory is
/// removed first.
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
                "bundle-linux: cannot remove prior staging at {}: {e}",
                app_dir.display()
            )
        })?;
    }
    std::fs::create_dir_all(&app_dir)?;

    // Release binary, marked executable.
    let dst_bin = app_dir.join(common::BIN_NAME);
    std::fs::copy(binary, &dst_bin).map_err(|e| {
        format!(
            "bundle-linux: cannot copy binary {} -> {}: {e}",
            binary.display(),
            dst_bin.display()
        )
    })?;
    common::set_executable(&dst_bin)?;

    // Vendored Leap runtime sits next to the binary; `$ORIGIN` resolves it.
    common::copy_leap_lib(leap_lib, &app_dir.join(LEAP_LIB))?;

    // Recursive assets/ copy.
    let dst_assets = app_dir.join("assets");
    let asset_count = common::copy_dir_all(assets_src, &dst_assets).map_err(|e| {
        format!(
            "bundle-linux: cannot copy assets {} -> {}: {e}",
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
    })
}

/// Print the human or JSON summary for an assembled staging directory.
fn report_out(report: &StageReport, json: bool) {
    let dir = report.dir.display();
    if json {
        println!(
            "{{\"dir\":\"{dir}\",\"size_bytes\":{},\"asset_count\":{}}}",
            report.size_bytes, report.asset_count
        );
    } else {
        // MiB with one decimal via integer arithmetic (no lossy u64 as f64).
        let mib_whole = report.size_bytes / (1024 * 1024);
        let mib_frac = (report.size_bytes % (1024 * 1024)) * 10 / (1024 * 1024);
        println!("Linux staging assembled: {dir}");
        println!("  size          {mib_whole}.{mib_frac} MiB");
        println!("  asset files   {}", report.asset_count);
        println!();
        println!("Archive with:  tar czf WaveConductor-linux-x86_64.tar.gz -C target/dist/linux-x86_64 WaveConductor");
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
        let dir = std::env::temp_dir().join(format!("wc-bundle-linux-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn assemble_lays_out_binary_lib_assets_and_notes() {
        let tmp = unique_tmp();

        // Synthetic inputs.
        let binary = tmp.join("waveconductor");
        std::fs::write(&binary, b"ELF-ish binary bytes").expect("write fake binary");
        let leap = tmp.join("libLeapC.so.6");
        std::fs::write(&leap, b"leap runtime bytes").expect("write fake leap lib");
        let assets = tmp.join("assets");
        std::fs::create_dir_all(assets.join("shaders")).expect("mk assets");
        std::fs::write(assets.join("a.txt"), b"a").expect("asset a");
        std::fs::write(assets.join("shaders").join("b.wgsl"), b"b").expect("asset b");

        let staging_root = tmp.join("dist");
        let report = assemble(&binary, &leap, &assets, &staging_root).expect("assemble");

        let app = staging_root.join("WaveConductor");
        assert!(app.join("waveconductor").is_file(), "binary staged");
        assert!(app.join("libLeapC.so.6").is_file(), "leap lib staged");
        assert!(app.join("assets").join("a.txt").is_file(), "top asset");
        assert!(
            app.join("assets").join("shaders").join("b.wgsl").is_file(),
            "nested asset"
        );
        assert!(app.join("RUN.txt").is_file(), "run notes");
        assert_eq!(report.asset_count, 2, "two asset files copied");
        assert!(report.size_bytes > 0, "non-empty staging");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn assemble_is_idempotent() {
        let tmp = unique_tmp();
        let binary = tmp.join("waveconductor");
        std::fs::write(&binary, b"bin").expect("bin");
        let leap = tmp.join("libLeapC.so.6");
        std::fs::write(&leap, b"leap").expect("leap");
        let assets = tmp.join("assets");
        std::fs::create_dir_all(&assets).expect("assets");
        std::fs::write(assets.join("a.txt"), b"a").expect("a");
        let staging_root = tmp.join("dist");

        assemble(&binary, &leap, &assets, &staging_root).expect("first");
        // A stale file inside the prior staging must not survive a re-run.
        let stale = staging_root.join("WaveConductor").join("STALE");
        std::fs::write(&stale, b"x").expect("stale");
        assemble(&binary, &leap, &assets, &staging_root).expect("second");
        assert!(!stale.exists(), "prior staging cleared on re-run");

        std::fs::remove_dir_all(&tmp).ok();
    }
}
