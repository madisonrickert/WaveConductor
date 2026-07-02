//! Per-platform persistence for [`SketchSettings`].
//!
//! ## Native
//!
//! A single TOML file at `dirs::config_dir() / "waveconductor" / "sketch-settings.toml"`.
//! Each settings struct occupies one top-level table keyed by its
//! [`SketchSettings::STORAGE_KEY`]:
//!
//! ```toml
//! [line]
//! particle_count = 5000
//! attractor_decay = 0.92
//!
//! [flame]
//! ...
//! ```
//!
//! The override env var [`CONFIG_DIR_ENV`] is consulted first; integration
//! tests use a `TempDir` and set this var so they never touch the real
//! XDG/macOS config dir.
//!
//! ## Web
//!
//! `web-sys`'s `window().local_storage()` with one JSON-encoded value per
//! sketch under key `wc-sketch-settings:<STORAGE_KEY>`. JSON instead of TOML
//! because `serde_json` has a much smaller wasm footprint than `toml`.

use std::path::{Path, PathBuf};

use super::trait_def::SketchSettings;

/// Environment variable that overrides the OS-determined config directory.
/// Production code does not set this; tests do, to point at a `TempDir`.
pub const CONFIG_DIR_ENV: &str = "WAVECONDUCTOR_CONFIG_DIR";

/// Returns the absolute path to the combined settings TOML file.
///
/// Falls back to the current working directory if neither
/// [`CONFIG_DIR_ENV`] nor [`dirs::config_dir`] yields a path — the only
/// realistic case is a stripped-down sandbox without any home env vars set,
/// and writing to CWD is still better than panicking.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn settings_path() -> PathBuf {
    let base = std::env::var_os(CONFIG_DIR_ENV)
        .map(PathBuf::from)
        .or_else(dirs::config_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("waveconductor").join("sketch-settings.toml")
}

/// Load the value for a specific settings type. Returns `S::default()` on
/// any error (file missing, parse failure, schema mismatch). Errors are
/// logged at `warn` level.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn load<S: SketchSettings>() -> S {
    let path = settings_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        tracing::debug!(
            ?path,
            key = S::STORAGE_KEY,
            "no settings file; using defaults"
        );
        return S::default();
    };
    let table: toml::Table = match toml::from_str(&text) {
        Ok(t) => t,
        Err(err) => {
            tracing::warn!(?err, "settings file is malformed TOML; using defaults");
            return S::default();
        }
    };
    let Some(value) = table.get(S::STORAGE_KEY) else {
        return S::default();
    };
    match value.clone().try_into::<S>() {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(
                ?err,
                key = S::STORAGE_KEY,
                "settings section failed to deserialize; using defaults",
            );
            S::default()
        }
    }
}

/// Persist a single settings struct. Reads the existing file, replaces
/// `[<STORAGE_KEY>]`, and writes it back. Errors are logged but not
/// returned; a settings save failure should never crash the app.
///
/// Two data-loss hazards are guarded against:
///
/// * **Corrupt-file clobbering.** The merge step ([`load_merge_table`])
///   distinguishes an absent file (fresh table, no fuss) from a present but
///   unparseable one. A present-but-corrupt file is *quarantined* (renamed to
///   a sibling `.corrupt-<n>`) and logged at `error!` before we proceed with a
///   fresh table, so a single malformed section can never silently erase every
///   other sketch's settings.
/// * **Torn writes.** The new contents go to a temp file in the *same*
///   directory, which is then [`std::fs::rename`]d over the target. `rename`
///   is atomic on a single filesystem and replaces the destination on Unix and
///   Windows 10+, so a crash or power loss mid-write leaves either the old file
///   or the new one intact — never a half-written one.
#[cfg(not(target_arch = "wasm32"))]
pub fn save<S: SketchSettings>(settings: &S) {
    let path = settings_path();
    let mut table = load_merge_table(&path);

    let new_value = match toml::Value::try_from(settings) {
        Ok(v) => v,
        Err(err) => {
            tracing::error!(?err, key = S::STORAGE_KEY, "settings failed to serialize");
            return;
        }
    };
    table.insert(S::STORAGE_KEY.to_string(), new_value);

    let serialized = match toml::to_string_pretty(&table) {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(?err, "settings table failed to serialize");
            return;
        }
    };

    if let Some(parent) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            tracing::error!(?err, ?parent, "failed to create config dir");
            return;
        }
    }

    // Atomic replace: stage to a sibling temp file, then rename over the
    // target. Writing in place (the old `std::fs::write(&path, ...)`) risked a
    // truncated file on a crash between truncate and full write.
    let tmp_path = temp_write_path(&path);
    if let Err(err) = std::fs::write(&tmp_path, serialized) {
        tracing::error!(?err, ?tmp_path, "failed to write temporary settings file");
        return;
    }
    if let Err(err) = std::fs::rename(&tmp_path, &path) {
        tracing::error!(
            ?err,
            ?tmp_path,
            ?path,
            "failed to atomically replace settings file"
        );
        // Best-effort cleanup so a failed rename does not leave the temp file
        // littering the config dir.
        let _ = std::fs::remove_file(&tmp_path);
    }
}

