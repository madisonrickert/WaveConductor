//! Runtime asset-root resolver.
//!
//! [`asset_root`] locates the `assets/` directory at runtime across dev,
//! release, and macOS `.app` bundle deployments. Uses a five-step priority
//! order; each candidate is existence-checked so missing candidates fall
//! through gracefully.

use std::path::{Path, PathBuf};

/// Return the root of the `assets/` directory, resolving in priority order:
///
/// 1. `WAVECONDUCTOR_ASSET_ROOT` environment variable — trusted as-is (no
///    existence check), allows deployment / CI path overrides.
/// 2. macOS `.app` bundle: when the binary lives inside
///    `<App>.app/Contents/MacOS/`, check `../Resources/assets`.
/// 3. `assets/` directory placed next to the running binary.
/// 4. *(debug builds only)* Workspace `assets/` via `CARGO_MANIFEST_DIR`,
///    so `cargo rund` / `cargo run` from the repo root work without staging
///    assets. This branch is compiled **out** of release builds so the
///    developer's home-directory path is never baked into a shipped binary.
/// 5. Absolute `current_dir()/assets` fallback — correct for
///    `cargo run --release` launched from the workspace root, and absolute so
///    Bevy's `FileAssetReader` resolves shaders even when the binary's working
///    directory differs from where it was launched.
pub fn asset_root() -> PathBuf {
    // 1. Explicit override — trusted without an existence check.
    if let Some(p) = std::env::var_os("WAVECONDUCTOR_ASSET_ROOT") {
        return PathBuf::from(p);
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if let Some(resolved) = resolve_from_exe_dir(dir) {
                return resolved;
            }
        }
    }

    // 4. Dev tree only: workspace assets relative to this crate's manifest dir.
    //    The `cfg(debug_assertions)` gate ensures the compile-time home-directory
    //    path is never baked into a release binary (`cargo xtask check-secrets`
    //    and AGENTS.md both forbid home paths in the shipped binary; the release
    //    profile strips debug-assertions).
    #[cfg(debug_assertions)]
    {
        let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets");
        if dev.is_dir() {
            return dev;
        }
    }

    // 5. Absolute CWD fallback so Bevy's FileAssetReader can locate shaders when
    //    `cargo run --release` is launched from the workspace root.
    std::env::current_dir()
        .map_or_else(|_| PathBuf::from("assets"), |d| d.join("assets"))
}

/// Inspect the directory holding the running binary and return the first
/// existing `assets/` location:
///
/// - If `dir` ends with `Contents/MacOS` (macOS `.app` bundle layout), checks
///   `../Resources/assets` first (step 2).
/// - Then checks `<dir>/assets` (step 3).
///
/// Returns `None` when neither candidate exists. Factored out as a pure
/// function so unit tests can call it with a fabricated directory path rather
/// than depending on `std::env::current_exe`.
pub fn resolve_from_exe_dir(dir: &Path) -> Option<PathBuf> {
    // Step 2: macOS .app bundle layout.
    if dir.ends_with("Contents/MacOS") {
        if let Some(contents) = dir.parent() {
            let res = contents.join("Resources").join("assets");
            if res.is_dir() {
                return Some(res);
            }
        }
    }
    // Step 3: assets/ placed next to the binary.
    let next = dir.join("assets");
    if next.is_dir() {
        return Some(next);
    }
    None
}

#[cfg(test)]
// `unsafe_code`: `std::env::set_var`/`remove_var` are unsafe in Rust ≥1.80
// (unsynchronised mutation can race concurrent reads); the `ENV_MUTEX` below
// serialises every mutation so there is no data race.  These calls appear only
// in test code and never in the production binary.
//
// `expect_used`: `expect` is appropriate in test scaffolding where a setup
// failure should abort the test with a clear message.
#[allow(unsafe_code, clippy::expect_used)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    /// Serialise tests that mutate environment variables so they do not race
    /// when `cargo test --lib` runs tests in parallel threads.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    /// Build a unique-per-process temp path so parallel test processes (nextest)
    /// do not collide with each other.
    fn tmp_subdir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("wc_assets_test_{}_{}", label, std::process::id()))
    }

    #[test]
    fn asset_root_env_override_wins() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tmp_subdir("env");
        fs::create_dir_all(&dir).expect("create temp dir");
        // SAFETY: serialised by ENV_MUTEX; no other thread touches
        // WAVECONDUCTOR_ASSET_ROOT while the lock is held.
        unsafe {
            std::env::set_var("WAVECONDUCTOR_ASSET_ROOT", &dir);
        }
        let result = asset_root();
        unsafe {
            std::env::remove_var("WAVECONDUCTOR_ASSET_ROOT");
        }
        fs::remove_dir_all(&dir).ok();
        assert_eq!(result, dir, "env override should be returned verbatim");
    }

    #[test]
    fn resolve_from_exe_dir_bundle_layout() {
        // Fabricate: <tmp>/Foo.app/Contents/MacOS  (simulated exe dir)
        //            <tmp>/Foo.app/Contents/Resources/assets
        let root = tmp_subdir("bundle");
        let macos_dir = root.join("Foo.app/Contents/MacOS");
        let resources_assets = root.join("Foo.app/Contents/Resources/assets");
        fs::create_dir_all(&macos_dir).expect("create Contents/MacOS");
        fs::create_dir_all(&resources_assets).expect("create Contents/Resources/assets");

        let result = resolve_from_exe_dir(&macos_dir);
        fs::remove_dir_all(&root).ok();

        assert_eq!(
            result,
            Some(resources_assets),
            "bundle exe dir should resolve to Contents/Resources/assets"
        );
    }

    #[test]
    fn resolve_from_exe_dir_next_to_binary() {
        // Fabricate: <tmp>/bin-dir/assets/
        let root = tmp_subdir("next_to_binary");
        let bin_dir = root.join("bin-dir");
        let assets_dir = bin_dir.join("assets");
        fs::create_dir_all(&assets_dir).expect("create assets dir");

        let result = resolve_from_exe_dir(&bin_dir);
        fs::remove_dir_all(&root).ok();

        assert_eq!(
            result,
            Some(assets_dir),
            "assets next to the binary should resolve correctly"
        );
    }

    #[test]
    fn resolve_from_exe_dir_returns_none_when_no_assets() {
        let root = tmp_subdir("no_assets");
        let bin_dir = root.join("bin-dir");
        fs::create_dir_all(&bin_dir).expect("create bin dir");

        let result = resolve_from_exe_dir(&bin_dir);
        fs::remove_dir_all(&root).ok();

        assert!(result.is_none(), "should return None when no assets dir exists");
    }
}
