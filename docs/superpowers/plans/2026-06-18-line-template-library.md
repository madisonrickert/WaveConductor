# Line Image Template Library Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the Line sketch's path-only image template into a persistent, app-managed template library: a content-addressed image cache with a manifest, an egui dropdown to select/import/delete cached templates with thumbnails, and graceful handling when the original file is deleted.

**Architecture:** A generic image-template store lives in `wc-core` (`templates/`), behind a native-only `templates` cargo feature (mirrors `hand-tracking-mediapipe`, which gates the optional `image` dep). The store does content-hashed blob copies + a TOML manifest + baked thumbnails. A new `SettingKind::TemplateLibrary` variant renders a bespoke egui `ComboBox` in the settings dock. The Line sketch opts its `spawn_template` field into the library widget and gains a migration system that ingests legacy/external paths. The field stays a `String` path on disk, so persistence/restart semantics are unchanged.

**Tech Stack:** Rust, Bevy 0.18, bevy_egui 0.39 (egui 0.33), `image` 0.25, `blake3` (content hash, already compiled via `bevy_asset`), `toml`, `dirs` 6, `rfd` (native file dialog), `tempfile` (dev).

## Global Constraints

Copied from `AGENTS.md` and the spec (`docs/superpowers/specs/2026-06-18-line-template-library-design.md`). Every task implicitly includes these.

- **No new dependency unless already in the build graph.** `blake3` chosen because `bevy_asset` already compiles it (zero added cost); `image` already compiled. Verify with `cargo tree -i <crate> -e normal --workspace`. See memory `feedback-avoid-new-dependencies`.
- **Versions:** Bevy `0.18`, bevy_egui `0.39` (egui `0.33`, accessed as `bevy_egui::egui`), `image` `0.25` (features `png`/`jpeg`/`webp`), `dirs` `6`, egui-phosphor `0.11`.
- **No `unwrap()`/`expect()` in non-test code** unless a documented invariant. Return `Result`, log with `tracing::warn!`/`error!`, fall back.
- **Never allocate in a hot path.** Thumbnail decode is one-time-per-template-per-session (cached in a resource), not per-frame. The settings panel runs only while the dock is open.
- **`///` rustdoc on every public item; `//!` on every module root.** Inline `//` for non-obvious logic.
- **One concept per file; files under ~300 lines.** Public API at top, private helpers at bottom, `#[cfg(test)] mod tests` at the footer.
- **No hardcoded local paths.** Use `dirs::data_dir()` + a `WAVECONDUCTOR_DATA_DIR` env override (mirrors the existing `WAVECONDUCTOR_CONFIG_DIR`).
- **Native-only.** Templates use `rfd` (no wasm file dialog) and `image`; the whole feature is gated `#[cfg(feature = "templates")]` and the feature pulls native-only deps. Web builds never enable it.
- **CI gates (run before claiming done, `--all-features` exercises `templates`):** `cargo fmt --all -- --check`; `cargo clippy --all-targets --all-features --workspace -- -D warnings`; `cargo nextest run --workspace --all-features` (+ `cargo test --doc --workspace`); `cargo doc --no-deps --workspace --document-private-items`; `cargo deny check`; `cargo xtask check-secrets`.
- **Dev run:** `cargo rund` (never the bare `target/` binary).

---

## File Structure

**Create:**
- `crates/wc-core/src/templates/mod.rs` — module root (`//!` doc, re-exports), `templates_dir()`.
- `crates/wc-core/src/templates/manifest.rs` — `TemplateEntry`, `Manifest`, `load_manifest`/`save_manifest`.
- `crates/wc-core/src/templates/store.rs` — `content_hash`, `ingest`, `delete`, `reconcile`, `is_managed`, `managed_path`.
- `crates/wc-core/src/templates/resource.rs` — `TemplateLibrary`, `TemplateThumbnailCache` resources, `TemplatesPlugin`.
- `crates/wc-core/src/templates/view.rs` — `TemplateRow` view-model + `build_rows`, `human_bytes` (pure, egui-free).
- `crates/wc-sketches/src/line/template_migrate.rs` — `migrate_spawn_template` system.

**Modify:**
- `Cargo.toml` (workspace) — add `blake3` to `[workspace.dependencies]`.
- `crates/wc-core/Cargo.toml` — `templates` feature + optional `blake3`.
- `crates/wc-core/src/lib.rs` — `pub mod templates;` (gated) + register `TemplatesPlugin`.
- `crates/wc-core/src/settings/def.rs` — `SettingKind::TemplateLibrary` variant.
- `crates/wc-core-macros/src/lib.rs` — `Kind::TemplateLibrary` + ty parse + codegen.
- `crates/wc-core-macros/tests/derive.rs` — macro test.
- `crates/wc-core/src/settings/panel_user.rs` — the widget + snapshot/threading + apply.
- `crates/wc-sketches/src/line/settings.rs` — `spawn_template` → `ty = TemplateLibrary`.
- `crates/wc-sketches/src/line/mod.rs` — register migration system; `mod template_migrate;`.
- `crates/wc-sketches/Cargo.toml` — `templates` feature forwarding.
- `crates/waveconductor/Cargo.toml` — enable `templates`.

---

## Task 1: Template store scaffolding — Cargo wiring, dirs, manifest model

**Files:**
- Modify: `Cargo.toml` (workspace `[workspace.dependencies]`)
- Modify: `crates/wc-core/Cargo.toml`
- Create: `crates/wc-core/src/templates/mod.rs`
- Create: `crates/wc-core/src/templates/manifest.rs`
- Modify: `crates/wc-core/src/lib.rs:18-32` (module decls)

**Interfaces:**
- Produces: `wc_core::templates::templates_dir() -> std::path::PathBuf`; `wc_core::templates::manifest::{TemplateEntry, Manifest, load_manifest, save_manifest}`.
- `TemplateEntry { hash: String, ext: String, original_name: String, imported_at: u64, width: u32, height: u32, bytes: u64, thumb: String }`.
- `Manifest { template: Vec<TemplateEntry> }`.
- `load_manifest(dir: &Path) -> Manifest` (missing/corrupt → empty, logged).
- `save_manifest(dir: &Path, m: &Manifest) -> std::io::Result<()>`.

- [ ] **Step 1: Add `blake3` to workspace dependencies**

In `Cargo.toml` under `[workspace.dependencies]` (starts at line 19), add a line (keep alphabetical-ish, near `bevy`):

```toml
blake3 = "1"
```

- [ ] **Step 2: Add the `templates` feature + optional `blake3` to wc-core**

In `crates/wc-core/Cargo.toml`, under `[features]` (after the `default = []` line, line 16) add:

```toml
# Image template library: content-addressed cache + manifest + thumbnails for
# the spawn-template picker. Native-only (uses `rfd`/`image`); mirrors the
# `hand-tracking-mediapipe` pattern of a feature that pulls optional native deps.
templates = ["dep:image", "dep:blake3"]
```

In the same file, under `[dependencies]` (after line 65 `dirs.workspace = true`) add:

```toml
blake3 = { workspace = true, optional = true }
```

(`image` is already declared optional under the non-wasm target table at line 75; the `templates` feature turns it on.)