/// Read and parse the existing settings file into a table to merge the new
/// section into. Returns a fresh empty table when the file is absent (the
/// normal first-save case) or unreadable.
///
/// On a present-but-unparseable file the bad file is quarantined (see
/// [`quarantine_path`]) and the error logged, then a fresh table is returned.
/// This is the crux of the corrupt-file guard: we never fold a malformed file
/// into `default()` and write that back over the top, which would erase every
/// sibling section.
#[cfg(not(target_arch = "wasm32"))]
fn load_merge_table(path: &Path) -> toml::Table {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // No existing file: fresh table. Expected on the very first save.
            return toml::Table::new();
        }
        Err(err) => {
            tracing::warn!(
                ?err,
                ?path,
                "could not read existing settings file for merge; starting fresh"
            );
            return toml::Table::new();
        }
    };

    match toml::from_str::<toml::Table>(&text) {
        Ok(table) => table,
        Err(err) => {
            if let Some(dest) = quarantine_path(path) {
                match std::fs::rename(path, &dest) {
                    Ok(()) => tracing::error!(
                        ?err,
                        ?path,
                        quarantine = ?dest,
                        "settings file is corrupt; quarantined it and starting from a fresh table",
                    ),
                    Err(rename_err) => tracing::error!(
                        ?err,
                        ?rename_err,
                        ?path,
                        "settings file is corrupt and could not be quarantined; starting fresh",
                    ),
                }
            } else {
                tracing::error!(
                    ?err,
                    ?path,
                    "settings file is corrupt and no free quarantine name was found; starting fresh",
                );
            }
            toml::Table::new()
        }
    }
}

/// Sibling path of `path` used to stage an atomic write. Lives in the same
/// directory (so the subsequent [`std::fs::rename`] never crosses a filesystem
/// boundary) and carries the current process id to avoid two instances racing
/// on the same temp name. Falls back to a fixed stem if `path` has no file
/// name (not reachable for the real settings path, which always ends in a
/// file).
#[cfg(not(target_arch = "wasm32"))]
fn temp_write_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map_or_else(
        || std::ffi::OsString::from("sketch-settings.toml"),
        std::ffi::OsStr::to_os_string,
    );
    name.push(format!(".tmp-{}", std::process::id()));
    path.with_file_name(name)
}

/// First unused `<file-name>.corrupt-<n>` sibling of `path`, scanning `n` from
/// `0` upward. Returns `None` if `path` has no file name or every candidate up
/// to `u32::MAX` is taken (neither is reachable in practice).
#[cfg(not(target_arch = "wasm32"))]
fn quarantine_path(path: &Path) -> Option<PathBuf> {
    let file_name = path.file_name()?;
    let mut n: u32 = 0;
    loop {
        let mut name = file_name.to_os_string();
        name.push(format!(".corrupt-{n}"));
        let candidate = path.with_file_name(name);
        if !candidate.exists() {
            return Some(candidate);
        }
        n = n.checked_add(1)?;
    }
}

// -- Web --------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
fn local_storage_key<S: SketchSettings>() -> String {
    format!("wc-sketch-settings:{}", S::STORAGE_KEY)
}

#[cfg(target_arch = "wasm32")]
#[must_use]
pub fn load<S: SketchSettings>() -> S {
    let key = local_storage_key::<S>();
    let storage = web_sys::window().and_then(|w| w.local_storage().ok().flatten());
    let Some(storage) = storage else {
        tracing::debug!("localStorage unavailable; using defaults");
        return S::default();
    };
    let Ok(Some(text)) = storage.get_item(&key) else {
        return S::default();
    };
    match serde_json::from_str::<S>(&text) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(?err, %key, "localStorage value failed to deserialize");
            S::default()
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub fn save<S: SketchSettings>(settings: &S) {
    let key = local_storage_key::<S>();
    let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) else {
        tracing::error!("localStorage unavailable; cannot save settings");
        return;
    };
    let serialized = match serde_json::to_string(settings) {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(?err, %key, "settings failed to serialize");
            return;
        }
    };
    if let Err(err) = storage.set_item(&key, &serialized) {
        tracing::error!(?err, %key, "localStorage set_item failed");
    }
}
