//! The template manifest: a TOML sidecar (`manifest.toml`) mapping each cached
//! blob's content hash to its friendly metadata. Mirrors the TOML round-trip
//! style of `settings::persistence`.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// One cached template. The `hash` is the blob filename stem and the dedup key.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TemplateEntry {
    /// `blake3` hex digest of the source bytes; also the blob filename stem.
    pub hash: String,
    /// Source file extension, lowercased (e.g. `"png"`). Blob is `<hash>.<ext>`.
    pub ext: String,
    /// The picked file's name, shown as the human-facing label.
    pub original_name: String,
    /// Unix seconds at import. Used to sort newest-first; not displayed.
    pub imported_at: u64,
    /// Source pixel width, for the `WxH` subtext.
    pub width: u32,
    /// Source pixel height, for the `WxH` subtext.
    pub height: u32,
    /// Source byte size, for the human-readable size subtext.
    pub bytes: u64,
    /// Thumbnail path relative to the templates dir (e.g. `thumbs/<hash>.png`).
    pub thumb: String,
}

/// The whole manifest: a flat array of [`TemplateEntry`] (TOML `[[template]]`).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Manifest {
    /// All cached templates, in insertion order.
    #[serde(default)]
    pub template: Vec<TemplateEntry>,
}

/// Path to the manifest file within `dir`.
#[must_use]
pub fn manifest_path(dir: &Path) -> std::path::PathBuf {
    dir.join("manifest.toml")
}

/// Load the manifest from `dir`. A missing or malformed file yields an empty
/// manifest (logged), never an error — the store degrades gracefully.
#[must_use]
pub fn load_manifest(dir: &Path) -> Manifest {
    let path = manifest_path(dir);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Manifest::default();
    };
    match toml::from_str(&text) {
        Ok(m) => m,
        Err(err) => {
            tracing::warn!(?err, ?path, "template manifest malformed; using empty");
            Manifest::default()
        }
    }
}

/// Persist the manifest to `dir`, creating the directory if needed.
pub fn save_manifest(dir: &Path, m: &Manifest) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let text = toml::to_string_pretty(m).map_err(std::io::Error::other)?;
    std::fs::write(manifest_path(dir), text)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is fine in test code")]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn save_then_load_round_trips() {
        let dir = TempDir::new().unwrap();
        let m = Manifest {
            template: vec![TemplateEntry {
                hash: "abc123".into(),
                ext: "png".into(),
                original_name: "wave.png".into(),
                imported_at: 1000,
                width: 1280,
                height: 853,
                bytes: 4242,
                thumb: "thumbs/abc123.png".into(),
            }],
        };
        save_manifest(dir.path(), &m).unwrap();
        let loaded = load_manifest(dir.path());
        assert_eq!(loaded.template, m.template);
    }

    #[test]
    fn load_missing_is_empty() {
        let dir = TempDir::new().unwrap();
        assert!(load_manifest(dir.path()).template.is_empty());
    }

    #[test]
    fn load_corrupt_is_empty() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join("manifest.toml"), "not [[ valid").unwrap();
        assert!(load_manifest(dir.path()).template.is_empty());
    }
}