- [ ] **Step 3: Create the module root `crates/wc-core/src/templates/mod.rs`**

```rust
//! Image template library: a content-addressed, app-managed store for images
//! used as particle-spawn templates.
//!
//! # Data flow
//! An image picked by the user is **ingested** ([`store::ingest`]): its bytes
//! are content-hashed with `blake3`, copied verbatim into the managed store as
//! `<hash>.<ext>`, a downscaled thumbnail is baked to `thumbs/<hash>.png`, and a
//! [`manifest::TemplateEntry`] is appended to `manifest.toml`. Sketches reference
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
```

- [ ] **Step 4: Write the failing manifest round-trip test**

Create `crates/wc-core/src/templates/manifest.rs` with only the test module first:

```rust
#[cfg(test)]
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
```

- [ ] **Step 5: Run the test to verify it fails**

Run: `cargo test -p wc-core --features templates templates::manifest -- --nocapture`
Expected: FAIL — `cannot find type Manifest` / `cannot find function save_manifest`.

- [ ] **Step 6: Implement the manifest model + load/save**

Prepend to `crates/wc-core/src/templates/manifest.rs` (above the test module):

```rust
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
```

- [ ] **Step 7: Add module declaration in `lib.rs`**

In `crates/wc-core/src/lib.rs`, after the `pub mod sketch;` line (around line 30), add:

```rust
/// Image template library (native-only, behind the `templates` feature).
#[cfg(feature = "templates")]
pub mod templates;
```

Create temporary stub files so the module compiles (they are filled by later tasks). Create `crates/wc-core/src/templates/store.rs`, `resource.rs`, `view.rs` each containing exactly:

```rust
//! (placeholder — implemented in a later task)
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test -p wc-core --features templates templates::manifest`
Expected: PASS (3 tests).

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml crates/wc-core/Cargo.toml crates/wc-core/src/templates crates/wc-core/src/lib.rs
git commit -F - <<'EOF'
feat(templates): scaffold image template store (dirs + manifest)

Adds the `templates` cargo feature (pulls optional image + blake3, mirrors
hand-tracking-mediapipe), the templates module root with templates_dir(), and
the TOML manifest model with load/save round-trip.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

## Task 2: Content hash + ingest (copy blob + bake thumbnail)

**Files:**
- Modify: `crates/wc-core/src/templates/store.rs`

**Interfaces:**
- Consumes: `manifest::{TemplateEntry, Manifest, load_manifest, save_manifest}`.
- Produces:
  - `content_hash(bytes: &[u8]) -> String` (blake3 hex).
  - `ingest(dir: &Path, source: &Path) -> Result<TemplateEntry, IngestError>`.
  - `enum IngestError { Read(std::io::Error), Decode(image::ImageError), Write(std::io::Error) }` (impl `std::error::Error` + `Display`).

- [ ] **Step 1: Write the failing ingest tests**

Append a test module to `crates/wc-core/src/templates/store.rs` (replace the placeholder line):

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p wc-core --features templates templates::store`
Expected: FAIL — `cannot find function content_hash` / `ingest`.

- [ ] **Step 3: Implement `content_hash` + `ingest`**

Prepend to `crates/wc-core/src/templates/store.rs` (above the test module, replacing the placeholder line):

```rust
//! Filesystem operations for the template store: content hashing, ingest
//! (copy + thumbnail bake + manifest upsert), delete, reconcile.

use std::path::{Path, PathBuf};

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
        bytes: bytes.len() as u64,
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
```

Note: `entry.bytes` uses `bytes.len() as u64`. `usize -> u64` is lossless on all supported targets; `as` is acceptable here (widening). If clippy objects, use `u64::try_from(bytes.len()).unwrap_or(u64::MAX)`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p wc-core --features templates templates::store`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/templates/store.rs
git commit -F - <<'EOF'
feat(templates): content hash + ingest (copy blob, bake thumbnail)

blake3 content-addressed ingest copies source bytes verbatim, bakes a 96px
thumbnail, and upserts the TOML manifest. Idempotent on identical bytes.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

## Task 3: delete, reconcile, is_managed, managed_path

**Files:**
- Modify: `crates/wc-core/src/templates/store.rs`

**Interfaces:**
- Produces:
  - `delete(dir: &Path, hash: &str) -> std::io::Result<()>`.
  - `reconcile(dir: &Path) -> Manifest` (drops entries whose blob is missing; persists; returns the pruned manifest).
  - `is_managed(dir: &Path, path: &Path) -> bool`.
  - `managed_path(dir: &Path, entry: &TemplateEntry) -> PathBuf`.

- [ ] **Step 1: Write the failing tests**

Add these to the existing `tests` module in `store.rs`:

```rust
    #[test]
    fn delete_removes_blob_thumb_and_entry() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("a.png");
        write_test_png(&src, 32, 32);
        let entry = ingest(dir.path(), &src).unwrap();

        delete(dir.path(), &entry.hash).unwrap();

        assert!(!dir.path().join(format!("{}.png", entry.hash)).exists());
        assert!(!dir.path().join(&entry.thumb).exists());
        assert!(load_manifest(dir.path()).template.is_empty());
    }

    #[test]
    fn reconcile_drops_entries_with_missing_blob() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("a.png");
        write_test_png(&src, 32, 32);
        let entry = ingest(dir.path(), &src).unwrap();
        // Delete the blob out-of-band, leaving the manifest entry dangling.
        std::fs::remove_file(dir.path().join(format!("{}.png", entry.hash))).unwrap();

        let pruned = reconcile(dir.path());

        assert!(pruned.template.is_empty());
        assert!(load_manifest(dir.path()).template.is_empty());
    }

    #[test]
    fn is_managed_distinguishes_store_paths() {
        let dir = TempDir::new().unwrap();
        assert!(is_managed(dir.path(), &dir.path().join("abc.png")));
        assert!(is_managed(dir.path(), &dir.path().join("thumbs/abc.png")));
        assert!(!is_managed(dir.path(), std::path::Path::new("/somewhere/else/abc.png")));
    }

    #[test]
    fn managed_path_points_at_blob() {
        let dir = TempDir::new().unwrap();
        let entry = TemplateEntry {
            hash: "h".into(),
            ext: "jpg".into(),
            original_name: "x.jpg".into(),
            imported_at: 0,
            width: 1,
            height: 1,
            bytes: 1,
            thumb: "thumbs/h.png".into(),
        };
        assert_eq!(managed_path(dir.path(), &entry), dir.path().join("h.jpg"));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p wc-core --features templates templates::store`
Expected: FAIL — `cannot find function delete` / `reconcile` / `is_managed` / `managed_path`.

- [ ] **Step 3: Implement the four functions**

Append to `store.rs` (above the test module):

