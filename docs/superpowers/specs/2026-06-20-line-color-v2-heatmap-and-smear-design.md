# Line Color v2 — Heatmap Palette Modes + Configurable Smear — Design Spec

**Date:** 2026-06-20
**Status:** Approved design, ready for implementation plan
**Supersedes (in part):** [2026-06-19-line-psychedelic-palette-design.md](2026-06-19-line-psychedelic-palette-design.md) — the v1 palette shipped (IQ cosine rainbow, random "Scatter", time-cycle). This v2 redesigns both palette modes after a live review and adds a separate configurable-smear component. The v1 infrastructure (the `PaletteMode` enum, `palette_params` uniform, crossfade compositing, `drive_palette` driver, change-gating, off-path discipline) is **kept**; this spec revises the palette math and removes the time-cycle, then adds the smear component.

## Why (live-review feedback)

1. **Velocity** v1 made the whole swarm uniformly strobe through a cycling rainbow. It should instead key each particle's hue to *its own* `|velocity|` across a **cool→hot heatmap** (slow = cool, fast = hot). Glow/intensity was good — keep it.
2. **Scatter** v1 (random per-particle hue) averaged into a gray wash. Replace it with a **creation-index heatmap** that peaks hot at the middle of the spawn list and cools toward both ends.
3. **Smear** is "just cool blue with hints of orange" and is a dominant on-screen color feature — make its coloring configurable.
4. Particles must **never crush so dark it looks like nothing is happening** — the palette must preserve brightness.

---

## Component 1 — Particle heatmap palette (`render.wgsl`, `LineSettings`, `LineMaterial`)

### Palette: Turbo, value-normalized
- Replace v1's IQ cosine rainbow `palette()` with **Turbo**, texture-free via a compact polynomial approximation inlined in the shader (matches the no-LUT approach; exact coefficients chosen in the plan from a known-good Turbo fit). Anchors: `t=0` blue, `t≈0.5` green, `t=1` red.
- **Value-normalize** every palette color so the palette supplies **hue only, never brightness**: `pal = turbo(t); pal /= max(pal.r, pal.g, pal.b)` (epsilon-guarded). This lifts Turbo's dark cool end (`turbo(0) ≈ (0.19,0.07,0.23)`) to a **bright** blue `(0.83,0.30,1.0)`, so the star keeps supplying brightness and **no particle crushes toward black** regardless of velocity or index. (A saturated hue is inherently lower-luminance than white; `palette_strength` is the lever for that, mixing back toward the white star. No hard luminance floor for now — revisit after a live look if needed.)

### Modes (`PaletteMode`)
- **`Off`** — unchanged; bit-exact pre-palette path (uniform-mode branch skipped at `palette_params.x == 0`).
- **`Velocity`** — per-particle `t = clamp(|velocity| · scale / 180.0, 0, 1)` → `turbo(t)`. **Clamped, not wrapped:** slow particles sit at the cool end, fast at the hot end. No time term.
- **`Spectrum`** (renames the v1 `Scatter` variant) — per-particle normalized creation index `idx_norm = f32(particle_index) / f32(arrayLength(&particles) − 1u)` (computed in the vertex stage from the built-in index and the storage-buffer length — **no count uniform needed**), shaped by a **center-peak tent** then Turbo: `t = pow(1 − abs(2·idx_norm − 1), scale)` → `turbo(t)`. Hot at the middle index, cooling to both ends of the spawn list. (Pre-release, one operator: the variant rename needs no migration; legacy TOML with `palette_mode = "Scatter"` falls back to the `Off` default harmlessly.)

### Drop the time-cycle
Remove `palette_cycle` (the setting, its `default_*` fn, its module-doc bullet), the `globals` import, and the `globals.time` term from `render.wgsl`. A cycling phase is incompatible with a stable heatmap (it was the "uniform strobe" the review rejected). The freed `palette_params.z` slot is reused for `scale` (count comes from `arrayLength`, so no uniform is needed for it).

### Settings (revised)
| field | type | category | default | meaning |
|---|---|---|---|---|
| `palette_mode` | `PaletteMode { Off, Velocity, Spectrum }` (`ty = Enum`) | User | `Off` | driver for the heatmap hue |
| `palette_strength` | `f32` `0–1` | User | `0.8` | crossfade image→palette (also the saturation/brightness lever) |
| `palette_scale` | `f32` `0.1–5` | Dev | `1.0` | per-mode tuning: Velocity speed-sensitivity (`180/scale` px/s ≈ full hot) / Spectrum tent sharpness (`pow` exponent) |

`palette_cycle` is removed. `palette_params: Vec4` becomes `(mode_index, strength, scale, 0.0)`; `palette_params()` helper drops the `cycle` arg. The change-gated `drive_palette` driver and its compare-against-material self-correction are otherwise unchanged.

### Compositing (unchanged from v1, now with the normalized heatmap)
`img_base = mix(texel.rgb, texel.rgb*img_rgb, template_color.x)`; inside the `palette_params.x > 0.5` uniform branch, `pal_base = texel.rgb * value_normalize(turbo(t))`, `base = mix(img_base, pal_base, strength)`. Attract wake tint + brightness lift still layer on top. Off path stays bit-exact.

---

