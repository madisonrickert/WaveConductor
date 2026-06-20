# Line Psychedelic Color Palette — Design Spec

**Date:** 2026-06-19
**Status:** Approved design, ready for implementation plan
**Related:** [2026-06-18-line-template-adjustments-design.md](2026-06-18-line-template-adjustments-design.md) (the per-image `color_influence` tint this composes with), [2026-05-29-line-screensaver-attract-mode-design.md](2026-05-29-line-screensaver-attract-mode-design.md) (the attract wake tint / brightness lift this layers under)

## Goal

Give the Line sketch a **configurable psychedelic color palette** for its particles: hue keyed to a per-particle property and gently cycling over time, selectable from the settings panel and persisted/autosaved on the existing settings rails. This is a **color** feature — no geometric kaleidoscope mirroring (see *Out of scope*).

## Context: the current color path

Particle color is decided entirely in `assets/shaders/line/render.wgsl`. The star sprite is a near-white star point, and every existing color effect *multiplies* it so the sprite's luminance shape (bright core, soft falloff) is preserved:

```wgsl
let img_rgb = /* unpack Particle.spawn_color (RGB8) */;
let base    = mix(texel.rgb, texel.rgb * img_rgb, template_color.x);  // per-image color influence
let wake    = smoothstep(WAKE_SPEED_LO, WAKE_SPEED_HI, in.speed) * attract_color.x;
let tinted  = mix(base, base * WAKE_TINT, wake);                      // attract-only velocity tint
let rgb     = tinted * (1.0 + attract_color.y);                       // attract-only brightness lift
```

- `template_color` (`@binding(5)`) is the per-image color-influence uniform; `attract_color` (`@binding(4)`) carries the attract wake tint (`.x`) and brightness lift (`.y`). Both are change-gated `LineMaterial` uniforms written by driver systems.
- Per-particle inputs available at the fragment stage: `in.speed` (= `|velocity|`, meaningful in **both** live and attract) and `in.spawn_color` (packed image RGB). The `Particle` GPU struct (`crates/wc-sketches/src/line/particle.rs`) also carries `spawn_hash` (a deterministic per-particle value in `0..=1`) and `age` — but **`age` is pinned to `0` during live interaction** (it only advances in attract mode as the lifetime accumulator), so it is not a viable always-on palette driver and is excluded.

So the palette has cheap inputs already on the GPU: `speed` is in the fragment stage today; `spawn_hash` is in the struct and only needs piping through the vertex stage. **No `Particle` layout change is needed** (size/alignment unchanged).

## Existing tools / no new dependencies

Nothing new enters the dependency graph. The settings derive macro already supports `ty = Enum` (rendered as an `egui::ComboBox`, variants sourced from `bevy_reflect`) and numeric sliders. The driver/uniform/change-gating pattern is established (`drive_color_influence`, `drive_attract_color`). Time-driven animation reads `globals.time` from the 2D view bind group the sprite pipeline already binds (`bevy_sprite_render::mesh2d_view_bindings`, `@group(0) @binding(1)`) — no new binding, no per-frame uniform write.

## Palette modes

```rust
/// Which per-particle property drives the palette hue. Unit variants only
/// (the settings ComboBox writes back a payload-less DynamicEnum).
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PaletteMode {
    /// Palette off — particle color is exactly today's path (image tint + star).
    #[default]
    Off,
    /// Hue keyed to |velocity|: the calm field sits at one end of the palette,
    /// stirred-up particles sweep through it. Color traces motion/energy.
    Velocity,
    /// Hue keyed to the stable per-particle `spawn_hash`: a static rainbow-
    /// confetti scatter where each particle keeps its own color.
    Scatter,
}
```

`PaletteMode::index()` maps `Off→0.0, Velocity→1.0, Scatter→2.0` for the uniform. `Default == Off` reproduces today's color bit-exactly (hard requirement, covered by a test).

## Settings (`LineSettings`, new "Palette" section)

