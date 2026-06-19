//! Image template library: a content-addressed, app-managed store for images
//! used as particle-spawn templates.
//!
//! # Data flow
//! An image picked by the user is **ingested**
//! ([`store::ingest`](crate::templates::store::ingest)): its bytes are
//! content-hashed with `blake3`, copied verbatim into the managed store as
//! `<hash>.<ext>`, a downscaled thumbnail is baked to `thumbs/<hash>.png`, and a
//! [`manifest::TemplateEntry`](crate::templates::manifest::TemplateEntry) is
//! appended to `manifest.toml`. Sketches reference
//! the managed blob by absolute path, so deleting the user's original file no
//! longer breaks the template. The store is native-only (file dialog + decode)
//! and lives behind the `templates` cargo feature.

pub mod manifest;
pub mod resource;
pub mod store;
pub mod view;

use std::path::PathBuf;

/// Environment override for the data directory root (parallels
/// `WAVECONDUCTOR_CONFIG_DIR` in `settings::persistence`). Tests set this to a
/// `TempDir`; production falls back to [`dirs::data_dir`].
pub const DATA_DIR_ENV: &str = "WAVECONDUCTOR_DATA_DIR";

/// Absolute path to the managed templates directory
/// (`<data_dir>/waveconductor/templates`). Does not create it; the store
/// creates directories on write.
#[must_use]
pub fn templates_dir() -> PathBuf {
    let base = std::env::var_os(DATA_DIR_ENV)
        .map(PathBuf::from)
        .or_else(dirs::data_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("waveconductor").join("templates")
}
