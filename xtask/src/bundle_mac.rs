//! `cargo xtask bundle-mac` — build the release binary and assemble a
//! self-contained `WaveConductor.app` bundle.
//!
//! ## Bundle layout
//!
//! ```text
//! target/WaveConductor.app/
//! └── Contents/
//!     ├── MacOS/waveconductor         (release binary, mode 0o755)
//!     ├── MacOS/libLeapC.6.dylib      (vendored Leap SDK runtime)
//!     ├── MacOS/libLeapC.dylib        (unversioned alias, resolves @loader_path)
//!     ├── Resources/WaveConductor.icns (app icon, generated from the source PNG)
//!     ├── Resources/assets/           (workspace assets/, recursive copy)
//!     ├── Info.plist                  (generated XML property list)
//!     └── PkgInfo                     (APPL????)
//! ```
//!
//! ## App icon
//!
//! The Dock/Finder icon is generated at bundle time from
//! `assets/app-icons/icon.png` (the 1024×1024 source carried over from v4) using
//! the stock-macOS `sips` + `iconutil` toolchain — no committed `.icns` blob and
//! no extra crates. The PNG is the single source of truth; the `.icns` is a
//! build artifact. `Info.plist` points at it via `CFBundleIconFile`.
//!
//! ## Out of scope
//!
//! - Code-signing / notarization: needed for distribution off this machine;
//!   the local kiosk runs unsigned.

use std::path::{Path, PathBuf};

use clap::Args as ClapArgs;

/// Arguments for the bundle-mac subcommand.
#[derive(ClapArgs)]
pub struct Args {
    /// Skip `cargo build -p waveconductor --release` and use an existing binary.
    #[arg(long)]
    pub skip_build: bool,

    /// Emit machine-readable JSON instead of the human summary.
    #[arg(long)]
    pub json: bool,
}

/// macOS application bundle identifier.
const BUNDLE_ID: &str = "com.madisonrickert.waveconductor";

/// Bundle display + short name.
const BUNDLE_NAME: &str = "WaveConductor";

/// Binary name (matches `[[bin]]` in the waveconductor Cargo.toml).
const BUNDLE_EXE: &str = "waveconductor";

/// Minimum macOS version required to launch the app.
const MIN_OS: &str = "12.0";

/// Camera usage description shown in the macOS permission dialog.
///
/// MANDATORY: without this key macOS denies camera access and the `MediaPipe`
/// hand-tracking provider fails.
const CAMERA_USAGE: &str = "WaveConductor uses the camera for hand-gesture tracking.";