```rust
/// Absolute path to the blob for `entry` within `dir`.
#[must_use]
pub fn managed_path(dir: &Path, entry: &TemplateEntry) -> PathBuf {
    dir.join(format!("{}.{}", entry.hash, entry.ext))
}

/// True if `path` lives inside the managed store `dir` (so it must not be
/// re-ingested). Compares by prefix; both paths are used as-is (callers pass
/// absolute paths).
#[must_use]
pub fn is_managed(dir: &Path, path: &Path) -> bool {
    path.starts_with(dir)
}

/// Remove the blob, thumbnail, and manifest entry for `hash`. Missing files are
/// ignored (idempotent). Returns the first I/O error from manifest persistence.
pub fn delete(dir: &Path, hash: &str) -> std::io::Result<()> {
    let mut manifest = load_manifest(dir);
    if let Some(pos) = manifest.template.iter().position(|e| e.hash == hash) {
        let entry = manifest.template.remove(pos);
        // Best-effort blob + thumb removal; absence is not an error.
        let _ = std::fs::remove_file(managed_path(dir, &entry));
        let _ = std::fs::remove_file(dir.join(&entry.thumb));
    }
    save_manifest(dir, &manifest)
}

/// Drop manifest entries whose blob no longer exists on disk, persist, and
/// return the pruned manifest. Run once at startup to heal out-of-band deletes.
#[must_use]
pub fn reconcile(dir: &Path) -> Manifest {
    let mut manifest = load_manifest(dir);
    let before = manifest.template.len();
    manifest
        .template
        .retain(|e| managed_path(dir, e).exists());
    if manifest.template.len() != before {
        if let Err(err) = save_manifest(dir, &manifest) {
            tracing::warn!(?err, "failed to persist reconciled template manifest");
        }
    }
    manifest
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p wc-core --features templates templates::store`
Expected: PASS (8 tests total in the module).

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/templates/store.rs
git commit -F - <<'EOF'
feat(templates): delete, reconcile, is_managed, managed_path

Completes the filesystem store: deletion of blob+thumb+entry, startup
reconcile that prunes dangling entries, and path helpers.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

## Task 4: Bevy resources + TemplatesPlugin (load + reconcile on startup)

**Files:**
- Modify: `crates/wc-core/src/templates/resource.rs`
- Modify: `crates/wc-core/src/lib.rs:42-57` (register plugin in `CorePlugin`)

**Interfaces:**
- Consumes: `templates_dir()`, `store::reconcile`, `manifest::{Manifest, TemplateEntry}`.
- Produces:
  - `TemplateLibrary { dir: PathBuf, entries: Vec<TemplateEntry> }` (Bevy `Resource`), with `reload(&mut self)`.
  - `TemplateThumbnailCache(HashMap<String, bevy_egui::egui::TextureHandle>)` (Bevy `Resource`, default-empty).
  - `TemplatesPlugin`.

- [ ] **Step 1: Write the failing resource test**

Replace the placeholder in `crates/wc-core/src/templates/resource.rs` with a test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn reload_reads_manifest_from_dir() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("a.png");
        // Reuse the store's test PNG writer via a tiny inline image.
        let mut img = image::RgbaImage::new(8, 8);
        for px in img.pixels_mut() {
            *px = image::Rgba([10, 20, 30, 255]);
        }
        img.save(&src).unwrap();
        crate::templates::store::ingest(dir.path(), &src).unwrap();

        let mut lib = TemplateLibrary {
            dir: dir.path().to_path_buf(),
            entries: Vec::new(),
        };
        lib.reload();

        assert_eq!(lib.entries.len(), 1);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p wc-core --features templates templates::resource`
Expected: FAIL — `cannot find type TemplateLibrary`.

- [ ] **Step 3: Implement the resources + plugin**

Prepend to `resource.rs` (above the test module):

```rust
//! Bevy resources for the template library and the plugin that loads them.
//!
//! [`TemplateLibrary`] mirrors the on-disk manifest in memory (rebuilt on
//! import/delete); [`TemplateThumbnailCache`] holds session-lived egui texture
//! handles so thumbnails decode once. [`TemplatesPlugin`] loads + reconciles the
//! library at startup.

use std::collections::HashMap;
use std::path::PathBuf;

use bevy::prelude::*;
use bevy_egui::egui;

use crate::templates::manifest::TemplateEntry;
use crate::templates::{store, templates_dir};

/// In-memory view of the template manifest. `dir` is resolved once at startup so
/// every operation targets the same store. Rebuilt from disk via [`Self::reload`]
/// after any import or delete.
#[derive(Resource, Clone, Debug)]
pub struct TemplateLibrary {
    /// Absolute path to the managed templates directory.
    pub dir: PathBuf,
    /// Cached manifest entries (insertion order; the UI sorts a copy).
    pub entries: Vec<TemplateEntry>,
}

impl TemplateLibrary {
    /// Re-read the manifest from `dir` into `entries`.
    pub fn reload(&mut self) {
        self.entries = crate::templates::manifest::load_manifest(&self.dir).template;
    }
}

impl Default for TemplateLibrary {
    fn default() -> Self {
        Self {
            dir: templates_dir(),
            entries: Vec::new(),
        }
    }
}

/// Session-lived egui texture handles for thumbnails, keyed by content hash.
/// A handle kept here keeps its GPU texture alive; dropping it frees the texture.
#[derive(Resource, Default)]
pub struct TemplateThumbnailCache(pub HashMap<String, egui::TextureHandle>);

/// Loads + reconciles the template library at startup and registers its
/// resources. Inert without the panel (the cache only fills when the dock opens).
pub struct TemplatesPlugin;

impl Plugin for TemplatesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TemplateThumbnailCache>()
            .add_systems(Startup, load_library_on_startup);
    }
}

/// Startup system: resolve the store dir, prune dangling entries, and insert the
/// [`TemplateLibrary`] resource.
fn load_library_on_startup(mut commands: Commands) {
    let dir = templates_dir();
    let entries = store::reconcile(&dir).template;
    commands.insert_resource(TemplateLibrary { dir, entries });
}
```

- [ ] **Step 4: Register the plugin in `CorePlugin`**

In `crates/wc-core/src/lib.rs`, inside `CorePlugin::build` (after `app.add_plugins(ui::WaveConductorUiPlugin);`, around line 56), add:

```rust
        #[cfg(feature = "templates")]
        app.add_plugins(templates::resource::TemplatesPlugin);
```

- [ ] **Step 5: Run the test + a build check**

Run: `cargo test -p wc-core --features templates templates::resource`
Expected: PASS (1 test).
Run: `cargo build -p wc-core --features templates`
Expected: builds clean.

- [ ] **Step 6: Commit**

```bash
git add crates/wc-core/src/templates/resource.rs crates/wc-core/src/lib.rs
git commit -F - <<'EOF'
feat(templates): library + thumbnail-cache resources + startup load

TemplateLibrary mirrors the manifest in memory; TemplateThumbnailCache holds
session-lived egui texture handles. TemplatesPlugin reconciles + loads at
startup, registered in CorePlugin behind the templates feature.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

## Task 5: `SettingKind::TemplateLibrary` variant + macro + interim render arm

**Files:**
- Modify: `crates/wc-core/src/settings/def.rs:10-43`
- Modify: `crates/wc-core-macros/src/lib.rs` (Kind enum ~104-115, ty parse ~246-260, codegen ~367-382)
- Modify: `crates/wc-core-macros/tests/derive.rs`
- Modify: `crates/wc-core/src/settings/panel_user.rs:766-780` (render_widget_value match)
- Modify: `crates/wc-sketches/src/line/settings.rs:93-151` (field ty)

