# Line Template Adjustments — Design Spec

**Date:** 2026-06-18
**Status:** Approved design, ready for implementation plan
**Related:** [2026-06-18-line-template-library-design.md](2026-06-18-line-template-library-design.md) (the picker this builds on)

## Goal

Give the Line sketch's image template seven per-image tuning knobs that control how the image shapes the particle spawn, persisted per image: **white point, black point, invert, gamma, position (x, y), scale (x, y), and color influence (0–100%)**.

## Context: how the template feeds the spawn today

`LineSettings.spawn_template` holds the managed-blob path of the active image (empty = no template). At spawn (`crates/wc-sketches/src/line/systems/spawn.rs::spawn_line`), a non-empty path is handed to `sample_from_heatmap` (`crates/wc-sketches/src/line/heatmap.rs`), which:

1. Decodes the image, downsamples to a ≤256px grid.
2. Computes a per-pixel weight `luminance × alpha`.
3. Builds a CDF and inverse-CDF samples `count` positions (window space).
4. Returns `Vec<Vec2>` positions only — **no color**.

Particle color today comes from the render shader: a star texture, an optional `solid_color` uniform, and a velocity/attract tint (`LineMaterial`, `assets/shaders/line/render.wgsl`). The `Particle` GPU struct (`crates/wc-sketches/src/line/particle.rs`) carries `position, velocity, original_xy, alpha, age, lifespan, spawn_hash, _pad: [f32; 2]` — particles recycle back to `original_xy`, so a particle's anchor is fixed for life.

## Existing tools / no new dependencies

Everything needed is already in the graph: `image` (decode + per-pixel RGB, already used by the sampler and the template store), `serde`/`toml` (persistence, already used by settings), `bevy_egui` (UI). No new crates. The work is sampler math, one GPU struct field, one shader uniform, a small persisted map, and a dock-section UI hook.

## Data model

New struct, owned by the Line sketch (`crates/wc-sketches/src/line/template_adjustments.rs`):

```rust
pub struct TemplateAdjustments {
    pub white_point: f32,    // 0.0–1.0, default 1.0
    pub black_point: f32,    // 0.0–1.0, default 0.0
    pub invert: bool,        // default false
    pub gamma: f32,          // 0.1–4.0, default 1.0
    pub position: Vec2,      // canvas-normalized offset, -1.0–1.0 per axis, default (0,0)
    pub scale: Vec2,         // 0.1–4.0 per axis, default (1,1)
    pub color_influence: f32 // 0.0–1.0 (UI shows 0–100%), default 0.0
}
```

`Default` reproduces today's behavior bit-exactly (white 1, black 0, gamma 1, invert off, position 0, scale 1, color 0) — an image with no saved adjustments samples and renders identically to now. **Zero regression at defaults** is a hard requirement and is covered by a test.

Held in a Bevy resource that is **registered through the existing settings system** so it rides the centralized persistence/autosave rails (per the settings-architecture decision below):

```rust
#[derive(Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[reflect(Resource, Default)]
pub struct LineTemplateAdjustments {
    map: HashMap<String /* content hash */, TemplateAdjustments>,
}
```

- **Key** = the image's content hash. The active hash is the file stem of `spawn_template` (managed blobs are stored as `{hash}.{ext}`), so no separate "active hash" field is needed.
- **Persistence:** `LineTemplateAdjustments` implements `SketchSettings` with `STORAGE_KEY = "line-template-adjustments"` and an **empty `settings_def()`** (so the reflection walker draws nothing for it). It is `register_sketch_settings::<…>()`'d like any other settings type. The existing `persistence` layer is pure serde (`toml::Value::try_from`), so the `HashMap` field serializes fine into the combined `sketch-settings.toml` under its own table; `autosave` arms off `is_resource_changed`, and there are **no `requires_restart` fields** so it never triggers the fade reload. No separate file, no separate flush machinery — editing the map through `world.get_resource_mut::<LineTemplateAdjustments>()` arms the existing debounce.
- **Pruning:** when an image is deleted from the template cache, its hash is removed from the map; startup reconcile drops entries whose hash is no longer in the library. (See "Deletion" below.)

### Settings-architecture decision (custom hook is render-only)

A senior-engineer review confirmed: the reflection-based `SketchSettings` system stays the default for **every** flat setting (no existing setting moves to the new hook — the codebase deliberately retired per-sketch custom renderers). The custom dock section is a **rendering-only** escape hatch for data the `SettingDef` table cannot express (this hash-keyed map). Its backing state MUST be a registered resource (as above) so persistence, autosave, and change-detection stay centralized — the hook renders, it does not persist. The `CustomDockSections` rustdoc will state this constraint so the hook does not become a route around the uniform settings system. Known accepted gap: the dev console's `set_setting` cannot address individual map entries by key.