/// Crate (workspace) version, injected at compile time.
const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Execute the bundle-mac subcommand.
pub fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let root = workspace_root();
    let target = root.join("target");
    let app_dir = target.join("WaveConductor.app");
    let contents = app_dir.join("Contents");

    // 1. Build the release binary (unless caller opted out).
    if !args.skip_build {
        build_release(&root)?;
    }

    let release_bin = target.join("release").join(BUNDLE_EXE);
    if !release_bin.exists() {
        return Err(format!(
            "bundle-mac: release binary not found at {}; run without --skip-build",
            release_bin.display()
        )
        .into());
    }

    // 2. Assemble bundle (idempotent: remove any prior bundle first).
    if app_dir.exists() {
        std::fs::remove_dir_all(&app_dir).map_err(|e| {
            format!(
                "bundle-mac: cannot remove prior bundle at {}: {e}",
                app_dir.display()
            )
        })?;
    }

    let macos_dir = contents.join("MacOS");
    let resources_dir = contents.join("Resources");
    std::fs::create_dir_all(&macos_dir)?;
    std::fs::create_dir_all(&resources_dir)?;

    // 2a. Copy the release binary and mark it executable.
    let dst_bin = macos_dir.join(BUNDLE_EXE);
    std::fs::copy(&release_bin, &dst_bin).map_err(|e| {
        format!(
            "bundle-mac: cannot copy binary {} -> {}: {e}",
            release_bin.display(),
            dst_bin.display()
        )
    })?;
    set_executable(&dst_bin)?;

    // 2b. Copy the vendored Leap SDK dylib next to the binary so that the
    //     binary's `@loader_path/libLeapC.6.dylib` rpath resolves at launch.
    copy_leap_dylib(&root, &macos_dir)?;

    // 2c. Recursively copy the workspace assets/ tree into Resources/assets/.
    //     The runtime resolver `asset_root()` in wc-core/src/platform/assets.rs
    //     detects the `Contents/MacOS` exe dir and resolves to
    //     `Contents/Resources/assets` — so this destination is load-bearing.
    let src_assets = root.join("assets");
    let dst_assets = resources_dir.join("assets");
    let asset_count = copy_dir_all(&src_assets, &dst_assets).map_err(|e| {
        format!(
            "bundle-mac: cannot copy assets {} -> {}: {e}",
            src_assets.display(),
            dst_assets.display()
        )
    })?;

    // 2c-bis. Generate the app icon (.icns) into Resources/ from the source PNG.
    //     Returns the bare icon-file name to reference from Info.plist.
    let icon_file = build_icns(&root, &resources_dir)?;

    // 2d. Write Info.plist.
    let plist_path = contents.join("Info.plist");
    let plist = info_plist(
        BUNDLE_NAME,
        BUNDLE_ID,
        BUNDLE_EXE,
        CRATE_VERSION,
        CAMERA_USAGE,
        MIN_OS,
        &icon_file,
    );
    std::fs::write(&plist_path, plist.as_bytes())?;

    // 2e. Write PkgInfo (optional, conventional for macOS app bundles).
    std::fs::write(contents.join("PkgInfo"), b"APPL????")?;

    // 3. Compute bundle byte size for the report.
    let size_bytes = dir_size(&app_dir)?;

    // 4. Emit the report.
    let app_path = app_dir.display().to_string();
    if args.json {
        println!(
            "{{\"app_path\":\"{app_path}\",\"version\":\"{CRATE_VERSION}\",\"size_bytes\":{size_bytes},\"asset_count\":{asset_count}}}"
        );
    } else {
        println!("Bundle assembled: {app_path}");
        println!("  version       {CRATE_VERSION}");
        println!("  icon          {icon_file}");
        // Display size in MiB with one decimal place using integer arithmetic
        // to avoid a `u64 as f64` precision-loss cast.
        let mib_whole = size_bytes / (1024 * 1024);
        let mib_frac = (size_bytes % (1024 * 1024)) * 10 / (1024 * 1024);
        println!("  size          {mib_whole}.{mib_frac} MiB");
        println!("  asset files   {asset_count}");
        println!();
        println!("To launch:  open target/WaveConductor.app");
        println!("Note: an unsigned app requires right-click -> Open (or");
        println!("  xattr -dr com.apple.quarantine target/WaveConductor.app) the first time.");
        println!("Out of scope: code-signing/notarization.");
    }

    Ok(())
}

/// Generate a well-formed XML property list string for a macOS application bundle.
///
/// All keys required for a working kiosk application are included:
/// - Retina support (`NSHighResolutionCapable`)
/// - Camera permission string (`NSCameraUsageDescription`) — mandatory
///   for `MediaPipe` hand-tracking; without it macOS denies camera access
/// - `LSMinimumSystemVersion` floor
/// - App icon reference (`CFBundleIconFile`) — the bare icon-file name (with or
///   without the `.icns` extension) of the icon in `Contents/Resources/`
///
/// The returned string passes `plutil -lint` on macOS.
pub fn info_plist(
    name: &str,
    bundle_id: &str,
    exe: &str,
    version: &str,
    camera_usage: &str,
    min_os: &str,
    icon_file: &str,
) -> String {
    format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\"",
            " \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
            "<plist version=\"1.0\">\n",
            "<dict>\n",
            "\t<key>CFBundleName</key>\n",
            "\t<string>{name}</string>\n",
            "\t<key>CFBundleDisplayName</key>\n",
            "\t<string>{name}</string>\n",
            "\t<key>CFBundleIdentifier</key>\n",
            "\t<string>{id}</string>\n",
            "\t<key>CFBundleExecutable</key>\n",
            "\t<string>{exe}</string>\n",
            "\t<key>CFBundleIconFile</key>\n",
            "\t<string>{icon}</string>\n",
            "\t<key>CFBundlePackageType</key>\n",
            "\t<string>APPL</string>\n",
            "\t<key>CFBundleVersion</key>\n",
            "\t<string>{ver}</string>\n",
            "\t<key>CFBundleShortVersionString</key>\n",
            "\t<string>{ver}</string>\n",
            "\t<key>NSHighResolutionCapable</key>\n",
            "\t<true/>\n",
            "\t<key>NSCameraUsageDescription</key>\n",
            "\t<string>{cam}</string>\n",
            "\t<key>LSMinimumSystemVersion</key>\n",
            "\t<string>{min_os}</string>\n",
            "\t<key>LSApplicationCategoryType</key>\n",
            "\t<string>public.app-category.entertainment</string>\n",
            "</dict>\n",
            "</plist>\n",
        ),
        name = name,
        id = bundle_id,
        exe = exe,
        ver = version,
        cam = camera_usage,
        min_os = min_os,
        icon = icon_file,
    )
}