## Component 2 — Configurable smear coloring (`gravity.wgsl`, `LinePostParams`, `LineSettings`)

The smear's "cool blue / warm orange" is **chromatic aberration**, not a sampled palette: `gravity.wgsl` accumulates per-step channel factors (`outgoing (0.96,1,1.042)`, `incoming (1.042,1,0.96)`) over `NUM_STEPS = 11`, compounding to outgoing end-tint `≈(0.64,1,1.59)` (blue boost) and incoming `≈(1.59,1,0.64)` (orange boost). The **>1 boost is load-bearing** — it makes the trails *glow* additively (feeding bloom/AgX), so it is preserved, not replaced by a ≤1 filter.

### Make the two fringe end-tints configurable (HDR-capable)
- Two **User color settings** plus a shared gain:
  - `smear_outgoing_color` (`ty = Color`, `[f32;4]`, normalized hue/ratio), default `≈(0.40, 0.63, 1.0, 1.0)`.
  - `smear_incoming_color` (`ty = Color`), default `≈(1.0, 0.63, 0.40, 1.0)`.
  - `smear_chroma_gain` (`f32`, range `0.0–3.0`), default `1.59`.
- CPU-side, each writer computes the **HDR end-tint** `end = color.rgb · gain` (so the dominant channel boosts past 1). The defaults reproduce today's end-tints **bit-identically** (`(0.40,0.63,1.0)·1.59 ≈ (0.64,1.0,1.59)`, mirror for incoming). Neutral smear = white colors + gain `1.0`.

### Shader
- `LinePostParams` gains `smear_outgoing_tint: [f32;4]` and `smear_incoming_tint: [f32;4]` (xyz = HDR end-tint, w = pad). `gravity.wgsl`'s `PostParams` mirrors them.
- Replace the hardcoded `outgoing_factor`/`incoming_factor` with the per-step factor derived from the configured end-tint: `factor = pow(end_tint, vec3(1.0 / f32(NUM_STEPS)))`, then accumulate exactly as today (`v_accum` starts at `factor`, multiplies by `factor` each step, reaching `end_tint` at the last step). This preserves the along-trail color-deepening; only the target hue changes. At the default end-tints, `pow(end, 1/11)` reproduces the current `(0.96,1,1.042)`/`(1.042,1,0.96)` factors — **pixel-identical default**.

### Wiring
The end-tints are static (change only on a settings edit) but cheap; the existing `LinePostParams` writers (`update_sim_params` for active, the screensaver driver for attract) each set the two tint fields from `LineSettings` via a small shared helper (`bake_smear_tints(&mut post, &settings)`) so the two writers can't drift. The smear is thus configured consistently in both active and screensaver. The default `LinePostParams` tint fields can stay zero (the `Default` derive): the smear is gated by `g_constant`, which is also `0.0` by default and is only raised by those same in-Line writers, so an unset tint never renders — the writers populate the tints every in-Line frame alongside `g_constant`, and `remove_sim_params` resetting `LinePostParams` to default on exit is harmless.

---

## Performance
No new per-frame cost of consequence: the palette adds a value-normalize (one `max` + divide) inside the already-gated uniform branch; the smear changes a compile-time constant into a `pow` of a uniform per fragment-step (11 `pow`s on a vec3 — negligible vs the 22 texture samples already in the march). No allocations; no new systems; zero cost when palette is `Off` (branch skipped) and outside `AppState::Line`. Soak profile unaffected.

## Testing
- **Turbo:** anchor tests (`t=0` blue-dominant, `t≈0.5` green-dominant, `t=1` red-dominant); **value-normalization** keeps max channel `== 1.0` for representative `t` (the brightness guarantee).
- **Velocity mapping:** `clamp` floors slow→0 and caps fast→1; `scale` shifts the band.
- **Spectrum mapping:** tent is `0` at `idx_norm ∈ {0,1}` and `1` at `0.5`; `scale` sharpens via `pow`.
- **Smear:** `end = color·gain` with the default color/gain reproduces the legacy end-tints within epsilon; `pow(end, 1/11)` reproduces the legacy per-step factors (pixel-identical default proof); white color + gain `1.0` → neutral `(1,1,1)` factor.
- **Forward-compat:** legacy TOML missing `smear_*` / containing `palette_cycle` / `palette_mode="Scatter"` deserializes to defaults with siblings preserved.
- **No-regression:** `Off` + default smear settings renders the established look (capture diff bit-exact, as verified for v1).

## Scope / decomposition
Two **independent** components — Component 1 touches `render.wgsl` / `LineMaterial` / palette settings; Component 2 touches `gravity.wgsl` / `LinePostParams` / smear settings. They share no uniforms or files and can be implemented and tested separately (the plan groups them as two task sequences). Both are part of "finishing the Line color work."

## Out of scope
- A hard luminance floor on the palette (try value-normalization first; add only if the live look still reads too dark).
- HDR color-picker widgets in the settings system (using color + gain instead; revisit only if it proves cleaner).
- The smear-follows-hand focal work ([2026-06-20-line-smear-follows-hand-design.md](2026-06-20-line-smear-follows-hand-design.md)) — separately specced and parked until this color work lands.
- Per-fringe (asymmetric) chroma gain — one shared gain reproduces today and covers the need; split later if wanted.