## Spawn-density math (white / black / gamma / invert)

In the sampler, replace `weight = luminance × alpha` with a luminance remap. Per pixel, with `lum` normalized to [0,1]:

```
t = clamp((lum - black_point) / max(white_point - black_point, eps), 0, 1)
t = t.powf(gamma)
if invert { t = 1.0 - t }
weight = t * alpha
```

- `black_point` raises the floor (darker pixels stop spawning), `white_point` lowers the ceiling, `gamma` bends the response curve, `invert` spawns in the dark regions.
- Edge cases: `white_point <= black_point` collapses the range — guard with `eps` so the divide is safe; the result is a hard threshold (acceptable, documented).
- If the remapped grid is all-zero, fall back to the existing horizontal-line layout (unchanged behavior).

## Transform (position / scale)

Applied to the sampler's output coordinates, about the canvas center:

```
pos = center + (sampled - center) * scale + position * (canvas_size / 2)
```

Scale zooms the layout, position shifts it. Particles that land outside the canvas are kept (the sim/camera already handle off-screen positions); we do **not** drop them, so scale-down/translate reads as the image moving within the field rather than particles vanishing.

## Smooth-tuning detail: deterministic sampling

The sampler currently uses `rand::rng()` (fresh entropy each call), so every re-seed reshuffles all particles. For live tuning that is noisy. The sampler will instead use a **deterministic, seeded** RNG (fixed seed) so particle *i* draws the same uniform `target` each time. Changing a knob then shifts the existing layout continuously (particle *i* slides as the CDF changes) instead of respraying. Seed is a fixed constant; spawn count changes still produce a stable prefix.

## Color influence (render-pipeline piece)

1. **Sampler returns color.** `sample_from_heatmap` (or a sibling that the spawn calls) also returns, per sampled particle, the source image's RGB at the chosen bin (the downsampled grid already holds it). Sampled **once at spawn**.
2. **Particle carries it.** Pack the RGB8 into a `u32`, bitcast to `f32`, and store in `Particle._pad[0]` (renamed to `spawn_color`); `_pad[1]` stays reserved. **GPU struct size and alignment are unchanged** (still 48 bytes). The WGSL `Particle` struct in `simulate.wgsl` gets the matching field rename (the sim does not touch it; it is render-only).
3. **Shader blends.** `render.wgsl` unpacks `spawn_color` and blends: `rgb = mix(base_rgb, spawn_rgb, color_influence)`, where `color_influence` is a new **`LineMaterial` uniform**. At `0.0` this is `mix(base, _, 0.0)` = today's color bit-exactly. The existing attract/velocity tint applies **after** this blend, preserving screensaver behavior.
4. Because color influence is a uniform, changing it is **live** — no re-seed, just write the uniform.

## Re-apply

- **Six position knobs** (white/black/gamma/invert, position, scale): a debounced (~200ms) Line system gated on `is_resource_changed::<LineTemplateAdjustments>()` re-runs the sampler with the active adjustments and **re-uploads the existing particle buffer in place** via `Assets<ShaderStorageBuffer>::get_mut` — no fade, no Home round-trip. Particles snap to the new layout and the sim animates from there. The system keeps a `Local` snapshot of the last-seeded *position-affecting* fields and skips the re-seed when only `color_influence` changed, so color tuning never re-seeds. Nothing here is `requires_restart`.
- **Color influence:** a system writes `color_influence` into the `LineMaterial` uniform each frame (or on change). Instant, no re-seed.

## UI: a custom Line dock section

A small, generic extension point in the settings dock (`crates/wc-core/src/settings/`): a `CustomDockSections` resource holding `{ tab, render: fn(&mut World, &mut egui::Ui, &OverlayStyle) }` entries. After `draw_user_panel` renders the reflected sections for the active tab, it calls each custom section registered for that tab. The render fn re-enters `World` (the dock is already an exclusive system), so it can read `LineSettings`, the adjustments map, and write back. This is a clean seam other sketches can reuse later (the templates may serve other sketches).

The Line sketch registers one section, **"Template adjustments"**, rendered in the Line tab, that:

- Shows nothing when `spawn_template` is empty.
- Otherwise resolves the active hash, looks up (default-inserts) its `TemplateAdjustments`, and renders **draggable normalized-percentage sliders** for every numeric knob (egui `Slider`, drag-the-track interaction, value shown as a percentage) plus a checkbox for invert. Each knob is presented on a percentage scale with **100% = its neutral/identity value** so the defaults read as a clean baseline: white 100%, black 0%, gamma 100%, position 0% (center), scale 100%, color influence 0%. The internal `f32` is derived from the percentage (see the ranges table). Emphasis on drag: these are slider widgets, not number-entry fields.
- On any edit: writes the value into the map via `world.get_resource_mut::<LineTemplateAdjustments>()`, which arms the existing autosave (persist) and the change-gated re-seed.
- Section sits directly below the template picker (the `spawn_template` field is ordered last in the Line settings section so the adjustments follow it).

## Deletion and the active-path bug

Deleting an image from the cache must (a) prune `LineTemplateAdjustments.map[hash]` and (b) clear `spawn_template` if it was the active image. Item (b) is the **separately-tracked persistence bug** ("deleting the active template leaves a stale path that warns on next launch") — it is a prerequisite cleanup fixed on its own, not inside this feature, but this feature's deletion path adds the adjustments pruning alongside it. Pruning also runs at startup reconcile: drop map entries whose hash is no longer in the template library (heals out-of-band deletes).

## Defaults and ranges

All numeric knobs are draggable percentage sliders; `internal = f(percent)` is the mapping from the displayed percent to the stored `f32`.

| Knob | Internal default | Internal range | Shown as | % range (default) | internal = f(%) |
|---|---|---|---|---|---|
| White point | 1.0 | 0.0–1.0 | % | 0–100% (100%) | pct/100 |
| Black point | 0.0 | 0.0–1.0 | % | 0–100% (0%) | pct/100 |
| Invert | false | — | checkbox | — | — |
| Gamma | 1.0 | 0.1–4.0 | % (100% = 1.0) | 10–400% (100%) | pct/100 |
| Position X/Y | 0.0 | −1.0–1.0 | % (0% = center) | −100–100% (0%) | pct/100 |
| Scale X/Y | 1.0 | 0.1–4.0 | % (100% = 1.0) | 10–400% (100%) | pct/100 |
| Color influence | 0.0 | 0.0–1.0 | % | 0–100% (0%) | pct/100 |

## Error handling

- Missing/undecodable image → existing fallback line (unchanged).
- `white_point <= black_point` → `eps`-guarded divide, hard-threshold result.
- All-zero remapped weights → fallback line.
- Adjustments file missing/malformed → empty map, defaults everywhere (logged, never fatal — mirrors `persistence::load`).
- Active hash not in the map → default-insert on first edit; render defaults until then.

## Testing

- `TemplateAdjustments::default()` produces the identity remap: a luminance remap test asserts `weight == luminance × alpha` at defaults (no-regression proof).
- Remap math: black/white point clamp, gamma curve, invert — table tests over sample luminances.
- Transform: position/scale move/zoom a known point as expected; off-canvas points are retained.
- Deterministic sampler: same adjustments + count → identical positions across two calls (the property that makes tuning smooth).
- Color packing: RGB8 → f32 → RGB8 round-trips; `mix(base, c, 0.0) == base`.
- Persistence: map round-trips through TOML; pruning drops a deleted hash; reconcile drops orphans.
- Per-image isolation: tuning image A then selecting image B shows B's (default) values; reselecting A restores A's values.

## Out of scope (this plan)

- GPU-side image re-sampling (particles recycle to fixed anchors; not needed).
- Adjustments for sketches other than Line (the dock-section hook is generic, but only Line registers one now).
- Value-conditional field hiding in the generic settings macro (the custom section handles its own show/hide).

## Future tuning-UX enhancements (from a UX review; deferred)

The debounced re-seed (snap to the new layout on pause) was shipped and works.
A UX review recommended keeping the snap rather than building a full per-particle
tween, but flagged two higher-value follow-ups worth a future pass:

- **Position/scale as a live render transform, not a re-seed.** `transform_point`
  is a pure affine map, so position X/Y and scale X/Y could ride a live render
  uniform (like `color_influence`) for continuous, zero-re-upload feedback while
  dragging — only the density knobs (white/black/gamma/invert) genuinely need
  re-sampling. **Caveat:** a render-only transform decouples the GPU sim
  (attractors, constrain-to-box anchors operate on the un-transformed positions)
  from what is shown, so it needs a "live uniform while dragging, bake into the
  spawn on pause" hybrid (or an inverse-transform of the attractor inputs) to keep
  interaction aligned. This is the reason it was deferred rather than shipped.
- **Preserve particle velocity across a re-seed** so the field never visibly
  freezes on each fire. Harder than it looks: velocities live on the GPU buffer,
  so this needs a readback or a GPU-side re-seed rather than the CPU rebuild.