| field | type | category | default | range | meaning |
|---|---|---|---|---|---|
| `palette_mode` | `PaletteMode` (`ty = Enum`) | User | `Off` | — | which property drives hue |
| `palette_strength` | `f32` | User | `0.8` | `0.0–1.0` | crossfade: image color → palette color |
| `palette_cycle` | `f32` | User | `0.03` | `0.0–0.5` | time-cycle rate (cycles/s; `0` = static) |
| `palette_scale` | `f32` | Dev | `1.0` | `0.1–5.0` | how far the property spreads across the palette |

- Each field gets a `default_<name>()` free fn and `#[serde(default = "...")]` (the missing-field forward-compat pattern AGENTS.md mandates; values mirror the `#[setting(default = ...)]`).
- `palette_strength` defaults to `0.8` (not `0`) so flipping mode to Velocity/Scatter immediately shows color; harmless while `Off` because the mode gate zeroes its effect.
- `palette_cycle` defaults to a slow `0.03` so the palette is gently alive without being seasick; easily zeroed for a static look.
- `palette_scale` is a Dev tuning knob (hidden from the curated panel); a unitless multiplier with a sensible value baked per mode in the shader (below), so `1.0` is a good default for both modes.

## Material uniform + driver

- New `LineMaterial::palette_params: Vec4` at `@binding(6)` = `(mode_index, strength, cycle, scale)`. Helper `LineMaterial::palette_off() -> Vec4 { Vec4::ZERO }` (mode `0` ⇒ provable no-op). `spawn.rs` seeds it with `palette_off()`.
- New `drive_palette` system (`crates/wc-sketches/src/line/systems/palette.rs`, **not** feature-gated): builds the target `Vec4` from `LineSettings` via a pure, unit-testable `palette_params(mode, strength, cycle, scale) -> Vec4` helper, and writes it into the material **only when it differs** (change-gated, single float compare otherwise — no per-frame asset churn). Registered under **both** `sketch_active(AppState::Line)` and `in_screensaver(AppState::Line)` (mirrors `drive_attract_color`) so the palette applies live and in the screensaver while preserving "zero systems when idle." Uses a `Local<Vec4>` last-written guard; only advances it on an actual write so a not-yet-loaded material retries.

## Shader: the crossfade, behind a uniform branch

`render.wgsl` imports `globals` alongside `view`, pipes `spawn_hash` through the vertex stage (`VertexOutput @location(4) @interpolate(flat) scatter: f32` — flat because it is a per-particle constant), and inserts the palette **between** `base` and the wake tint:

```wgsl
fn palette(t: f32) -> vec3<f32> {
    // Inigo Quilez cosine palette — canonical rainbow.
    // Each term: a = bias, b = amplitude, c = frequency, d = per-channel phase.
    let a = vec3<f32>(0.5, 0.5, 0.5);
    let b = vec3<f32>(0.5, 0.5, 0.5);
    let c = vec3<f32>(1.0, 1.0, 1.0);
    let d = vec3<f32>(0.0, 0.33, 0.67);
    return a + b * cos(6.28318530718 * (c * t + d));
}

// img_base is today's behavior (image pixel color gated by color-influence).
let img_base = mix(texel.rgb, texel.rgb * img_rgb, template_color.x);

let mode     = palette_params.x;  // 0 Off, 1 Velocity, 2 Scatter
let strength = palette_params.y;
let cycle    = palette_params.z;
let scale    = palette_params.w;

var base = img_base;
if (mode > 0.5) {                 // uniform branch: constant across the draw, no divergence
    var driver_t: f32;
    if (mode < 1.5) {
        // Velocity: 180 px/s ≈ one palette cycle at scale 1 (matches the wake band).
        driver_t = in.speed * scale / 180.0;
    } else {
        // Scatter: stable per-particle hash spread across the palette.
        driver_t = in.scatter * scale;
    }
    let t        = fract(driver_t + globals.time * cycle);  // wrap + animate, no uniform write
    let pal_base = texel.rgb * palette(t);                  // palette fully colorizes the white star
    base         = mix(img_base, pal_base, strength);       // crossfade image ↔ palette
}
// wake tint + brightness lift apply on top of `base`, unchanged.
```