// ---- private helpers -------------------------------------------------------

/// Workspace root: parent of the xtask crate dir (`CARGO_MANIFEST_DIR`).
fn workspace_root() -> PathBuf {
    std::env::var("CARGO_MANIFEST_DIR")
        .ok()
        .and_then(|d| PathBuf::from(d).parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Shell out to `cargo build -p waveconductor --release`.
fn build_release(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let status = std::process::Command::new("cargo")
        .current_dir(root)
        .args(["build", "-p", "waveconductor", "--release"])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(
            format!("bundle-mac: `cargo build -p waveconductor --release` failed with {status}")
                .into(),
        )
    }
}

/// Generate the macOS `.icns` app icon into `resources_dir` from the source PNG
/// at `assets/app-icons/icon.png`, returning the bare icon-file name to write
/// into `Info.plist`'s `CFBundleIconFile` key.
///
/// Uses the stock-macOS toolchain only — `sips` to rasterize each required
/// size into a temporary `.iconset` directory, then `iconutil -c icns` to pack
/// them. No extra crates and no committed `.icns` blob: the 1024×1024 PNG is the
/// single source of truth and the `.icns` is a pure build artifact. The scratch
/// `.iconset` is written under the `.app` (not the source tree) and removed
/// afterward, so a re-run leaves nothing behind.
///
/// macOS-only, which is the bundler's sole target. Returns a clear error if the
/// source PNG is missing or either tool fails.
fn build_icns(root: &Path, resources_dir: &Path) -> Result<String, Box<dyn std::error::Error>> {
    // (pixel size, file name) per Apple's iconset naming convention. Each `@2x`
    // entry is the next size up so Retina displays get the denser raster.
    const VARIANTS: &[(u32, &str)] = &[
        (16, "icon_16x16.png"),
        (32, "icon_16x16@2x.png"),
        (32, "icon_32x32.png"),
        (64, "icon_32x32@2x.png"),
        (128, "icon_128x128.png"),
        (256, "icon_128x128@2x.png"),
        (256, "icon_256x256.png"),
        (512, "icon_256x256@2x.png"),
        (512, "icon_512x512.png"),
        (1024, "icon_512x512@2x.png"),
    ];

    let src_png = root.join("assets").join("app-icons").join("icon.png");
    if !src_png.exists() {
        return Err(format!(
            "bundle-mac: app icon source not found at {}; expected the 1024x1024 PNG",
            src_png.display()
        )
        .into());
    }

    // Scratch iconset lives inside the .app dir (resources_dir is
    // `…/Contents/Resources`), never in the source tree.
    let iconset = resources_dir.join("WaveConductor.iconset");
    if iconset.exists() {
        std::fs::remove_dir_all(&iconset)?;
    }
    std::fs::create_dir_all(&iconset)?;

    for (size, name) in VARIANTS {
        sips_resize(&src_png, *size, &iconset.join(name))?;
    }

    let icon_file = format!("{BUNDLE_NAME}.icns");
    let icns_path = resources_dir.join(&icon_file);
    let status = std::process::Command::new("iconutil")
        .args(["-c", "icns"])
        .arg(&iconset)
        .arg("-o")
        .arg(&icns_path)
        .status()
        .map_err(|e| format!("bundle-mac: cannot run `iconutil` (stock macOS tool): {e}"))?;
    if !status.success() {
        return Err(format!("bundle-mac: `iconutil -c icns` failed with {status}").into());
    }

    // The iconset has served its purpose; leave only the packed .icns behind.
    std::fs::remove_dir_all(&iconset)?;

    Ok(icon_file)
}

/// Rasterize `src` to a square `size`×`size` PNG at `out` via `sips`.
fn sips_resize(src: &Path, size: u32, out: &Path) -> Result<(), Box<dyn std::error::Error>> {
    // `sips -z <height> <width>` — square, so height == width == size.
    let status = std::process::Command::new("sips")
        .arg("-z")
        .arg(size.to_string())
        .arg(size.to_string())
        .arg(src)
        .arg("--out")
        .arg(out)
        .status()
        .map_err(|e| format!("bundle-mac: cannot run `sips` (stock macOS tool): {e}"))?;
    if !status.success() {
        return Err(format!("bundle-mac: `sips` resize to {size}px failed with {status}").into());
    }
    Ok(())
}

/// Recursively copy `src` directory tree into `dst`, creating `dst` as needed.
///
/// Returns the number of regular files copied. Symbolic links are skipped;
/// the workspace `assets/` tree contains no symlinks.
fn copy_dir_all(src: &Path, dst: &Path) -> Result<u64, Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dst)?;
    let mut count = 0_u64;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            count += copy_dir_all(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            std::fs::copy(&src_path, &dst_path)?;
            count += 1;
        }
        // Symlinks are intentionally skipped (not expected in assets/).
    }
    Ok(count)
}

