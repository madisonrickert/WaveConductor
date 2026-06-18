# Line Sketch — Image Template Library (cache + picker)

**Date:** 2026-06-18
**Status:** Design approved, pending spec review
**Sketch:** Line
**Scope:** One of three independent workstreams from 2026-06-18 testing feedback
(the other two — screensaver feel/dimming, psychedelic color — are separate specs).

## Problem

The Line sketch loads an image as a *template* (particles arrange to form the
image). Today the template is referenced only by an on-disk file path stored in
`LineSettings::spawn_template: String`. The image is re-read from that path on
every sketch entry via `image::open(path)` in `heatmap.rs`. Nothing is cached.

Consequences observed in testing (2026-06-18):

- Deleting or moving the original file silently breaks the template. The sketch
  falls back to the default horizontal line, but the settings row still shows the
  filename, so the UI reads as "loaded" when it is not.
- There is no way to reuse a previously-loaded image, and no way to manage
  (delete) old images.

## Goal

Turn the template path into a persistent, app-managed **template library**:

1. Loading an image copies it into an app-owned store, so the displayed template
   survives deletion/moving of the original.
2. A dropdown lets the user select any previously-cached template.
3. The user can delete old images from the cache.
4. The UI honestly reflects template state (active name, or "missing").

The unrecoverable case is acknowledged: the template lost on 2026-06-17 was never
cached and cannot be restored. This feature is forward-looking; the user re-picks
that image once and it is safe thereafter.

## Decisions (from brainstorming)

- **Presentation:** an enriched `ComboBox` row in the existing settings dock — not
  a separate window. Fits the dock's density and the repo's no-`egui::Window`
  convention (the dock already uses `egui::Area` + backdrop blur; `egui::Window`
  was deliberately removed, see `panel_dev.rs:11`).
- **Thumbnails:** included in v1. In a visual tool the image is its identity, and
  same-named files (`download.png`) are otherwise indistinguishable.
- **UI scope:** honest status folds into the combobox (active name / "(none)" /
  "file missing, using default").
- **Delete confirmation:** inline two-step confirm in the row, no modal.

## Environment (confirmed)

- egui `0.33.3`, epaint `0.33.3`, bevy_egui `0.39.1` (workspace `Cargo.toml`).
- egui APIs verified for this version: `ComboBox::from_id_salt / selected_text /
  show_ui / height`, `Context::load_texture -> TextureHandle`, `egui::Image::new`
  from `(TextureId, Vec2)`, `ColorImage::from_rgba_unmultiplied`,
  `selectable_label`, frameless `egui::Button::new(..).frame(false)`, `ui.close()`.
- Settings panel is reflection-driven: `draw_user_panel` (exclusive `World`
  system) → `render_section_by_key` → `render_user_fields_via_reflect` → 3-column
  `egui::Grid` → `render_widget_value` dispatches on `SettingKind`
  (`panel_user.rs:760-781`).
- Existing texture-into-egui precedent: `picker.rs` registers a texture before the
  egui closure and paints it inside (`picker.rs:99-124`). The settings panel uses
  a snapshot-before-closure / apply-after-closure pattern for world data
  (`panel_user.rs:198`, `547-558`, `286-291`).
- Heatmap already fails soft: a missing/undecodable template falls back to the
  horizontal-line layout and logs a warning (`heatmap.rs:55-67`). Load-bearing for
  the delete-active-template flow.

## Architecture

The widget lives in the generic settings UI (`wc-core`), so the store and library
resources also live in `wc-core`. Line is the first (currently only) consumer; it
opts its field into the library widget via a setting attribute. The capability is
parameterized (accepted extensions, filter label come from the field attributes),
not Line-hardcoded.

### Module placement

- **`crates/wc-core/src/templates/`** (new) — the image template library:
  - `store.rs` — filesystem store: dir resolution, content-hash, ingest, delete,
    thumbnail baking, reconcile. No `unwrap`/`expect`; returns `Result`.
  - `manifest.rs` — `manifest.toml` load/save + the entry model.
  - `resource.rs` — `TemplateLibrary` (in-memory manifest) and
    `TemplateThumbnailCache(HashMap<Hash, egui::TextureHandle>)` Bevy resources.
  - `mod.rs` — `//!` module doc, public re-exports.
- **`crates/wc-core/src/settings/def.rs`** — add `SettingKind::TemplateLibrary {
  filter_label, extensions }` (mirrors the existing `FilePath` variant).
- **`crates/wc-core/src/settings/panel_user.rs`** — snapshot builder, the
  `render_template_library` widget, and apply-after-closure action handling.
- **`crates/wc-sketches/src/line/settings.rs`** — change `spawn_template` from
  `ty = FilePath` to `ty = TemplateLibrary`. Field stays `String` (managed path).
