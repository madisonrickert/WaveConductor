//! Filesystem operations for the template store: content hashing, ingest
//! (copy + thumbnail bake + manifest upsert), delete, reconcile.

use std::path::Path;

use crate::templates::manifest::{load_manifest, save_manifest, Manifest, TemplateEntry};

/// Longest-edge size (px) of baked thumbnails. Bounds decode + GPU cost in the
/// dropdown regardless of source resolution.
const THUMB_MAX_EDGE: u32 = 96;

/// Errors from [`ingest`].
#[derive(Debug)]
pub enum IngestError {
    /// Reading the source file failed (e.g. it was deleted).
    Read(std::io::Error),
    /// Decoding the source image failed (unsupported / corrupt).
    Decode(image::ImageError),
    /// Writing a blob, thumbnail, or the manifest failed.
    Write(std::io::Error),
}

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(e) => write!(f, "read source: {e}"),
            Self::Decode(e) => write!(f, "decode image: {e}"),
            Self::Write(e) => write!(f, "write store: {e}"),
        }
    }
}

impl std::error::Error for IngestError {}

/// `blake3` hex digest of `bytes`. Stable across runs, platforms, and Rust
/// versions, so it is safe as a persistent content-addressed cache key.
#[must_use]
pub fn content_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Copy `source` into the managed store at `dir`, baking a thumbnail and
/// upserting the manifest. Idempotent: re-ingesting identical bytes returns the
/// existing entry without duplicating the blob. The source's raw bytes are
/// stored verbatim (faithful copy); the sampler downscales at spawn time.
pub fn ingest(dir: &Path, source: &Path) -> Result<TemplateEntry, IngestError> {
    let bytes = std::fs::read(source).map_err(IngestError::Read)?;
    let hash = content_hash(&bytes);

    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_else(|| "png".to_string());
    let original_name = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("{hash}.{ext}"));

    // Decode once for dimensions + thumbnail.
    let decoded = image::load_from_memory(&bytes).map_err(IngestError::Decode)?;
    let (width, height) = (decoded.width(), decoded.height());

    std::fs::create_dir_all(dir).map_err(IngestError::Write)?;
    let blob_path = dir.join(format!("{hash}.{ext}"));
    if !blob_path.exists() {
        std::fs::write(&blob_path, &bytes).map_err(IngestError::Write)?;
    }

    let thumbs_dir = dir.join("thumbs");
    std::fs::create_dir_all(&thumbs_dir).map_err(IngestError::Write)?;
    let thumb_rel = format!("thumbs/{hash}.png");
    let thumb_path = dir.join(&thumb_rel);
    if !thumb_path.exists() {
        // `thumbnail` preserves aspect ratio with the longest edge <= max.
        decoded
            .thumbnail(THUMB_MAX_EDGE, THUMB_MAX_EDGE)
            .save_with_format(&thumb_path, image::ImageFormat::Png)
            .map_err(|e| match e {
                image::ImageError::IoError(io) => IngestError::Write(io),
                other => IngestError::Decode(other),
            })?;
    }

    let entry = TemplateEntry {
        hash: hash.clone(),
        ext,
        original_name,
        imported_at: unix_now(),
        width,
        height,
        // `usize -> u64` is lossless on supported targets; `try_from` keeps us
        // off `as` casts (AGENTS.md) and degrades safely on the impossible path.
        bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        thumb: thumb_rel,
    };

    // Upsert: replace any existing entry with the same hash, else append.
    let mut manifest: Manifest = load_manifest(dir);
    if let Some(slot) = manifest.template.iter_mut().find(|e| e.hash == hash) {
        *slot = entry.clone();
    } else {
        manifest.template.push(entry.clone());
    }
    save_manifest(dir, &manifest).map_err(IngestError::Write)?;

    Ok(entry)
}

/// Unix seconds now, or 0 if the clock is before the epoch (never, in practice).
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Write a deterministic test PNG to `path` and return its path.
    fn write_test_png(path: &std::path::Path, w: u32, h: u32) {
        let mut img = image::RgbaImage::new(w, h);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255]);
        }
        img.save(path).unwrap();
    }

    #[test]
    fn content_hash_is_stable_and_dedups() {
        assert_eq!(content_hash(b"hello"), content_hash(b"hello"));
        assert_ne!(content_hash(b"hello"), content_hash(b"world"));
        // 64 hex chars for a 256-bit blake3 digest.
        assert_eq!(content_hash(b"hello").len(), 64);
    }

    #[test]
    fn ingest_writes_blob_thumb_and_entry() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("portrait.png");
        write_test_png(&src, 200, 100);

        let entry = ingest(dir.path(), &src).unwrap();

        assert_eq!(entry.original_name, "portrait.png");
        assert_eq!(entry.ext, "png");
        assert_eq!(entry.width, 200);
        assert_eq!(entry.height, 100);
        assert!(entry.bytes > 0);
        assert!(dir.path().join(format!("{}.png", entry.hash)).exists());
        assert!(dir.path().join(&entry.thumb).exists());
        let m = load_manifest(dir.path());
        assert_eq!(m.template.len(), 1);
        assert_eq!(m.template[0], entry);
    }

    #[test]
    fn ingest_is_idempotent_on_identical_bytes() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("a.png");
        write_test_png(&src, 64, 64);

        let e1 = ingest(dir.path(), &src).unwrap();
        let e2 = ingest(dir.path(), &src).unwrap();

        assert_eq!(e1.hash, e2.hash);
        assert_eq!(load_manifest(dir.path()).template.len(), 1);
    }

    #[test]
    fn ingest_missing_source_errors() {
        let dir = TempDir::new().unwrap();
        let err = ingest(dir.path(), &dir.path().join("nope.png"));
        assert!(err.is_err());
        assert!(load_manifest(dir.path()).template.is_empty());
    }
}