**Interfaces:**
- Produces: `SettingKind::TemplateLibrary { filter_label: &'static str, extensions: &'static [&'static str] }`.
- The macro accepts `ty = TemplateLibrary` with the same `filter_label` / `extensions` attributes as `FilePath`.

- [ ] **Step 1: Add the `SettingKind` variant**

In `crates/wc-core/src/settings/def.rs`, inside `enum SettingKind`, after the `FilePath { .. }` variant (before `Enum`), add:

```rust
    /// Like [`SettingKind::FilePath`], but the field is backed by the managed
    /// image **template library** (`crate::templates`): the widget is a picker
    /// of previously-imported templates with thumbnails plus an import button,
    /// and the stored value is the absolute path to the managed blob. The
    /// `filter_label` / `extensions` configure the import file dialog exactly as
    /// for `FilePath`. Falls back to a plain file picker when the `templates`
    /// feature is off.
    TemplateLibrary {
        /// File-dialog filter label for the Import action (e.g. "Image").
        filter_label: &'static str,
        /// Extensions the import dialog accepts (e.g. `&["png", "jpg"]`).
        extensions: &'static [&'static str],
    },
```

- [ ] **Step 2: Add the macro `Kind` variant + ty parse + codegen**

In `crates/wc-core-macros/src/lib.rs`:

(a) In `enum Kind` (~line 104), after `FilePath,` add:

```rust
    /// Managed image template library; same attributes as `FilePath`.
    TemplateLibrary,
```

(b) In the `ty` match (~line 251), after the `"FilePath" => Kind::FilePath,` arm add:

```rust
        "TemplateLibrary" => Kind::TemplateLibrary,
```

and update the error string's expected list to include `` `TemplateLibrary` ``.

(c) In the codegen match (~line 367), after the `Kind::FilePath => { .. }` arm add:

```rust
                Kind::TemplateLibrary => {
                    let filter_label =
                        f.filter_label.clone().unwrap_or_else(|| "File".to_string());
                    let exts: Vec<&str> = f
                        .extensions
                        .as_deref()
                        .unwrap_or(&[])
                        .iter()
                        .map(String::as_str)
                        .collect();
                    quote! {
                        ::wc_core::settings::SettingKind::TemplateLibrary {
                            filter_label: #filter_label,
                            extensions: &[ #( #exts, )* ],
                        }
                    }
                }
```

- [ ] **Step 3: Write the failing macro test**

In `crates/wc-core-macros/tests/derive.rs`, add a field with `ty = TemplateLibrary` to the test settings struct and assert the emitted kind. Mirror the existing FilePath test (search for `FilePath` in that file). Add:

```rust
    // A TemplateLibrary field emits SettingKind::TemplateLibrary with the same
    // filter_label/extensions plumbing as FilePath.
    let tl = defs
        .iter()
        .find(|d| d.field_name == "template_field")
        .expect("template_field def present");
    match &tl.kind {
        wc_core::settings::SettingKind::TemplateLibrary {
            filter_label,
            extensions,
        } => {
            assert_eq!(*filter_label, "Image");
            assert_eq!(*extensions, &["png", "jpg"]);
        }
        other => panic!("expected TemplateLibrary, got {other:?}"),
    }
```

and add the field to that test's settings struct:

```rust
    #[setting(
        default = String::new(),
        ty = TemplateLibrary,
        filter_label = "Image",
        extensions = ["png", "jpg"],
        category = User
    )]
    #[serde(default)]
    pub template_field: String,
```

- [ ] **Step 4: Run the macro test to verify it fails, then passes after the interim render arm**

Run: `cargo test -p wc-core-macros`
Expected: FAIL first if the variant/codegen is incomplete; PASS once Steps 1-2 are in.

- [ ] **Step 5: Add the interim render arm (compile-fix + graceful fallback)**

In `crates/wc-core/src/settings/panel_user.rs`, in `render_widget_value`'s match (after the `SettingKind::FilePath { .. } => { .. }` arm), add:

```rust
        SettingKind::TemplateLibrary {
            filter_label,
            extensions,
        } => {
            // Real library widget lands in a later task; until then (and when the
            // `templates` feature is off) render the plain file picker so the
            // field stays usable and the match is exhaustive.
            render_file_path(field, filter_label, extensions, ui);
        }
```

- [ ] **Step 6: Switch the Line field to the new kind**

In `crates/wc-sketches/src/line/settings.rs`, change the `spawn_template` attribute `ty = FilePath` to `ty = TemplateLibrary` (lines ~101). Leave `filter_label`, `extensions`, `section`, `category`, `requires_restart`, and the `#[serde(default)]` unchanged.

- [ ] **Step 7: Run tests + build**

Run: `cargo test -p wc-core-macros && cargo build -p wc-core --features templates && cargo build -p wc-sketches`
Expected: all pass/clean.

- [ ] **Step 8: Commit**