- **`crates/wc-sketches/src/line/heatmap.rs` / `systems/spawn.rs`** — unchanged;
  they receive the managed path string and read it as before.

### On-disk layout

```
dirs::data_dir()/waveconductor/templates/
  manifest.toml
  <hash>.<ext>            # raw original bytes (faithful copy)
  thumbs/<hash>.png       # ~96px-longest-edge preview, baked at import
```

- Blobs are app **data** (potentially large), so they live under `data_dir()`,
  distinct from the small `config_dir()` settings file. Add a `templates_dir()`
  helper honoring a `WAVECONDUCTOR_DATA_DIR` override (paralleling the existing
  `WAVECONDUCTOR_CONFIG_DIR`) so tests use a `TempDir` and never touch the real
  cache. No hardcoded paths (AGENTS.md).
- **Hashing:** `blake3` content hash of the source bytes
  (`blake3::hash(&bytes).to_hex()`) is the blob filename stem and the manifest
  key, giving idempotent dedup. Chosen because it is already in the **normal
  runtime build graph** (via `bevy_asset`), so adding it to `wc-core`'s manifest
  links the existing rlib at zero extra compile cost; it is the canonical
  content-addressed-store hash (collision-free at any scale, which future-proofs
  generalizing templates to other sketches); and it is deterministic/stable across
  Rust versions and platforms, so dedup keeps working long-term. The full 64-char
  hex is used directly (no truncation). `twox-hash` (XXH3, also already in the
  graph via `bevy_image`) is an equally-zero-cost non-cryptographic alternative;
  `crc32fast` is the lighter 32-bit fallback. The randomized hashmap hashes
  (`ahash`/`foldhash`/`rustc-hash`) are unsuitable — they are version-unstable and
  would break persistent dedup.
- Blobs store the **raw original** bytes; the sampler downscales to 256px at spawn
  time (`heatmap.rs`), and thumbnails are baked separately at 96px. No re-encode.

### Manifest format (TOML)

```toml
[[template]]
hash = "9f3c..."
ext = "png"
original_name = "portrait-of-a-wave.png"   # friendly label
imported_at = "2026-06-18T12:00:00Z"        # sort newest-first
width = 1280                                 # subtext "1280x853"
height = 853
bytes = 482133                               # subtext "471 KB"
thumb = "thumbs/9f3c....png"
```

TOML matches the existing settings persistence (`persistence.rs` already depends
on `toml`). Loaded once into the `TemplateLibrary` resource at startup; rewritten
on import/delete. Not a hot path, so serialize allocations are acceptable.

### Store API (sketch of responsibilities)

- `templates_dir() -> PathBuf` — resolve + create the managed dir.
- `ingest(source: &Path) -> Result<TemplateEntry>` — read bytes, hash, copy blob
  if absent, capture dimensions, bake + write thumbnail, append/refresh manifest
  entry, save manifest. Idempotent on duplicate bytes.
- `delete(hash) -> Result<()>` — remove blob, thumbnail, and manifest entry; save.
- `reconcile()` — on startup, drop manifest entries whose blob is missing.
- `is_managed(path) -> bool` — true when a path already lives in the store (so
  legacy migration never re-ingests our own copy).
- `managed_path(entry) -> PathBuf` — absolute path written into `spawn_template`.

### Legacy / migration path — dropped (2026-06-18)

An earlier draft auto-migrated an external `spawn_template` into the store on Line
entry. **Dropped during implementation:** the app is pre-release with a single
operator whose only external template was already deleted (so the migration would
no-op on the real config), and a permanent backward-compat migration shim is
unwanted churn in active development. The transition is covered instead by the
honest-status UI (active name / "(none)" / "file missing") plus re-importing via
the Import button, which caches immediately. The startup **reconcile** (pruning
manifest entries whose blob is missing) is kept — it is store hygiene, not
migration.

## Interaction flows

**Select.** Open the combobox; rows are sorted newest-first, each showing
`[48px thumb] original_name` with `1280x853 - 471 KB` subtext. Clicking the label
writes the managed path into `spawn_template` through the same reflected field
handle the rest of the panel uses, so Bevy change detection, autosave, and
`requires_restart` (the field is `requires_restart`) fire identically — the sketch
re-runs `spawn_line` with the new template. The popup closes on select
(`ui.close()`). The closed combobox shows the active template's `original_name`,
or "(none)".

**Import a new image.** A pinned "＋ Import image…" row at the top of the dropdown
opens the existing `rfd::FileDialog` filtered to the field's `extensions`. On pick:
ingest (hash, copy, bake thumb, manifest) then select it. Import = add-to-cache
**and** select, one action. Re-importing identical bytes dedups and reselects.