/// Map a Rust target architecture name to the vendor subdirectory that holds
/// the prebuilt Leap SDK libraries for that architecture.
///
/// Returns `None` for architectures that have no vendored Leap SDK copy.
/// Covered by unit tests in the `tests` module.
pub fn leap_vendor_subdir(arch: &str) -> Option<&'static str> {
    match arch {
        "aarch64" => Some("macos-aarch64"),
        "x86_64" => Some("macos-x86_64"),
        _ => None,
    }
}

/// Copy the vendored Leap SDK dylib files into `dst_dir` (normally
/// `Contents/MacOS/`) so that the binary's `@loader_path/libLeapC.6.dylib`
/// rpath entry resolves at launch.
///
/// Both the versioned file (`libLeapC.6.dylib`) and the unversioned alias
/// (`libLeapC.dylib`) are copied as regular files; the alias is a symlink in
/// the vendor tree but we copy the target bytes so the bundle is self-contained
/// without symlink support being required by every tool that touches the .app.
///
/// Returns a clear error if the host architecture is unsupported or the source
/// dylib is absent.
fn copy_leap_dylib(root: &Path, dst_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let arch = std::env::consts::ARCH;
    let subdir = leap_vendor_subdir(arch).ok_or_else(|| {
        format!(
            "bundle-mac: no vendored Leap SDK for architecture '{arch}'; \
             add a vendor/leapc/macos-{arch}/ directory with libLeapC.6.dylib"
        )
    })?;
    let vendor_dir = root.join("vendor").join("leapc").join(subdir);

    for name in &["libLeapC.6.dylib", "libLeapC.dylib"] {
        let src = vendor_dir.join(name);
        if !src.exists() {
            return Err(format!(
                "bundle-mac: vendored dylib not found at {}; \
                 re-run `git lfs pull` or restore the vendor tree",
                src.display()
            )
            .into());
        }
        // Read via `read` (not symlink-following `copy`) so that if `libLeapC.dylib`
        // is a symlink we copy the actual bytes rather than creating another symlink.
        let bytes = std::fs::read(&src)
            .map_err(|e| format!("bundle-mac: cannot read {}: {e}", src.display()))?;
        let dst = dst_dir.join(name);
        std::fs::write(&dst, &bytes)
            .map_err(|e| format!("bundle-mac: cannot write {}: {e}", dst.display()))?;
    }

    Ok(())
}

/// Set the executable bit (Unix mode 0o755) on a file.
#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

/// No-op on non-Unix platforms (xtask is a dev-only tool; macOS is the target).
#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