```bash
git add crates/wc-core/src/settings/def.rs crates/wc-core-macros/src/lib.rs crates/wc-core-macros/tests/derive.rs crates/wc-core/src/settings/panel_user.rs crates/wc-sketches/src/line/settings.rs
git commit -F - <<'EOF'
feat(settings): SettingKind::TemplateLibrary + macro support

New setting kind mirroring FilePath, wired through the derive macro. Interim
render delegates to the file picker (and is the permanent fallback when the
templates feature is off). Line's spawn_template opts into it.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

## Task 6: Line migration system (ingest external `spawn_template`)

**Files:**
- Create: `crates/wc-sketches/src/line/template_migrate.rs`
- Modify: `crates/wc-sketches/src/line/mod.rs` (add `mod`, register system in `OnEnter`)
- Modify: `crates/wc-sketches/Cargo.toml` (`templates` feature)

**Interfaces:**
- Consumes: `wc_core::templates::{templates_dir, store, resource::TemplateLibrary}`, `LineSettings`.
- Produces: `migrate_spawn_template` system, ordered `.before(systems::spawn_line)`.

- [ ] **Step 1: Add the wc-sketches `templates` feature**

In `crates/wc-sketches/Cargo.toml`, under `[features]` (after the `hand-tracking-gestures` line), add:

```toml
templates = ["wc-core/templates"]
```

- [ ] **Step 2: Write the failing migration test**

Create `crates/wc-sketches/src/line/template_migrate.rs` with the test module first:

```rust
#[cfg(all(test, feature = "templates"))]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use wc_core::templates::resource::TemplateLibrary;

    #[test]
    fn external_path_is_ingested_and_repointed() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("face.png");
        let mut img = image::RgbaImage::new(16, 16);
        for px in img.pixels_mut() {
            *px = image::Rgba([1, 2, 3, 255]);
        }
        img.save(&src).unwrap();

        let mut library = TemplateLibrary {
            dir: dir.path().to_path_buf(),
            entries: Vec::new(),
        };
        let mut path = src.to_string_lossy().into_owned();

        let changed = migrate_path(&mut path, &mut library);

        assert!(changed);
        assert!(wc_core::templates::store::is_managed(
            dir.path(),
            std::path::Path::new(&path)
        ));
        assert!(std::path::Path::new(&path).exists());
        assert_eq!(library.entries.len(), 1);
    }

    #[test]
    fn already_managed_path_is_left_alone() {
        let dir = TempDir::new().unwrap();
        let mut library = TemplateLibrary {
            dir: dir.path().to_path_buf(),
            entries: Vec::new(),
        };
        let managed = dir.path().join("abc.png").to_string_lossy().into_owned();
        let mut path = managed.clone();
        // Blob exists so it counts as managed-and-present.
        std::fs::write(dir.path().join("abc.png"), b"x").unwrap();

        let changed = migrate_path(&mut path, &mut library);

        assert!(!changed);
        assert_eq!(path, managed);
    }

    #[test]
    fn missing_external_path_is_left_for_ui_to_flag() {
        let dir = TempDir::new().unwrap();
        let mut library = TemplateLibrary {
            dir: dir.path().to_path_buf(),
            entries: Vec::new(),
        };
        let mut path = "/gone/forever.png".to_string();

        let changed = migrate_path(&mut path, &mut library);

        assert!(!changed);
        assert_eq!(path, "/gone/forever.png");
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p wc-sketches --features templates line::template_migrate`
Expected: FAIL — `cannot find function migrate_path`.

- [ ] **Step 4: Implement `migrate_path` + the system**

Prepend to `template_migrate.rs` (above the test module):

```rust
//! Migrates a Line `spawn_template` that points at an external (user) file into
//! the managed template store, so deleting the original no longer breaks the
//! template. Runs on `OnEnter(AppState::Line)` before `spawn_line`, and also
//! heals legacy configs from before the library existed.

#[cfg(feature = "templates")]
use bevy::prelude::*;

#[cfg(feature = "templates")]
use wc_core::templates::{resource::TemplateLibrary, store};

#[cfg(feature = "templates")]
use crate::line::settings::LineSettings;

/// Core migration logic on a path string, separated from Bevy for testing.
/// Returns `true` if `path` was rewritten to a managed blob (caller persists).
///
/// - empty, or already inside the store and present → leave (no-op);
/// - external path that exists → ingest, repoint to the managed blob, refresh
///   `library`;
/// - external path that is missing → leave (the UI surfaces "file missing").
#[cfg(feature = "templates")]
#[must_use]
fn migrate_path(path: &mut String, library: &mut TemplateLibrary) -> bool {
    if path.is_empty() {
        return false;
    }
    let p = std::path::Path::new(path.as_str());
    if store::is_managed(&library.dir, p) {
        return false;
    }
    if !p.exists() {
        return false;
    }
    match store::ingest(&library.dir, &p.to_path_buf()) {
        Ok(entry) => {
            *path = store::managed_path(&library.dir, &entry)
                .to_string_lossy()
                .into_owned();
            library.reload();
            true
        }
        Err(err) => {
            tracing::warn!(?err, "failed to ingest spawn_template into library");
            false
        }
    }
}

/// `OnEnter(AppState::Line)` system: migrate `spawn_template` before the sketch
/// samples it. Mutating `LineSettings` triggers change detection → autosave.
#[cfg(feature = "templates")]
pub fn migrate_spawn_template(
    mut settings: ResMut<LineSettings>,
    library: Option<ResMut<TemplateLibrary>>,
) {
    let Some(mut library) = library else {
        return;
    };
    let mut path = std::mem::take(&mut settings.spawn_template);
    let changed = migrate_path(&mut path, &mut library);
    settings.spawn_template = path;
    if !changed {
        // No write occurred; avoid a spurious change-detection tick.
        settings.bypass_change_detection();
    }
}
```

Note: `ResMut` always marks the resource changed on deref. To avoid a spurious `requires_restart` restart when nothing migrated, the no-op branch calls `bypass_change_detection()` after the write-back of the unchanged value. (If `bypass_change_detection` on the value already assigned is awkward, an equivalent: only take/replace `spawn_template` when `changed`. Prefer whichever compiles cleanly; both avoid the false restart.)

- [ ] **Step 5: Register the module + system**

In `crates/wc-sketches/src/line/mod.rs`:

(a) Add near the other `mod` declarations:

```rust
mod template_migrate;
```

(b) Change the `OnEnter(AppState::Line)` registration (currently `(systems::spawn_line, enter_line_audio)`) to run migration first:

```rust
        app.add_systems(
            OnEnter(AppState::Line),
            (
                #[cfg(feature = "templates")]
                template_migrate::migrate_spawn_template,
                systems::spawn_line,
                enter_line_audio,
            )
                .chain(),
        );
```

`.chain()` guarantees `migrate_spawn_template` repoints the path before `spawn_line` samples it. (`enter_line_audio` ordering is unaffected — it has no dependency on the others.)

- [ ] **Step 6: Run the test + build**

Run: `cargo test -p wc-sketches --features templates line::template_migrate`
Expected: PASS (3 tests).
Run: `cargo build -p wc-sketches --features templates`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/wc-sketches/Cargo.toml crates/wc-sketches/src/line/template_migrate.rs crates/wc-sketches/src/line/mod.rs
git commit -F - <<'EOF'
feat(line): migrate spawn_template into the managed template store

On entering Line, an external spawn_template path is ingested into the store
and repointed at the managed blob (before spawn samples it), so deleting the
original file no longer breaks the template. Legacy configs self-heal.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

## Task 7: View-model + select-from-cache ComboBox (names only)

**Files:**
- Modify: `crates/wc-core/src/templates/view.rs`
- Modify: `crates/wc-core/src/settings/panel_user.rs` (snapshot build, threading, widget, apply)

**Interfaces:**
- Produces:
  - `view::TemplateRow { hash, label, subtext, managed_path, thumb: Option<egui::TextureId> }`.
  - `view::build_rows(lib: &TemplateLibrary) -> Vec<TemplateRow>` (sorted newest-first; `thumb = None` here — Task 9 fills it).
  - `view::human_bytes(n: u64) -> String`.
  - `panel_user`: snapshot built before the Area closure, threaded through `render_section_by_key` / `render_user_fields_via_reflect` / `render_widget_value`; a `template_dirty: &mut bool` out-flag; apply (library reload) after the closure.

- [ ] **Step 1: Write the failing view-model tests**

Replace the placeholder in `crates/wc-core/src/templates/view.rs` with a test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates::manifest::TemplateEntry;
    use crate::templates::resource::TemplateLibrary;

    fn entry(hash: &str, name: &str, at: u64, w: u32, h: u32, bytes: u64) -> TemplateEntry {
        TemplateEntry {
            hash: hash.into(),
            ext: "png".into(),
            original_name: name.into(),
            imported_at: at,
            width: w,
            height: h,
            bytes,
            thumb: format!("thumbs/{hash}.png"),
        }
    }

    #[test]
    fn rows_sort_newest_first_with_subtext() {
        let lib = TemplateLibrary {
            dir: std::path::PathBuf::from("/store"),
            entries: vec![
                entry("old", "old.png", 100, 640, 480, 1024),
                entry("new", "new.png", 200, 1280, 853, 482133),
            ],
        };
        let rows = build_rows(&lib);
        assert_eq!(rows[0].label, "new.png");
        assert_eq!(rows[0].subtext, "1280x853 · 471 KB");
        assert_eq!(rows[1].label, "old.png");
        assert_eq!(rows[0].managed_path, "/store/new.png");
    }

    #[test]
    fn human_bytes_scales() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1 KB");
        assert_eq!(human_bytes(482133), "471 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p wc-core --features templates templates::view`
Expected: FAIL — `cannot find function build_rows`.

- [ ] **Step 3: Implement the view-model**

Prepend to `view.rs` (above the test module):

```rust
//! Pure (egui-free) view-model for the template-library widget: turns manifest
//! entries into display rows. Thumbnail texture ids are attached later by the
//! panel (see `settings::panel_user`), so this module stays unit-testable.

use bevy_egui::egui;

use crate::templates::resource::TemplateLibrary;
use crate::templates::store::managed_path;

/// One row in the template dropdown.
#[derive(Clone, Debug)]
pub struct TemplateRow {
    /// Content hash (stable id for selection, delete, thumbnail lookup).
    pub hash: String,
    /// Friendly label (the original filename).
    pub label: String,
    /// `"WxH · <size>"` subtext.
    pub subtext: String,
    /// Absolute path to the managed blob, written into the setting on select.
    pub managed_path: String,
    /// Thumbnail texture id, filled by the panel once decoded; `None` until then.
    pub thumb: Option<egui::TextureId>,
}

/// Build display rows from the library, sorted newest-import first.
#[must_use]
pub fn build_rows(lib: &TemplateLibrary) -> Vec<TemplateRow> {
    let mut entries: Vec<_> = lib.entries.iter().collect();
    entries.sort_by(|a, b| b.imported_at.cmp(&a.imported_at));
    entries
        .into_iter()
        .map(|e| TemplateRow {
            hash: e.hash.clone(),
            label: e.original_name.clone(),
            subtext: format!("{}x{} · {}", e.width, e.height, human_bytes(e.bytes)),
            managed_path: managed_path(&lib.dir, e).to_string_lossy().into_owned(),
            thumb: None,
        })
        .collect()
}

/// Human-readable byte size (`"512 B"`, `"471 KB"`, `"5.0 MB"`).
#[must_use]
pub fn human_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    if n < KB {
        format!("{n} B")
    } else if n < MB {
        format!("{} KB", n / KB)
    } else {
        format!("{:.1} MB", n as f64 / MB as f64)
    }
}
```

- [ ] **Step 4: Run the view-model tests**

Run: `cargo test -p wc-core --features templates templates::view`
Expected: PASS (2 tests).

- [ ] **Step 5: Thread the snapshot + dirty flag through the panel**

In `crates/wc-core/src/settings/panel_user.rs`:

(a) Add the snapshot build after the egui context clone and `state.apply(world)` (after line 229), before the `egui::Area::new(...)` call:

```rust
    // Snapshot the template library into display rows before the Area closure
    // (where `world` is no longer reachable). Empty when the feature is off.
    #[cfg(feature = "templates")]
    let template_rows: Vec<crate::templates::view::TemplateRow> =
        template_library_rows(world, &ctx);
    #[cfg(not(feature = "templates"))]
    let template_rows: Vec<()> = Vec::new();
    // Set true inside the closure when an import/delete changed the store, so we
    // reload the library resource after the closure releases the world borrow.
    let mut template_dirty = false;
```

(b) Change `render_section_by_key`'s signature (line 372) and `render_user_fields_via_reflect`'s signature (line 471) to thread the rows + dirty flag. Add these two parameters to BOTH function signatures (after `provider_status`):

```rust
    #[cfg(feature = "templates")] template_rows: &[crate::templates::view::TemplateRow],
    template_dirty: &mut bool,
```

For the non-`templates` build, `render_template_library` is never called, so `template_rows` is unused there — gate the parameter with the same `#[cfg]`. Pass them at the call site inside the `ScrollArea` closure (line 551 area):

```rust
                                    render_section_by_key(
                                        world,
                                        ui,
                                        key,
                                        provider_status,
                                        #[cfg(feature = "templates")]
                                        &template_rows,
                                        &mut template_dirty,
                                        advanced,
                                        &style,
                                    );
```

and forward them from `render_section_by_key` into `render_user_fields_via_reflect`, and from there into `render_widget_value` (extend `render_widget_value`'s signature the same way).

(c) Replace the interim `SettingKind::TemplateLibrary` arm in `render_widget_value` with:

```rust
        SettingKind::TemplateLibrary {
            filter_label,
            extensions,
        } => {
            #[cfg(feature = "templates")]
            render_template_library(
                field,
                filter_label,
                extensions,
                storage_key,
                def.field_name,
                template_rows,
                template_dirty,
                &style_for_template(ui),
                ui,
            );
            #[cfg(not(feature = "templates"))]
            render_file_path(field, filter_label, extensions, ui);
        }
```

(d) After the `egui::Area` `.show(...)` closure returns (around line 284, alongside the `SettingsDockTab`/`SettingsDockAdvanced` write-back), apply the dirty flag:

```rust
    #[cfg(feature = "templates")]
    if template_dirty {
        if let Some(mut lib) = world.get_resource_mut::<crate::templates::resource::TemplateLibrary>() {
            lib.reload();
        }
    }
```

(e) Add the snapshot helper (names-only for now; Task 9 fills thumbnails). Place near the other free functions:

```rust
    #[cfg(feature = "templates")]
    fn template_library_rows(
        world: &mut World,
        _ctx: &egui::Context,
    ) -> Vec<crate::templates::view::TemplateRow> {
        world
            .get_resource::<crate::templates::resource::TemplateLibrary>()
            .map(crate::templates::view::build_rows)
            .unwrap_or_default()
    }
```

(f) Add the widget. The style accessor `style_for_template` just returns the `OverlayStyle` already in scope; if `render_widget_value` already has `&OverlayStyle` available, pass it directly instead of adding a helper — adjust to match the real call chain (the reset cell already receives `style: &OverlayStyle`, so thread `style` into `render_widget_value` if it isn't already). The names-only widget:

```rust
#[cfg(feature = "templates")]
#[allow(clippy::too_many_arguments)]
fn render_template_library(
    field: &mut dyn bevy::reflect::PartialReflect,
    filter_label: &str,
    extensions: &[&str],
    storage_key: &'static str,
    field_name: &'static str,
    rows: &[crate::templates::view::TemplateRow],
    _dirty: &mut bool,
    style: &OverlayStyle,
    ui: &mut egui::Ui,
) {
    let _ = (filter_label, extensions, style); // used in Task 8/9
    let Some(v) = field.try_downcast_mut::<String>() else {
        ui.label("(expected String for template path)");
        return;
    };

    // Closed-state label: the active template's friendly name, or "(none)".
    let selected_text = rows
        .iter()
        .find(|r| r.managed_path == *v)
        .map_or("(none)", |r| r.label.as_str())
        .to_owned();

    egui::ComboBox::from_id_salt(("wc-template-lib", storage_key, field_name))
        .selected_text(selected_text)
        .height(280.0)
        .show_ui(ui, |ui| {
            for row in rows {
                let is_sel = row.managed_path == *v;
                if ui.selectable_label(is_sel, &row.label).clicked() {
                    *v = row.managed_path.clone();
                    ui.close();
                }
            }
        });
}
```

- [ ] **Step 6: Build + run the full wc-core test set**

Run: `cargo build -p wc-core --features templates && cargo test -p wc-core --features templates`
Expected: clean build, all tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/wc-core/src/templates/view.rs crates/wc-core/src/settings/panel_user.rs
git commit -F - <<'EOF'
feat(templates): select-from-cache dropdown + panel snapshot plumbing

build_rows view-model (sorted newest-first, WxH + human size subtext) and a
ComboBox that selects a cached template into the setting. Threads the library
snapshot + dirty flag through the panel render chain.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

## Task 8: Import row + delete with inline confirm

**Files:**
- Modify: `crates/wc-core/src/settings/panel_user.rs` (`render_template_library`)

**Interfaces:**
- Consumes: `templates::store::{ingest, delete, managed_path}`, the `dir` from `TemplateLibrary` (via the rows' `managed_path` parent — see note).

Note: ingest/delete need the store `dir`. Pass it into the widget by adding a `dir: &std::path::Path` parameter (snapshot it from `TemplateLibrary.dir` next to `template_rows`). Update `template_library_rows`/threading to also carry `dir`, or include it on a small snapshot struct. Simplest: snapshot `let template_dir = world.get_resource::<TemplateLibrary>().map(|l| l.dir.clone());` before the closure and thread `Option<&Path>` alongside the rows.

- [ ] **Step 1: Add the import + delete UI to `render_template_library`**

Extend the `show_ui` closure body. The full updated function:

```rust
#[cfg(feature = "templates")]
#[allow(clippy::too_many_arguments)]
fn render_template_library(
    field: &mut dyn bevy::reflect::PartialReflect,
    filter_label: &str,
    extensions: &[&str],
    storage_key: &'static str,
    field_name: &'static str,
    dir: Option<&std::path::Path>,
    rows: &[crate::templates::view::TemplateRow],
    dirty: &mut bool,
    style: &OverlayStyle,
    ui: &mut egui::Ui,
) {
    use crate::templates::store;

    let Some(v) = field.try_downcast_mut::<String>() else {
        ui.label("(expected String for template path)");
        return;
    };

    let selected_text = rows
        .iter()
        .find(|r| r.managed_path == *v)
        .map_or("(none)", |r| r.label.as_str())
        .to_owned();

    // Which hash (if any) is mid delete-confirm; persisted in egui memory so it
    // survives frames without a Bevy resource.
    let confirm_id = egui::Id::new(("wc-template-confirm", storage_key, field_name));

    egui::ComboBox::from_id_salt(("wc-template-lib", storage_key, field_name))
        .selected_text(selected_text)
        .height(280.0)
        .show_ui(ui, |ui| {
            // Pinned import row.
            let import = egui::RichText::new(format!("{}  Import image…", phosphor::PLUS))
                .family(egui::FontFamily::Name("phosphor".into()));
            if ui.selectable_label(false, import).clicked() {
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(dir) = dir {
                    let mut dlg = rfd::FileDialog::new();
                    if !extensions.is_empty() {
                        dlg = dlg.add_filter(filter_label, extensions);
                    }
                    if let Some(path) = dlg.pick_file() {
                        match store::ingest(dir, &path) {
                            Ok(entry) => {
                                *v = store::managed_path(dir, &entry)
                                    .to_string_lossy()
                                    .into_owned();
                                *dirty = true;
                            }
                            Err(err) => tracing::warn!(?err, "template import failed"),
                        }
                    }
                }
                ui.close();
            }
            ui.separator();

            if rows.is_empty() {
                ui.label(egui::RichText::new("No templates yet").italics().color(style.text_faint));
            }

            let mut confirm: Option<String> =
                ui.memory(|m| m.data.get_temp(confirm_id)).unwrap_or(None);

            for row in rows {
                ui.horizontal(|ui| {
                    if confirm.as_deref() == Some(row.hash.as_str()) {
                        // Inline two-step delete confirm.
                        ui.label(
                            egui::RichText::new(format!("Delete \"{}\"?", row.label))
                                .color(style.error_red),
                        );
                        if ui
                            .button(egui::RichText::new("Delete").color(style.error_red))
                            .clicked()
                        {
                            if let Some(dir) = dir {
                                if let Err(err) = store::delete(dir, &row.hash) {
                                    tracing::warn!(?err, "template delete failed");
                                }
                                // Clear the field if the active template was deleted.
                                if *v == row.managed_path {
                                    v.clear();
                                }
                                *dirty = true;
                            }
                            confirm = None;
                        }
                        if ui.button("Cancel").clicked() {
                            confirm = None;
                        }
                    } else {
                        let is_sel = row.managed_path == *v;
                        ui.vertical(|ui| {
                            if ui.selectable_label(is_sel, &row.label).clicked() {
                                *v = row.managed_path.clone();
                                ui.close();
                            }
                            ui.label(
                                egui::RichText::new(&row.subtext)
                                    .size(10.0)
                                    .color(style.text_faint),
                            );
                        });
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                let trash = egui::RichText::new(phosphor::TRASH)
                                    .family(egui::FontFamily::Name("phosphor".into()))
                                    .color(style.text_secondary);
                                if ui
                                    .add(egui::Button::new(trash).frame(false))
                                    .on_hover_text("Delete from cache")
                                    .clicked()
                                {
                                    confirm = Some(row.hash.clone());
                                }
                            },
                        );
                    }
                });
            }

            ui.memory_mut(|m| m.data.insert_temp(confirm_id, confirm));
        });
}
```

Update the call in `render_widget_value` to pass `dir` (the snapshotted `Option<&Path>`) and thread `template_dir` through the same chain as `template_rows`.

- [ ] **Step 2: Build + clippy**

Run: `cargo clippy -p wc-core --features templates --all-targets -- -D warnings`
Expected: clean (the `#[allow(clippy::too_many_arguments)]` covers the wide signature).

- [ ] **Step 3: Manual verification (rendering can't be unit-tested)**

Run: `cargo rund`. Open the settings dock (Shift+D), go to the Line section's Spawn group. Verify: the template field is now a dropdown; "Import image…" opens a file dialog; importing an image selects it and the art updates; the trash icon shows the inline "Delete \"name\"? [Delete] [Cancel]" confirm; deleting the active template clears it and the art falls back to the default line.

- [ ] **Step 4: Commit**

```bash
git add crates/wc-core/src/settings/panel_user.rs
git commit -F - <<'EOF'
feat(templates): import row + inline delete confirm in the dropdown

Pinned "Import image…" ingests via the file dialog and selects; per-row trash
button flips to an inline two-step confirm; deleting the active template clears
the field (sketch falls back to the default line). Marks the snapshot dirty so
the library reloads after the panel closure.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

## Task 9: Thumbnails + honest status

**Files:**
- Modify: `crates/wc-core/src/settings/panel_user.rs` (`template_library_rows` fills thumbnails; rows render the image; missing-status text)

**Interfaces:**
- Consumes: `TemplateThumbnailCache`, `TemplateLibrary`, egui `Context::load_texture`, `image` decode.

- [ ] **Step 1: Fill thumbnail texture ids in the snapshot helper**

Replace `template_library_rows` with a version that lazily decodes each thumbnail once and caches the `TextureHandle`:

```rust
#[cfg(feature = "templates")]
fn template_library_rows(
    world: &mut World,
    ctx: &egui::Context,
) -> Vec<crate::templates::view::TemplateRow> {
    use crate::templates::resource::{TemplateLibrary, TemplateThumbnailCache};

    let Some(lib) = world.get_resource::<TemplateLibrary>() else {
        return Vec::new();
    };
    let dir = lib.dir.clone();
    let mut rows = crate::templates::view::build_rows(lib);

    // Decode + register any thumbnails not already cached (one-time per session).
    let needed: Vec<(String, std::path::PathBuf)> = {
        let cache = world.resource::<TemplateThumbnailCache>();
        lib.entries
            .iter()
            .filter(|e| !cache.0.contains_key(&e.hash))
            .map(|e| (e.hash.clone(), dir.join(&e.thumb)))
            .collect()
    };
    for (hash, thumb_path) in needed {
        if let Some(handle) = load_thumb_texture(ctx, &hash, &thumb_path) {
            world
                .resource_mut::<TemplateThumbnailCache>()
                .0
                .insert(hash, handle);
        }
    }
    // Drop cache entries whose template was deleted, freeing their GPU textures.
    {
        let live: std::collections::HashSet<&str> =
            world.resource::<TemplateLibrary>().entries.iter().map(|e| e.hash.as_str()).collect_to_set();
        world
            .resource_mut::<TemplateThumbnailCache>()
            .0
            .retain(|h, _| live.contains(h.as_str()));
    }
    // Attach texture ids.
    let cache = world.resource::<TemplateThumbnailCache>();
    for row in &mut rows {
        row.thumb = cache.0.get(&row.hash).map(egui::TextureHandle::id);
    }
    rows
}

/// Decode a baked thumbnail PNG and upload it as an egui texture. `None` on any
/// read/decode failure (the row then renders without a thumbnail).
#[cfg(feature = "templates")]
fn load_thumb_texture(
    ctx: &egui::Context,
    hash: &str,
    path: &std::path::Path,
) -> Option<egui::TextureHandle> {
    let img = image::open(path).ok()?.to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let color = egui::ColorImage::from_rgba_unmultiplied([w, h], img.as_raw());
    Some(ctx.load_texture(format!("wc-tpl-thumb-{hash}"), color, egui::TextureOptions::LINEAR))
}
```

Note: replace `collect_to_set()` with an inline `HashSet` build if no such helper exists:
```rust
        let live: std::collections::HashSet<String> =
            world.resource::<TemplateLibrary>().entries.iter().map(|e| e.hash.clone()).collect();
```
and adjust the `retain` predicate to `live.contains(h)`.

- [ ] **Step 2: Render the thumbnail in each row**

In `render_template_library`, inside the per-row `ui.horizontal(|ui| { ... })`, before the `ui.vertical(...)` label block, add:

```rust
                        if let Some(tid) = row.thumb {
                            ui.add(
                                egui::Image::new(egui::load::SizedTexture::new(
                                    tid,
                                    egui::vec2(40.0, 40.0),
                                ))
                                .fit_to_exact_size(egui::vec2(40.0, 40.0)),
                            );
                        }
```

- [ ] **Step 3: Add the missing-active-template status**

In `render_template_library`, after computing `selected_text` but before the `ComboBox`, add a missing-file note under the row when the active path is non-empty and absent on disk:

```rust
    // Honest status: a non-empty active path whose blob is gone reads as missing.
    let active_missing = !v.is_empty() && !std::path::Path::new(v.as_str()).exists();
```

and after the `ComboBox::...show_ui(...)` call:

```rust
    if active_missing {
        ui.label(
            egui::RichText::new("file missing, using default")
                .size(10.0)
                .color(style.warn_amber),
        );
    }
```

- [ ] **Step 4: Build + clippy + the wc-core suite**

Run: `cargo clippy -p wc-core --features templates --all-targets -- -D warnings && cargo test -p wc-core --features templates`
Expected: clean.

- [ ] **Step 5: Manual verification**

Run: `cargo rund`. In the Line spawn settings: the dropdown rows show 40px thumbnails next to the names; importing two same-named files shows both, distinguishable by thumbnail; the closed dropdown shows the active name; delete the managed blob out-of-band (or the original before migration) and confirm the "file missing, using default" note appears and the sketch still renders the default line.

- [ ] **Step 6: Commit**

```bash
git add crates/wc-core/src/settings/panel_user.rs
git commit -F - <<'EOF'
feat(templates): dropdown thumbnails + honest missing-file status

Baked thumbnails decode once into session-lived egui textures (cache pruned on
delete); each row shows a 40px preview. The active template's name shows in the
closed dropdown, and a missing active blob reads as "file missing, using default".

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

## Task 10: Full verification + soak-safety check

**Files:** none (verification only; small doc touch if needed).

- [ ] **Step 1: Run the full CI gate set with all features**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
cargo doc --no-deps --workspace --document-private-items
cargo deny check
cargo xtask check-secrets
```
Expected: all green (the ~29 pre-existing doc-link warnings noted in `AGENTS.md` are non-fatal).

- [ ] **Step 2: Confirm web build is unaffected**

The `templates` feature is native-only and off by default. Confirm a default (no-templates) build still compiles and the Line field renders as a plain file picker:
```bash
cargo build -p wc-core
cargo build -p wc-sketches
```
Expected: clean; no `templates`-gated code compiled.

- [ ] **Step 3: End-to-end manual smoke (the original bug)**

Run: `cargo rund`. Reproduce the reported scenario:
1. Import an image template → particles form it.
2. Quit, delete the original file from disk, relaunch.
3. Confirm the template still renders (it's served from the managed blob), and the dropdown still lists it with its thumbnail.
4. Import a second image, switch between the two via the dropdown, delete one, confirm the other is unaffected.

- [ ] **Step 4: Idle/soak sanity**

Confirm no per-frame allocation regressions: the thumbnail decode happens only when the dock is open and only once per template (cached). With the dock closed, no template systems run beyond the migration on Line entry. (Spot-check by leaving the app idle into screensaver and watching for steady memory.)

- [ ] **Step 5: Final commit (if any doc tweaks)**

If `AGENTS.md` or a README mentions the template path behavior, update it to describe the library. Otherwise no commit needed — the feature is complete.

---

## Self-Review Notes

- **Spec coverage:** store + manifest (Tasks 1-3), resources + startup reconcile (Task 4), `SettingKind::TemplateLibrary` + macro (Task 5), legacy migration (Task 6), select dropdown (Task 7), import + delete inline confirm (Task 8), thumbnails + honest status (Task 9), verification (Task 10). All spec sections map to a task.
- **blake3 / build-time constraint:** honored — `blake3` already in the graph via `bevy_asset`; no other new deps.
- **Native-only / wasm:** the whole feature is `#[cfg(feature = "templates")]` with a `render_file_path` fallback; Task 10 Step 2 verifies the default build.
- **Ambiguity resolved:** `migrate_path` logic is split from Bevy for unit testing; the dropdown's confirm-delete state lives in egui memory; the dirty flag drives a single post-closure library reload.
