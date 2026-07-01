//! Shared staging primitives for the `bundle-*` subcommands.
//!
//! Every platform bundler (`mac`, `linux`, `windows`) performs the same core
//! steps: build the release binary, copy it into a staging layout, drop the
//! vendored `LeapC` runtime for the target next to it, and recursively copy the
//! workspace `assets/` tree. The file-copy, directory-size, release-build, and
//! executable-bit primitives common to all three live here; the platform layout
//! (macOS `.app` + `Info.plist` + `.icns` vs. the flat Linux/Windows staging
//! directory) lives in each sibling module.
//!
//! Code-signing / notarization is out of scope on every platform (the local
//! kiosk runs unsigned; distribution signing is a separate follow-up).

use std::path::{Path, PathBuf};

/// Binary name (matches `[[bin]]` in the `waveconductor` crate). macOS and Linux
/// use it verbatim; Windows appends `.exe` (see [`windows`](super::windows)).
pub const BIN_NAME: &str = "waveconductor";

/// Human-facing application name used for bundle / staging directory names.
pub const APP_NAME: &str = "WaveConductor";

/// Result of assembling a flat (Linux/Windows) staging directory.
///
/// The macOS bundler reports its own richer summary inline (icon, plist); the
/// flat bundlers share this shape.
pub struct StageReport {
    /// Absolute path to the assembled staging directory.
    pub dir: PathBuf,
    /// Total size of the staging directory in bytes.
    pub size_bytes: u64,
    /// Number of regular files copied from `assets/`.
    pub asset_count: u64,
}

/// Workspace root: parent of the xtask crate dir (`CARGO_MANIFEST_DIR`).
pub fn workspace_root() -> PathBuf {
    std::env::var("CARGO_MANIFEST_DIR")
        .ok()
        .and_then(|d| PathBuf::from(d).parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Shell out to `cargo build -p waveconductor --release` from `root`.
///
/// Shared by every bundler so a `--skip-build` run and a fresh build produce
/// the same binary path (`target/release/`).
pub fn build_release(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let status = std::process::Command::new("cargo")
        .current_dir(root)
        .args(["build", "-p", "waveconductor", "--release"])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("bundle: `cargo build -p waveconductor --release` failed with {status}").into())
    }
}

/// Recursively copy `src` directory tree into `dst`, creating `dst` as needed.
///
/// Returns the number of regular files copied. Symbolic links are skipped; the
/// workspace `assets/` tree contains no symlinks.
pub fn copy_dir_all(src: &Path, dst: &Path) -> Result<u64, Box<dyn std::error::Error>> {
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

/// Copy the vendored `LeapC` runtime library at `src` to `dst`, following the
/// source if it is a symlink so the destination is a real file.
///
/// The vendor tree can carry the unversioned name as a symlink to the versioned
/// one; reading the bytes (rather than a symlink-preserving `copy`) keeps the
/// staged folder self-contained without requiring symlink support from every
/// tool that unpacks the archive.
pub fn copy_leap_lib(src: &Path, dst: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if !src.exists() {
        return Err(format!(
            "bundle: vendored LeapC library not found at {}; \
             restore the vendor/leapc tree",
            src.display()
        )
        .into());
    }
    let bytes =
        std::fs::read(src).map_err(|e| format!("bundle: cannot read {}: {e}", src.display()))?;
    std::fs::write(dst, &bytes)
        .map_err(|e| format!("bundle: cannot write {}: {e}", dst.display()))?;
    Ok(())
}

/// Walk `dir` recursively and return the sum of sizes of all regular files.
pub fn dir_size(dir: &Path) -> Result<u64, Box<dyn std::error::Error>> {
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

/// Set the executable bit (Unix mode 0o755) on a file.
#[cfg(unix)]
pub fn set_executable(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

/// No-op on non-Unix platforms (Windows uses the `.exe` extension, not a mode bit).
#[cfg(not(unix))]
pub fn set_executable(_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}
