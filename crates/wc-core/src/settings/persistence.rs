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

use std::path::PathBuf;

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
#[cfg(not(target_arch = "wasm32"))]
pub fn save<S: SketchSettings>(settings: &S) {
    let path = settings_path();
    let mut table: toml::Table = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default();

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
    if let Err(err) = std::fs::write(&path, serialized) {
        tracing::error!(?err, ?path, "failed to write settings file");
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