**Off path is provably free and bit-exact.** `palette_params` is a uniform, constant across the entire draw, so the `mode > 0.5` branch is taken uniformly by every fragment (no warp divergence). When `Off` the palette math (`cos`, `fract`, `mix`) is never executed and `base == img_base` exactly — current rendering is unchanged in both output and cost.

## Performance

Aligned with the soak/thermal priorities (multi-hour stability, not peak FPS):

- **Fragment ALU when enabled:** ~3 `cos` + a handful of `fract`/`mix`/`mul`, negligible against the texture sample + alpha-blend overdraw that already dominate the ~12.8k 13×13px quads. When **disabled** (the default), the uniform branch skips all of it — the common case pays nothing.
- **No per-frame uniform churn:** time-cycling reads the already-bound `globals.time` in-shader rather than threading time through the uniform; the `palette_params` uniform is change-gated and re-uploads only on a settings edit. (Putting time in the uniform would force a per-frame asset mutation + bind-group re-prepare — explicitly avoided.)
- **Vertex stage:** one extra flat `f32` interpolant (`spawn_hash`), always present regardless of mode — trivial.
- **CPU:** one change-gated driver system (one float compare, occasional `Vec4` write, no allocation), zero when idle / outside Line.
- **Soak:** default-off is unchanged, so the 8-hour soak target is unaffected; enabled adds only a bounded, steady-state ALU increase (no growth, no allocation, no jitter). In the screensaver the per-tier present-rate throttle (≈30/15/3 fps) caps the per-second cost. `globals.time` over 8h (~28.8k s) retains ~1/500 s f32 resolution, so the phase animation quantizes imperceptibly.

## Behavior notes

- **Applies in both live and attract** when enabled — it is a global look toggle, not screensaver-specific. In attract the calm warm-white personality gives way to the palette; the attract wake tint and brightness lift still layer on top of the palette-colored `base`. Keeping attract calm/white while live is palette-on would be a per-mode follow-up, not in this cut.
- **Crossfade semantics:** `palette_strength` dials each particle's color from its image-influence color (`strength = 0`, today's behavior) toward the full palette color (`strength = 1`). The palette portion fully colorizes the white star regardless of image-influence, so the palette is visible even with no template loaded.

## Testing

- `palette_params()` helper: `Off → Vec4::ZERO`; mode index encoding (`Velocity→1`, `Scatter→2`); strength/cycle/scale clamped to range; out-of-range inputs clamp rather than invert.
- `PaletteMode`: reflect variant-name round-trip (`["Off","Velocity","Scatter"]`); serde round-trip; `Default == Off`.
- Forward-compat: extend `settings.rs`'s missing-field test so legacy TOML lacking the palette keys still deserializes, palette fields fall back to defaults, and siblings are preserved.
- No-regression: with `Off`, the helper yields `Vec4::ZERO`; document (and assert via the helper) that the shader's `base == img_base` when `mode == Off`.
- Visual: add `cargo xtask capture` scenarios for Velocity and Scatter palettes; the operating agent reviews the PNGs (no LLM API spend). Confirm `Off` capture is pixel-identical to the pre-change baseline.

## Out of scope (this plan)

- **Geometric kaleidoscope** (screen-space mirror-symmetry post-process). A separate, larger feature with its own render-graph node and spec; this plan is color only.
- **Position / Age palette modes.** Position needs the position interpolant and a spatial-mapping design; Age only advances in attract (dead during live interaction). Both deferred.
- **Palette presets.** v1 ships one curated cosine rainbow. A `palette_preset` enum (a small table of `a/b/c/d` coefficient sets selected by an index, packed into a second uniform or a spare slot) is a clean follow-up once the single palette is proven.
- **Per-mode "keep attract calm"** override (palette suppressed in the screensaver). Deferred; the palette currently applies in both.