/// Walk `dir` recursively and return the sum of sizes of all regular files.
fn dir_size(dir: &Path) -> Result<u64, Box<dyn std::error::Error>> {
    let mut total = 0_u64;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            total += dir_size(&entry.path())?;
        } else if file_type.is_file() {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "expect is appropriate in test scaffolding"
)]
mod tests {
    use super::*;

    // ---- leap_vendor_subdir ---------------------------------------------------

    #[test]
    fn leap_vendor_subdir_aarch64() {
        assert_eq!(
            leap_vendor_subdir("aarch64"),
            Some("macos-aarch64"),
            "aarch64 must map to macos-aarch64"
        );
    }

    #[test]
    fn leap_vendor_subdir_x86_64() {
        assert_eq!(
            leap_vendor_subdir("x86_64"),
            Some("macos-x86_64"),
            "x86_64 must map to macos-x86_64"
        );
    }

    #[test]
    fn leap_vendor_subdir_unsupported_returns_none() {
        assert_eq!(
            leap_vendor_subdir("riscv64"),
            None,
            "unsupported arch must return None"
        );
        assert_eq!(
            leap_vendor_subdir("wasm32"),
            None,
            "wasm32 must return None"
        );
        assert_eq!(
            leap_vendor_subdir(""),
            None,
            "empty string must return None"
        );
    }

    // ---- plist ---------------------------------------------------------------

    fn sample_plist() -> String {
        info_plist(
            "WaveConductor",
            "com.madisonrickert.waveconductor",
            "waveconductor",
            "5.0.0-dev",
            "WaveConductor uses the camera for hand-gesture tracking.",
            "12.0",
            "WaveConductor.icns",
        )
    }

    #[test]
    fn plist_starts_with_xml_declaration() {
        let p = sample_plist();
        assert!(
            p.starts_with("<?xml version=\"1.0\""),
            "plist must start with XML declaration, got: {p:.80}"
        );
    }

    #[test]
    fn plist_contains_doctype() {
        let p = sample_plist();
        assert!(
            p.contains("<!DOCTYPE plist"),
            "plist must contain DOCTYPE declaration"
        );
    }

    #[test]
    fn plist_contains_bundle_identifier() {
        let p = sample_plist();
        assert!(
            p.contains("<string>com.madisonrickert.waveconductor</string>"),
            "plist must contain bundle identifier"
        );
    }

    #[test]
    fn plist_contains_executable_name() {
        let p = sample_plist();
        assert!(
            p.contains("<string>waveconductor</string>"),
            "plist must contain the executable name"
        );
    }

    #[test]
    fn plist_contains_nscamera_key() {
        let p = sample_plist();
        assert!(
            p.contains("<key>NSCameraUsageDescription</key>"),
            "plist must contain NSCameraUsageDescription key"
        );
    }

    #[test]
    fn plist_contains_nscamera_value() {
        let p = sample_plist();
        assert!(
            p.contains("WaveConductor uses the camera for hand-gesture tracking."),
            "plist must contain the camera usage string"
        );
    }

    #[test]
    fn plist_contains_high_resolution_key() {
        let p = sample_plist();
        assert!(
            p.contains("<key>NSHighResolutionCapable</key>"),
            "plist must contain NSHighResolutionCapable key"
        );
    }

    #[test]
    fn plist_high_resolution_is_true() {
        let p = sample_plist();
        // <true/> must appear after the NSHighResolutionCapable key.
        let key_pos = p
            .find("<key>NSHighResolutionCapable</key>")
            .expect("key must be present");
        let true_pos = p.find("<true/>").expect("<true/> must be present");
        assert!(
            true_pos > key_pos,
            "<true/> must follow the NSHighResolutionCapable key"
        );
    }

    #[test]
    fn plist_contains_icon_file() {
        let p = sample_plist();
        assert!(
            p.contains("<key>CFBundleIconFile</key>"),
            "plist must contain CFBundleIconFile key"
        );
        assert!(
            p.contains("<string>WaveConductor.icns</string>"),
            "plist must reference the generated .icns file name"
        );
    }

    #[test]
    fn plist_contains_min_os() {
        let p = sample_plist();
        assert!(
            p.contains("<string>12.0</string>"),
            "plist must contain LSMinimumSystemVersion value"
        );
    }
}