**Delete.** Each row has a trailing frameless trash button (reusing the reset-glyph
idiom, `panel_user.rs:639-647`, with `phosphor::TRASH` and the error color on
hover). Clicking it does **not** select and does **not** close the popup; it flips
the row into an inline confirm: `Delete "name"?  [Delete] [Cancel]`. Confirm
removes blob + thumb + manifest entry. If the deleted hash is the active template,
clear `spawn_template` to `""` (fires restart); the sketch falls back to the
default line via the existing heatmap soft-fail. No dead end; deletion of the
active item is handled, not blocked.

**Honest status.** Active name in the closed combobox; if the active blob is gone,
the row reads "file missing, using default".

## Thumbnails (egui-native)

1. **At import:** bake a 96px-longest-edge thumbnail with the `image` crate, write
   `thumbs/<hash>.png`. Caps decode + GPU cost regardless of source resolution.
2. **Lazily on first dropdown open:** for each visible entry without a live handle,
   read the thumb, `ColorImage::from_rgba_unmultiplied([w,h], &rgba)`, then
   `ctx.load_texture("tpl-thumb-<hash>", img, TextureOptions::LINEAR) ->
   TextureHandle`. Store handles in `TemplateThumbnailCache` (session-lived).
3. **Render:** `ui.add(egui::Image::new((handle.id(), vec2(48,48)))
   .fit_to_exact_size(vec2(48,48)))`.
4. **Lifecycle:** egui frees the GPU texture when the `TextureHandle` drops;
   deleting an entry removes it from the cache. Build-once-and-cache keeps this off
   the per-frame hot path (decode happens once per template per session on a
   user-triggered open) — consistent with AGENTS.md's no-alloc-in-hot-path rule.

Use the egui-native `load_texture` path (we already own the bytes), not the
`asset_server.load` + `EguiUserTextures::add_image` path from `picker.rs` (blobs
live outside Bevy's asset root, so that path would need a custom asset source).

## Settings integration

Add `SettingKind::TemplateLibrary { filter_label, extensions }` to `def.rs`
(mirroring `FilePath`). The variant is the **marker**; the actual draw is the
special-cased `render_template_library`, which receives a pre-closure snapshot.
The generic reflection grid cannot reach the manifest/textures or perform file
mutations, so the data is threaded the same way the provider-status row already is
(`panel_user.rs:198`, `547-558`), and the action is applied after the `Area`
closure (`panel_user.rs:286-291`). The field stays a `String` path on disk, so
serde/restart semantics are unchanged. The row keeps its reset glyph (reset clears
to `""` → default line, handled by the heatmap fallback).

Widget returns `enum TemplateAction { Select(Hash), Delete(Hash), Import }`,
applied after the closure releases the world borrow. Inline confirm state
(`Option<Hash>`) lives in a small dock resource (like `SettingsDockTab`).

## Out of scope (noted for later)

- Cache GC/eviction beyond manual delete (content-hashed single images are small;
  dedup bounds growth).
- A standalone full-screen "template manager" window (revisit only if the library
  outgrows a dropdown).
- Generalizing the store to non-image files (the capability is image-specific:
  thumbnails, dimensions).

## Testing

Colocated `#[cfg(test)] mod tests`, using a `TempDir` via `WAVECONDUCTOR_DATA_DIR`:

- Content hash is stable and dedups identical bytes.
- `ingest` copies blob, bakes a thumb, writes a manifest entry; returns a managed
  path.
- Re-ingesting identical bytes does not duplicate the blob or entry (idempotent).
- `delete` removes blob + thumb + manifest entry.
- Deleting the active template clears `spawn_template`.
- `is_managed` is true for store paths, false for external paths.
- `ingest` of a missing/unreadable source returns `Err` and leaves settings
  untouched.
- `reconcile` drops manifest entries whose blob is missing.
- Manifest load/save round-trips; a missing/corrupt manifest yields an empty
  library with a warning, not a panic.

## Risks

- **egui 0.33 pitfalls:** `from_id_salt` (not `from_id_source`); a `Button` click
  inside a `ComboBox` popup keeps it open (good for trash) while
  `selectable_label` closes it (good for select) — verify the trash never
  registers as a selection; cap the dropdown with `ComboBox::height` so the inner
  `ScrollArea` only engages past the cap; `TextureHandle` must outlive the frame
  (store in the resource) or the image renders blank.
- **bevy_egui borrow ordering:** texture registration and world reads must happen
  before the `egui::Area` closure (the dock already clones the `EguiContext` Arc
  and releases the `SystemState` borrow first); follow that ordering.
- **Blob deleted out-of-band:** thumb decode failure renders a placeholder glyph,
  not a panic; missing active blob falls back via the heatmap soft-fail; startup
  `reconcile` prunes stale entries.
