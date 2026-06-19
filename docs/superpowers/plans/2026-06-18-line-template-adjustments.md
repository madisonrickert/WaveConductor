# Line Template Adjustments Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Seven per-image tuning knobs (white/black point, invert, gamma, position X/Y, scale X/Y, color influence) for the Line sketch's image template, persisted per image, feeding the particle spawner.

**Architecture:** Pure adjustment math + a colour-returning heatmap sampler (always compiled) drive particle positions and a packed per-particle spawn colour in the existing GPU `Particle` struct; the render shader blends that colour by a live `LineMaterial` uniform. The per-image adjustments live in a registered `SketchSettings` resource (hash-keyed map, empty `settings_def`) so they persist/autosave through the existing central path; a render-only custom dock-section hook renders the active image's knobs. Templates-coupled pieces (map, UI, pruning, re-seed-from-map) sit behind a native-only `wc-sketches/templates` feature.

**Tech Stack:** Rust, Bevy 0.18, bevy_egui 0.39 (egui 0.33), wgpu/WGSL, `image`, `bytemuck`, serde/toml.

## Global Constraints

- Defaults reproduce today's behaviour bit-exactly: white_point 1.0, black_point 0.0, gamma 1.0, invert false, position [0,0], scale [1,1], color_influence 0.0. Covered by tests.
- The GPU `Particle` struct stays **48 bytes** (12 × f32); the Rust `#[repr(C)]` struct and the WGSL `struct Particle` in `simulate.wgsl` and `render.wgsl` MUST stay byte-identical.
- No `unwrap()`/`expect()` in non-test code unless a documented invariant. No `as` casts where `From`/`TryFrom`/`try_from` work (note: `heatmap.rs` already has a module-level `#![allow]` for its bounded image-dimension casts).
- `position`/`scale` are `[f32; 2]` (serde-trivial; avoids depending on glam's serde feature). Convert to `Vec2` at use sites.
- Templates-coupled code is `#[cfg(feature = "templates")]` in `wc-sketches` (feature `templates = ["wc-core/templates"]`, off by default, native-only — mirrors `hand-tracking-mediapipe`). The `waveconductor` binary enables `wc-sketches/templates`.
- Per-image state persists through a **registered `SketchSettings` resource**, not a separate file. The custom dock-section hook renders only; it never persists.
- All numeric knobs are **draggable percentage sliders** (egui `Slider`); invert is a checkbox. Position/scale have **independent X and Y** sliders. Percent→internal: `internal = pct/100` (white/black/color 0–100%; gamma/scale 10–400% where 100%=1.0; position −100–100% where 0%=center).
- No new crate dependencies.
- Verify each task with: `cargo clippy -p <crate> --features templates --all-targets -- -D warnings`, `cargo nextest run -p <crate> --features templates`, `cargo fmt --all -- --check`. Final task also runs `--all-features --workspace` clippy + `cargo doc`.

---

### Task 1: `TemplateAdjustments` struct + remap/transform/pack math

**Files:**
- Create: `crates/wc-sketches/src/line/template_adjustments.rs`
- Modify: `crates/wc-sketches/src/line/mod.rs` (add `pub mod template_adjustments;` near the other `mod` lines, ~line 40)
- Test: same file, `#[cfg(test)] mod tests`

**Interfaces:**
- Produces:
  - `pub struct TemplateAdjustments { pub white_point: f32, pub black_point: f32, pub invert: bool, pub gamma: f32, pub position: [f32;2], pub scale: [f32;2], pub color_influence: f32 }` deriving `Clone, Debug, PartialEq, Reflect, Serialize, Deserialize` with `#[reflect(Default)]` and a manual `Default` (identity values).
  - `pub fn remap_weight(luminance_0_255: f32, alpha_0_1: f32, adj: &TemplateAdjustments) -> f32` — the per-pixel spawn weight.
  - `pub fn transform_point(sampled: Vec2, canvas: Vec2, adj: &TemplateAdjustments) -> Vec2` — transform a window-space sample about the canvas centre.
  - `pub fn pack_rgb8(rgb: [u8;3]) -> f32` and `pub fn unpack_rgb8(packed: f32) -> [u8;3]` — bit-preserving colour pack for the `Particle` spawn-colour slot.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Vec2;

    fn approx(a: f32, b: f32) { assert!((a - b).abs() < 1e-5, "got {a}, want {b}"); }

    #[test]
    fn defaults_are_identity_weight() {
        // At defaults the weight must equal luminance/255 * alpha (the old
        // `luminance * alpha` divided by 255 because remap normalizes lum).
        let adj = TemplateAdjustments::default();
        approx(remap_weight(255.0, 1.0, &adj), 1.0);
        approx(remap_weight(0.0, 1.0, &adj), 0.0);
        approx(remap_weight(128.0, 0.5, &adj), (128.0 / 255.0) * 0.5);
    }

    #[test]
    fn black_and_white_point_clamp() {
        let adj = TemplateAdjustments { black_point: 0.25, white_point: 0.75, ..Default::default() };
        // lum below black -> 0, above white -> 1*alpha, midpoint -> 0.5.
        approx(remap_weight(0.25 * 255.0, 1.0, &adj), 0.0);
        approx(remap_weight(0.75 * 255.0, 1.0, &adj), 1.0);
        approx(remap_weight(0.5 * 255.0, 1.0, &adj), 0.5);
    }

    #[test]
    fn gamma_bends_curve() {
        let adj = TemplateAdjustments { gamma: 2.0, ..Default::default() };
        // t=0.5 -> 0.25 under gamma 2.
        approx(remap_weight(0.5 * 255.0, 1.0, &adj), 0.25);
    }

    #[test]
    fn invert_flips() {
        let adj = TemplateAdjustments { invert: true, ..Default::default() };
        approx(remap_weight(255.0, 1.0, &adj), 0.0);
        approx(remap_weight(0.0, 1.0, &adj), 1.0);
    }

    #[test]
    fn degenerate_white_le_black_is_threshold_not_nan() {
        let adj = TemplateAdjustments { black_point: 0.5, white_point: 0.5, ..Default::default() };
        let w = remap_weight(0.9 * 255.0, 1.0, &adj);
        assert!(w.is_finite());
    }

    #[test]
    fn transform_default_is_identity() {
        let adj = TemplateAdjustments::default();
        let canvas = Vec2::new(1280.0, 720.0);
        let p = Vec2::new(300.0, 200.0);
        assert_eq!(transform_point(p, canvas, &adj), p);
    }

    #[test]
    fn transform_scale_zooms_about_center() {
        let adj = TemplateAdjustments { scale: [2.0, 2.0], ..Default::default() };
        let canvas = Vec2::new(1000.0, 1000.0);
        // The centre point is invariant under scale-about-centre.
        assert_eq!(transform_point(Vec2::new(500.0, 500.0), canvas, &adj), Vec2::new(500.0, 500.0));
        // A point 100 right of centre moves to 200 right of centre.
        approx(transform_point(Vec2::new(600.0, 500.0), canvas, &adj).x, 700.0);
    }

    #[test]
    fn transform_position_shifts_by_half_canvas_per_unit() {
        let adj = TemplateAdjustments { position: [1.0, 0.0], ..Default::default() };
        let canvas = Vec2::new(1000.0, 800.0);
        // position.x = 1.0 shifts right by half the canvas width (500).
        approx(transform_point(Vec2::new(500.0, 400.0), canvas, &adj).x, 1000.0);
    }

    #[test]
    fn pack_unpack_round_trips() {
        for rgb in [[0,0,0],[255,255,255],[10,20,30],[1,2,3]] {
            assert_eq!(unpack_rgb8(pack_rgb8(rgb)), rgb);
        }
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p wc-sketches --lib line::template_adjustments 2>&1 | tail -20`
Expected: compile error / FAIL (module/functions not defined).

- [ ] **Step 3: Implement**

```rust
//! Per-image adjustments for the Line spawn template: the luminance remap
//! (white/black point, gamma, invert) that reshapes spawn density, the
//! position/scale transform of the sampled coordinates, and the RGB pack used
//! to carry a per-particle spawn colour through the GPU `Particle` struct.
//!
//! Defaults are the identity: an image with default adjustments samples and
//! renders exactly as it did before this module existed.

use bevy::math::Vec2;
use bevy::reflect::Reflect;
use serde::{Deserialize, Serialize};

/// Per-image tuning knobs. `position`/`scale` are `[f32;2]` (serde-trivial);
/// `Default` is the identity (no remap, no transform, no colour tint).
#[derive(Clone, Debug, PartialEq, Reflect, Serialize, Deserialize)]
#[reflect(Default)]
pub struct TemplateAdjustments {
    /// Upper luminance (0..1) mapped to full spawn weight.
    pub white_point: f32,
    /// Lower luminance (0..1) mapped to zero spawn weight.
    pub black_point: f32,
    /// Spawn in the dark regions instead of the bright ones.
    pub invert: bool,
    /// Response curve exponent (1.0 = linear).
    pub gamma: f32,
    /// Canvas-normalized offset; ±1.0 shifts by half the canvas on that axis.
    pub position: [f32; 2],
    /// Per-axis zoom about the canvas centre (1.0 = original).
    pub scale: [f32; 2],
    /// Blend toward the image pixel colour, 0..1 (a live render uniform).
    pub color_influence: f32,
}

impl Default for TemplateAdjustments {
    fn default() -> Self {
        Self {
            white_point: 1.0,
            black_point: 0.0,
            invert: false,
            gamma: 1.0,
            position: [0.0, 0.0],
            scale: [1.0, 1.0],
            color_influence: 0.0,
        }
    }
}

/// Per-pixel spawn weight after the luminance remap. `luminance_0_255` is the
/// Rec.601 luminance, `alpha_0_1` the pixel's normalized alpha. At defaults this
/// is `(luminance/255) * alpha` — the old `luminance * alpha` up to the /255
/// normalization the remap introduces.
#[must_use]
pub fn remap_weight(luminance_0_255: f32, alpha_0_1: f32, adj: &TemplateAdjustments) -> f32 {
    let lum = (luminance_0_255 / 255.0).clamp(0.0, 1.0);
    // eps-guarded so white<=black degrades to a hard threshold, not NaN.
    let span = (adj.white_point - adj.black_point).max(1e-4);
    let mut t = ((lum - adj.black_point) / span).clamp(0.0, 1.0);
    t = t.powf(adj.gamma.max(1e-3));
    if adj.invert {
        t = 1.0 - t;
    }
    t * alpha_0_1
}

/// Transform a window-space sample about the canvas centre: scale zooms, then
/// position shifts by `position * (canvas/2)` per axis.
#[must_use]
pub fn transform_point(sampled: Vec2, canvas: Vec2, adj: &TemplateAdjustments) -> Vec2 {
    let center = canvas * 0.5;
    let scale = Vec2::new(adj.scale[0], adj.scale[1]);
    let offset = Vec2::new(adj.position[0], adj.position[1]) * center;
    center + (sampled - center) * scale + offset
}

/// Pack an RGB8 triple into an `f32` slot bit-for-bit (store via `f32::from_bits`,
/// recover via `bitcast<u32>` in WGSL). Never do float math on the result.
#[must_use]
pub fn pack_rgb8(rgb: [u8; 3]) -> f32 {
    let bits = (u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2]);
    f32::from_bits(bits)
}

/// Inverse of [`pack_rgb8`].
#[must_use]
pub fn unpack_rgb8(packed: f32) -> [u8; 3] {
    let bits = packed.to_bits();
    [
        ((bits >> 16) & 0xFF) as u8,
        ((bits >> 8) & 0xFF) as u8,
        (bits & 0xFF) as u8,
    ]
}
```

Note: the `as u8` in `unpack_rgb8` needs `#[allow(clippy::cast_possible_truncation, reason = "masked to 8 bits")]` on the function, or use `u8::try_from((bits >> 16) & 0xFF).unwrap_or(0)`. Prefer the masked-`as` with the allow since the mask guarantees the range.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p wc-sketches --lib line::template_adjustments 2>&1 | tail -10`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/wc-sketches/src/line/template_adjustments.rs crates/wc-sketches/src/line/mod.rs
git commit -m "feat(line): TemplateAdjustments struct + remap/transform/pack math"
```

---

### Task 2: Colour-returning, adjustment-aware, deterministic heatmap sampler

**Files:**
- Modify: `crates/wc-sketches/src/line/heatmap.rs` (the whole sampler)
- Modify: `crates/wc-sketches/src/line/systems/spawn.rs:148-172` (the caller)
- Test: `heatmap.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `TemplateAdjustments`, `remap_weight`, `transform_point` from Task 1.
- Produces:
  - `pub struct SampledParticle { pub pos: Vec2, pub color: [u8;3] }`
  - `pub fn sample_from_heatmap(path: &Path, canvas_w: f32, canvas_h: f32, count: usize, adj: &TemplateAdjustments) -> Vec<SampledParticle>` (replaces the old `-> Vec<Vec2>`). Deterministic: same `(path, canvas, count, adj)` → identical output.

- [ ] **Step 1: Write the failing tests** (add to the existing test module; keep the existing fallback tests, updating them to read `.pos`)

```rust
#[test]
fn sampler_is_deterministic() {
    // Build a tiny gradient PNG in a tempdir.
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("g.png");
    let mut img = image::RgbaImage::new(16, 16);
    for (x, _y, px) in img.enumerate_pixels_mut() {
        let v = (x * 16) as u8;
        *px = image::Rgba([v, v, v, 255]);
    }
    img.save(&path).expect("save");
    let adj = TemplateAdjustments::default();
    let a = sample_from_heatmap(&path, 320.0, 320.0, 200, &adj);
    let b = sample_from_heatmap(&path, 320.0, 320.0, 200, &adj);
    assert_eq!(a.len(), 200);
    let pa: Vec<_> = a.iter().map(|s| s.pos.to_array()).collect();
    let pb: Vec<_> = b.iter().map(|s| s.pos.to_array()).collect();
    assert_eq!(pa, pb, "sampler must be deterministic for stable tuning");
}

#[test]
fn invert_moves_mass_to_dark_region() {
    // Left half black, right half white. Default samples cluster right; invert
    // clusters left.
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("split.png");
    let mut img = image::RgbaImage::new(32, 8);
    for (x, _y, px) in img.enumerate_pixels_mut() {
        let v = if x < 16 { 0 } else { 255 };
        *px = image::Rgba([v, v, v, 255]);
    }
    img.save(&path).expect("save");
    let canvas = 320.0;
    let def = sample_from_heatmap(&path, canvas, canvas, 400, &TemplateAdjustments::default());
    let inv = sample_from_heatmap(&path, canvas, canvas, 400,
        &TemplateAdjustments { invert: true, ..Default::default() });
    let mean = |v: &[SampledParticle]| v.iter().map(|s| s.pos.x).sum::<f32>() / v.len() as f32;
    assert!(mean(&def) > canvas * 0.5, "default mass on the bright (right) half");
    assert!(mean(&inv) < canvas * 0.5, "inverted mass on the dark (left) half");
}

#[test]
fn sampled_color_matches_source_region() {
    // Solid red image -> every sampled colour is ~red.
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("red.png");
    let mut img = image::RgbaImage::new(8, 8);
    for px in img.pixels_mut() { *px = image::Rgba([200, 10, 10, 255]); }
    img.save(&path).expect("save");
    let s = sample_from_heatmap(&path, 64.0, 64.0, 50, &TemplateAdjustments::default());
    for p in &s {
        assert!(p.color[0] > 150 && p.color[1] < 60 && p.color[2] < 60, "got {:?}", p.color);
    }
}
```

- [ ] **Step 2: Run to verify fail** — `cargo test -p wc-sketches --lib line::heatmap 2>&1 | tail -20` → FAIL (signature/type).

- [ ] **Step 3: Implement** — rewrite `try_sample_from_heatmap` and `sample_from_heatmap`:
  - Keep the decode + `resize_exact` downsample to `sample_w × sample_h` and `to_rgba8()`.
  - Build the CDF using `remap_weight(luminance, alpha, adj)` instead of `luminance * alpha`. Keep the Rec.601 luminance computation; pass it to `remap_weight`.
  - Keep a parallel `Vec<[u8;3]>` of each grid cell's RGB (`[px[0],px[1],px[2]]`) so a chosen bin yields its colour.
  - Replace `rand::rng()` with a deterministic seeded RNG: `let mut rng = rand::rngs::StdRng::seed_from_u64(0xL1NE_5EED);` (use `use rand::SeedableRng;`). The i-th draw is then stable across calls.
  - For each of `count`: roll `target`, binary-search the CDF, get bin `idx`, compute window-space `(x,y)` as today, build `Vec2`, then `transform_point(pos, Vec2::new(canvas_w,canvas_h), adj)`, and read the bin colour. Push `SampledParticle { pos: transformed, color }`.
  - Fallback (`fallback_line`) returns `SampledParticle { pos, color: [255,255,255] }` (white = no tint) for each point. Update its signature/return type and the all-zero-weight path.
  - Update `fallback_line` callers and tests to use `.pos`.

- [ ] **Step 4: Update the caller** in `spawn.rs` (the `else` branch, ~line 148):

```rust
        // Window-space positions + per-particle colours from the heatmap sampler.
        let path = Path::new(&settings.spawn_template);
        // Task 7 replaces TemplateAdjustments::default() with the active image's
        // adjustments; for now defaults preserve current behaviour exactly.
        let adj = crate::line::template_adjustments::TemplateAdjustments::default();
        let sampled = sample_from_heatmap(path, w, win_h, count as usize, &adj);
        sampled
            .into_iter()
            .enumerate()
            .map(|(i, sp)| {
                let x = sp.pos.x - half_w;
                let y = -(sp.pos.y - half_h);
                Particle {
                    position: [x, y],
                    velocity: [0.0, 0.0],
                    original_xy: [x, y],
                    alpha: 0.0,
                    age: 0.0,
                    lifespan: attract_lifespan(i as u32),
                    spawn_hash: spawn_hash01(i as u32),
                    spawn_color: crate::line::template_adjustments::pack_rgb8(sp.color),
                    _pad: 0.0,
                }
            })
            .collect()
```

(The `spawn_color`/`_pad` fields land in Task 3; this step compiles only after Task 3's struct change. If implementing strictly in order, leave the `Particle` literal using the old `_pad: [0.0;2]` here and add `spawn_color` in Task 3 — OR do Task 3 first. Recommended: implement Task 3 immediately after Task 2's sampler before re-running the caller. See note below.)

> **Ordering note:** Tasks 2 and 3 both touch the `Particle` literal in `spawn.rs`. Implement Task 2's sampler + tests, then Task 3's struct, then wire the caller's `Particle` literal (Task 3 Step "spawn wiring"). Commit Task 2's sampler with the default-layout branch still passing; the heatmap branch's `Particle` literal is completed in Task 3.

- [ ] **Step 5: Run + commit**

Run: `cargo nextest run -p wc-sketches --lib line::heatmap` → pass. Then:
```bash
git add crates/wc-sketches/src/line/heatmap.rs crates/wc-sketches/src/line/systems/spawn.rs
git commit -m "feat(line): adjustment-aware, deterministic, colour-returning heatmap sampler"
```

---

### Task 3: Per-particle spawn colour through the GPU pipeline

**Files:**
- Modify: `crates/wc-sketches/src/line/particle.rs:16-49` (Particle struct)
- Modify: `assets/shaders/line/simulate.wgsl` (the `struct Particle`)
- Modify: `assets/shaders/line/render.wgsl` (struct + VertexOutput + fragment blend + binding 5)
- Modify: `crates/wc-sketches/src/line/material.rs` (add `template_color` uniform + helper)
- Modify: `crates/wc-sketches/src/line/systems/spawn.rs` (default-layout `Particle` literal + heatmap literal + seed `LineMaterial.template_color`)
- Test: `material.rs` tests (off-sentinel); the shader is verified by a capture in Task review.

**Interfaces:**
- Consumes: `pack_rgb8` (Task 1).
- Produces: `Particle.spawn_color: f32` (packed RGB), `Particle._pad: f32`; `LineMaterial.template_color: Vec4` (`.x` = color_influence 0..1); `LineMaterial::template_color_off() -> Vec4` (`Vec4::ZERO`).

- [ ] **Step 1:** Change the Rust `Particle` struct — replace `pub _pad: [f32; 2]` with:

```rust
    /// Packed RGB8 spawn colour sampled from the template image at this
    /// particle's anchor (white = no tint). Bit-preserved via `pack_rgb8`;
    /// read in `render.wgsl` by `bitcast<u32>`. Never used in float math.
    pub spawn_color: f32,
    /// Padding to keep the struct multiple-of-16 aligned.
    #[allow(clippy::pub_underscore_fields, reason = "GPU struct layout padding must be pub for bytemuck")]
    pub _pad: f32,
```

Keep the `const _: () = { assert!(size_of::<Particle>().is_multiple_of(16)); }` — still 48 bytes.

- [ ] **Step 2:** Update the WGSL `struct Particle` in **both** `simulate.wgsl` and `render.wgsl`: replace `_pad: vec2<f32>,` with `spawn_color: f32,` then `_pad: f32,`.

- [ ] **Step 3:** `material.rs` — add the uniform and helper:

```rust
    /// Per-image colour-influence params. `x` = blend strength 0..1
    /// (`LineTemplateAdjustments[active].color_influence`); `y`/`z`/`w` reserved.
    /// `Vec4::ZERO` (`template_color_off`) makes the fragment tint a bit-exact
    /// no-op: `mix(rgb, rgb*c, 0.0) == rgb`.
    #[uniform(5)]
    pub template_color: Vec4,
```
and `pub fn template_color_off() -> Vec4 { Vec4::ZERO }`, plus a test mirroring `default_attract_color_is_off`. Update the bind-group doc comment to mention `@binding(5)`.

- [ ] **Step 4:** `render.wgsl` — bindings + blend:
  - Add `@group(2) @binding(5) var<uniform> template_color: vec4<f32>;` (doc: x = colour-influence).
  - Add `@location(3) spawn_color: f32,` to `VertexOutput`; in `vertex`, `out.spawn_color = p.spawn_color;`.
  - In `fragment`, before the wake tint, unpack and blend:
    ```wgsl
    let packed = bitcast<u32>(in.spawn_color);
    let img_rgb = vec3<f32>(
        f32((packed >> 16u) & 0xFFu),
        f32((packed >> 8u) & 0xFFu),
        f32(packed & 0xFFu)) / 255.0;
    let base = mix(texel.rgb, texel.rgb * img_rgb, template_color.x);
    let wake = smoothstep(WAKE_SPEED_LO, WAKE_SPEED_HI, in.speed) * attract_color.x;
    let rgb = mix(base, base * WAKE_TINT, wake);
    ```
  - (Replaces the current `let rgb = mix(texel.rgb, texel.rgb * WAKE_TINT, wake);`.)

- [ ] **Step 5:** `spawn.rs` — set `spawn_color` in both `Particle` literals (`pack_rgb8([255,255,255])` for the default horizontal-line layout; `pack_rgb8(sp.color)` for the heatmap branch) and `_pad: 0.0`. Seed the material in the `LineMaterial { … }` construction (search the spawn for `solid_color:`/`attract_color:`) with `template_color: LineMaterial::template_color_off(),`.

- [ ] **Step 6:** Run + capture + commit.
  - `cargo nextest run -p wc-sketches --features templates` → pass.
  - `cargo rund` would render; for regression use `cargo xtask capture <line scenario>` and confirm the default (no-template) render is unchanged (color_influence uniform 0 ⇒ bit-exact). Review the PNG.
```bash
git add crates/wc-sketches/src/line/particle.rs crates/wc-sketches/src/line/material.rs crates/wc-sketches/src/line/systems/spawn.rs assets/shaders/line/simulate.wgsl assets/shaders/line/render.wgsl
git commit -m "feat(line): per-particle spawn colour + colour-influence render uniform"
```

---

### Task 4: `LineTemplateAdjustments` registered resource + persistence

**Files:**
- Create: `crates/wc-sketches/src/line/template_adjustments_store.rs` (the map resource + `SketchSettings` impl + accessors)
- Modify: `crates/wc-sketches/src/line/mod.rs` (gated `mod` + `register_sketch_settings` in `build`)
- Modify: `crates/wc-sketches/Cargo.toml` (add `templates` feature)
- Modify: `crates/waveconductor/Cargo.toml` (enable `wc-sketches/templates`)
- Test: integration test `crates/wc-sketches/tests/line_template_adjustments_persist.rs`

**Interfaces:**
- Consumes: `TemplateAdjustments` (Task 1).
- Produces:
  - `#[derive(Resource, Reflect, Serialize, Deserialize, Clone, Debug, Default)] #[reflect(Resource, Default)] pub struct LineTemplateAdjustments { pub map: std::collections::HashMap<String, TemplateAdjustments> }`
  - `impl SketchSettings for LineTemplateAdjustments { const STORAGE_KEY = "line-template-adjustments"; fn settings_def() -> Vec<SettingDef> { Vec::new() } }`
  - `pub fn hash_of_path(spawn_template: &str) -> Option<String>` — `Path::new(s).file_stem()?.to_str().map(str::to_owned)`, `None` if empty.
  - `impl LineTemplateAdjustments { pub fn get(&self, hash: &str) -> TemplateAdjustments /* cloned or default */; pub fn entry_mut(&mut self, hash: &str) -> &mut TemplateAdjustments /* default-insert */ }`

- [ ] **Step 1:** `crates/wc-sketches/Cargo.toml` — add under `[features]`: `templates = ["wc-core/templates"]`. `crates/waveconductor/Cargo.toml` — change the `wc-sketches` dep to `features = ["templates"]` (mirror the existing `wc-core` templates enablement).

- [ ] **Step 2: Write the failing test**

```rust
//! LineTemplateAdjustments persists its hash-keyed map through the central
//! settings persistence (no separate file).
#![cfg(feature = "templates")]
#![allow(clippy::expect_used, reason = "test code")]
use wc_sketches::line::template_adjustments::TemplateAdjustments;
use wc_sketches::line::template_adjustments_store::LineTemplateAdjustments;
use wc_core::settings::persistence;
use std::sync::Mutex;

fn with_temp_dir<R>(f: impl FnOnce() -> R) -> R { /* same pattern as line_settings_persist.rs */ }

#[test]
fn adjustments_map_round_trips() {
    with_temp_dir(|| {
        let mut s = LineTemplateAdjustments::default();
        s.map.insert("deadbeef".into(), TemplateAdjustments { gamma: 2.0, color_influence: 0.5, ..Default::default() });
        persistence::save(&s);
        let loaded = persistence::load::<LineTemplateAdjustments>();
        let got = loaded.map.get("deadbeef").expect("entry persisted");
        assert!((got.gamma - 2.0).abs() < 1e-6);
        assert!((got.color_influence - 0.5).abs() < 1e-6);
    });
}
```

- [ ] **Step 3:** Implement the resource + `SketchSettings` impl + helpers (manual impl, empty `settings_def`). `hash_of_path`, `get`, `entry_mut` as in Interfaces. Add `pub mod template_adjustments_store;` (gated `#[cfg(feature="templates")]`) to `mod.rs`, and in `LinePlugin::build` (gated): `app.register_sketch_settings::<template_adjustments_store::LineTemplateAdjustments>();`.

- [ ] **Step 4:** Run → `cargo nextest run -p wc-sketches --features templates --test line_template_adjustments_persist` → pass.

- [ ] **Step 5: Commit**
```bash
git add crates/wc-sketches/src/line/template_adjustments_store.rs crates/wc-sketches/src/line/mod.rs crates/wc-sketches/Cargo.toml crates/waveconductor/Cargo.toml crates/wc-sketches/tests/line_template_adjustments_persist.rs
git commit -m "feat(line): registered LineTemplateAdjustments map (per-image persistence)"
```

---

### Task 5: `CustomDockSections` render-only hook in the settings dock

**Files:**
- Create: `crates/wc-core/src/settings/custom_section.rs` (the resource + registration ext)
- Modify: `crates/wc-core/src/settings/mod.rs` (`pub mod custom_section;` + re-export; init resource in `SettingsPlugin`)
- Modify: `crates/wc-core/src/settings/panel_user.rs` (call registered sections in `draw_user_panel` after the reflected sections for the active tab)
- Test: `custom_section.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Produces:
  - `pub type DockSectionFn = fn(&mut bevy::prelude::World, &mut bevy_egui::egui::Ui, &crate::ui::style::OverlayStyle);`
  - `#[derive(Resource, Default)] pub struct CustomDockSections { entries: Vec<(SettingsTab, DockSectionFn)> }` with `pub fn register(&mut self, tab: SettingsTab, f: DockSectionFn)` and `pub fn for_tab(&self, tab: SettingsTab) -> impl Iterator<Item = DockSectionFn> + '_`.
  - `pub trait RegisterDockSectionExt { fn register_dock_section(&mut self, tab: SettingsTab, f: DockSectionFn) -> &mut Self; }` impl for `App`.
- Rustdoc on `CustomDockSections` MUST state: *render-only escape hatch for data the `SettingDef` table cannot express; standard flat settings MUST use `#[derive(SketchSettings)]`; the backing state must be a registered resource so persistence/autosave/restart stay centralized.*

- [ ] **Step 1: Test** — a registered section's fn is stored and returned for its tab only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SettingsTab;
    fn noop(_: &mut bevy::prelude::World, _: &mut bevy_egui::egui::Ui, _: &crate::ui::style::OverlayStyle) {}
    #[test]
    fn sections_are_tab_scoped() {
        let mut s = CustomDockSections::default();
        s.register(SettingsTab::Sketch, noop);
        assert_eq!(s.for_tab(SettingsTab::Sketch).count(), 1);
        assert_eq!(s.for_tab(SettingsTab::System).count(), 0);
    }
}
```
(Use the actual `SettingsTab` variant names — check `panel_user.rs`/the tab enum; replace `Sketch`/`System` with real variants.)

- [ ] **Step 2:** Run → FAIL.

- [ ] **Step 3:** Implement the resource/ext. In `SettingsPlugin` build, `app.init_resource::<CustomDockSections>();`. In `draw_user_panel`, after the `for key in &keys { … render_section_by_key … }` loop inside the scroll area (panel_user.rs ~line 280-296), snapshot the section fns for `selected_tab` (to avoid borrowing the resource across the `world` re-entry) and call each: allocate them into a `SmallVec`/`Vec<DockSectionFn>` from `world.get_resource::<CustomDockSections>()`, drop the borrow, then `for f in fns { f(world, ui, &style); }`. Function pointers are `Copy`, so the snapshot is cheap and releases the resource borrow before re-entering `world`.

- [ ] **Step 4:** Run → pass. `cargo clippy -p wc-core --features templates --all-targets -- -D warnings` → clean.

- [ ] **Step 5: Commit**
```bash
git add crates/wc-core/src/settings/custom_section.rs crates/wc-core/src/settings/mod.rs crates/wc-core/src/settings/panel_user.rs
git commit -m "feat(settings): render-only CustomDockSections hook for sketch-contributed UI"
```

---

### Task 6: Line "Template adjustments" custom section (draggable % sliders)

**Files:**
- Create: `crates/wc-sketches/src/line/template_adjustments_panel.rs` (the `DockSectionFn`)
- Modify: `crates/wc-sketches/src/line/mod.rs` (gated `register_dock_section` in `build`)
- Test: `template_adjustments_panel.rs` `#[cfg(test)] mod tests` for the percent↔internal mapping helpers (the egui draw is verified by smoke).

**Interfaces:**
- Consumes: `LineTemplateAdjustments`, `hash_of_path`, `entry_mut` (Task 4); `LineSettings.spawn_template`; `CustomDockSections`/`register_dock_section` (Task 5); `OverlayStyle`.
- Produces: `pub fn render_template_adjustments(world: &mut World, ui: &mut egui::Ui, style: &OverlayStyle)` (a `DockSectionFn`), plus pure helpers `fn pct_slider(ui, label, value: &mut f32, range_pct: RangeInclusive<f32>, neutral_label) -> bool` and the percent mappings, unit-tested.

- [ ] **Step 1: Test the percent mappings**

```rust
#[test]
fn percent_round_trips() {
    // gamma 1.0 <-> 100%, scale 2.0 <-> 200%, position 0.0 <-> 0%.
    assert!((pct_to_internal(100.0) - 1.0).abs() < 1e-6);
    assert!((internal_to_pct(1.0) - 100.0).abs() < 1e-6);
    assert!((pct_to_internal(0.0) - 0.0).abs() < 1e-6);
}
```
where `pct_to_internal(p) = p / 100.0` and `internal_to_pct(v) = v * 100.0`.

- [ ] **Step 2:** Run → FAIL.

- [ ] **Step 3:** Implement `render_template_adjustments`:
  - `let spawn = world.resource::<LineSettings>().spawn_template.clone();`
  - `let Some(hash) = hash_of_path(&spawn) else { return; };` (renders nothing with no active template).
  - `world.resource_scope(|world, mut adj_store: Mut<LineTemplateAdjustments>| { let adj = adj_store.entry_mut(&hash); … render sliders mutating `adj` … });` — mutating the `Mut` arms autosave + the re-seed change-gate.
  - Section header `"TEMPLATE ADJUSTMENTS"` (uppercase, matching `render_section_by_key`'s header style). For each numeric knob a draggable `egui::Slider` over its percent range with `%` suffix, writing back via `internal = pct/100`; e.g. white/black 0–100, gamma/scale 10–400, position −100–100, color 0–100; X and Y are **separate sliders** for position and scale. A checkbox for `invert`. Use `style.text_primary`/accent for labels to match the dock.
- [ ] **Step 4:** Register (gated) in `LinePlugin::build`: `app.register_dock_section(SettingsTab::<the Line tab>, template_adjustments_panel::render_template_adjustments);` (use the real tab variant the Line settings render under — confirm via `tab_for_storage_key`).
- [ ] **Step 5:** Run tests + `cargo rund` smoke (manual). Commit:
```bash
git add crates/wc-sketches/src/line/template_adjustments_panel.rs crates/wc-sketches/src/line/mod.rs
git commit -m "feat(line): template-adjustments dock section with draggable % sliders"
```

---

### Task 7: Spawn reads active adjustments + in-place debounced re-seed

**Files:**
- Modify: `crates/wc-sketches/src/line/systems/spawn.rs` (read the active adjustments instead of `default()`)
- Create: `crates/wc-sketches/src/line/systems/reseed.rs` (the re-seed system)
- Modify: `crates/wc-sketches/src/line/systems/mod.rs` + `crates/wc-sketches/src/line/mod.rs` (gated system registration)
- Test: `reseed.rs` unit test for the position-field change predicate.

**Interfaces:**
- Consumes: `LineTemplateAdjustments`, `sample_from_heatmap`, `SampledParticle`, `pack_rgb8`, the `LineSimParams`/particle buffer handle (read how `spawn_line` stores the `Handle<ShaderStorageBuffer>` — it lives on `LineSimParams`/the `LineRoot`).
- Produces: `fn position_fields_changed(prev: &TemplateAdjustments, curr: &TemplateAdjustments) -> bool` (all fields except `color_influence`); `fn reseed_on_adjustments_change(...)` system.

- [ ] **Step 1:** Make `spawn_line` (gated) resolve the active adjustments: `let adj = world-or-Res<LineTemplateAdjustments>` keyed by `hash_of_path(&settings.spawn_template)`, falling back to default. Since `spawn_line` is a system, add `adjustments: Option<Res<LineTemplateAdjustments>>` param (gated) and compute `let adj = adjustments.and_then(|a| hash_of_path(&settings.spawn_template).map(|h| a.get(&h))).unwrap_or_default();`. Replace the `TemplateAdjustments::default()` from Task 2.
- [ ] **Step 2: Test** the predicate:
```rust
#[test]
fn only_color_influence_change_is_not_a_position_change() {
    let a = TemplateAdjustments::default();
    let b = TemplateAdjustments { color_influence: 0.9, ..Default::default() };
    assert!(!position_fields_changed(&a, &b));
    let c = TemplateAdjustments { gamma: 2.0, ..Default::default() };
    assert!(position_fields_changed(&a, &c));
}
```
- [ ] **Step 3:** Implement `reseed_on_adjustments_change`:
  - Params: `Res<LineTemplateAdjustments>` (gated), `Res<LineSettings>`, `Single<&Window>`, `ResMut<Assets<ShaderStorageBuffer>>`, the resource holding the particle buffer handle, a `Local<Option<TemplateAdjustments>>` snapshot, a `Local<Option<Duration>>` debounce stamp, `Res<Time>`.
  - Run condition: `sketch_active(AppState::Line)` AND `resource_exists::<LineTemplateAdjustments>`.
  - Logic: resolve active `adj`; if `position_fields_changed(prev, &adj)`, set the debounce stamp; once `~200ms` quiescent, re-run `sample_from_heatmap` with the current window + count (reuse the count formula from `spawn_line` — extract a shared `fn particle_count(density, width) -> u32` to avoid divergence), rebuild the `Vec<Particle>` (same conversion as spawn), and write it into the existing buffer via `buffers.get_mut(handle).set_data(bytemuck::cast_slice(&particles))`. Update the `Local` snapshot.
  - If `spawn_template` is empty, no-op.
- [ ] **Step 4:** Register (gated) in `Update` alongside the other Line systems, run_if `sketch_active(AppState::Line)`.
- [ ] **Step 5:** Run unit test + `cargo rund` smoke (drag a slider → particles redistribute in place, no fade). Commit:
```bash
git add crates/wc-sketches/src/line/systems/reseed.rs crates/wc-sketches/src/line/systems/spawn.rs crates/wc-sketches/src/line/systems/mod.rs crates/wc-sketches/src/line/mod.rs
git commit -m "feat(line): in-place debounced re-seed on adjustment change"
```

---

### Task 8: Colour-influence uniform write system

**Files:**
- Create: `crates/wc-sketches/src/line/systems/color_influence.rs`
- Modify: `crates/wc-sketches/src/line/systems/mod.rs` + `mod.rs` (gated registration)
- Test: unit test the value the system would write (extract `fn influence_for(settings, store) -> f32`).

**Interfaces:**
- Consumes: `LineTemplateAdjustments`, `LineSettings`, `LineMaterial` (handle on `LineRoot`/material assets), `LineMaterial::template_color_off`.
- Produces: `fn influence_for(spawn_template: &str, store: &LineTemplateAdjustments) -> f32` (0 if no template), `fn drive_color_influence(...)` system.

- [ ] **Step 1: Test**
```rust
#[test]
fn influence_zero_when_no_template() {
    let store = LineTemplateAdjustments::default();
    assert_eq!(influence_for("", &store), 0.0);
}
#[test]
fn influence_reads_active_entry() {
    let mut store = LineTemplateAdjustments::default();
    store.map.insert("h".into(), TemplateAdjustments { color_influence: 0.7, ..Default::default() });
    assert!((influence_for("/x/h.png", &store) - 0.7).abs() < 1e-6);
}
```
- [ ] **Step 2:** Run → FAIL.
- [ ] **Step 3:** Implement `drive_color_influence`: each frame (run_if `sketch_active(AppState::Line)`), compute `influence_for(&settings.spawn_template, &store)` and write `mat.template_color = Vec4::new(influence, 0.0, 0.0, 0.0)` into the `LineMaterial` asset (look up the handle the same way `drive_attract_color` does — mirror that system's material-handle access). When no store/template, write `template_color_off()`.
- [ ] **Step 4:** Register (gated). Run unit test + smoke (drag colour-influence → particles tint live, no re-seed). Commit:
```bash
git add crates/wc-sketches/src/line/systems/color_influence.rs crates/wc-sketches/src/line/systems/mod.rs crates/wc-sketches/src/line/mod.rs
git commit -m "feat(line): live colour-influence uniform driver"
```

---

### Task 9: Prune adjustments for deleted images

**Files:**
- Create: `crates/wc-sketches/src/line/systems/prune_adjustments.rs`
- Modify: `crates/wc-sketches/src/line/systems/mod.rs` + `mod.rs` (gated registration)
- Test: unit test the pure prune function.

**Interfaces:**
- Consumes: `LineTemplateAdjustments`, `wc_core::templates::resource::TemplateLibrary` (available because `templates` is on).
- Produces: `fn prune(map: &mut HashMap<String, TemplateAdjustments>, live_hashes: &HashSet<&str>) -> bool` (returns whether anything was removed); `fn prune_orphan_adjustments(...)` system.

- [ ] **Step 1: Test**
```rust
#[test]
fn prune_drops_orphans_only() {
    let mut map = HashMap::new();
    map.insert("keep".to_string(), TemplateAdjustments::default());
    map.insert("gone".to_string(), TemplateAdjustments::default());
    let live: HashSet<&str> = ["keep"].into_iter().collect();
    assert!(prune(&mut map, &live));
    assert!(map.contains_key("keep") && !map.contains_key("gone"));
}
```
- [ ] **Step 2:** Run → FAIL.
- [ ] **Step 3:** Implement `prune` (retain entries whose hash is in `live_hashes`) and `prune_orphan_adjustments`: run_if `resource_exists::<TemplateLibrary>` AND (`is_resource_changed::<TemplateLibrary>` OR a one-shot startup run); build `live_hashes` from `library.entries.iter().map(|e| e.hash.as_str())`, call `prune` on `LineTemplateAdjustments.map` (via `ResMut`, only take `&mut` when a removal is needed to avoid arming autosave every frame — check membership first, mutate only if `prune` would change something). Register in `Update` (gated).
- [ ] **Step 4:** Run unit test. Commit:
```bash
git add crates/wc-sketches/src/line/systems/prune_adjustments.rs crates/wc-sketches/src/line/systems/mod.rs crates/wc-sketches/src/line/mod.rs
git commit -m "feat(line): prune per-image adjustments when a template is deleted"
```

---

### Task 10: Full verification

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets --all-features --workspace -- -D warnings` (the binding-5 material, the WGSL, the gated code under `--all-features`)
- [ ] `cargo nextest run --workspace --all-features` + `cargo test --doc --workspace`
- [ ] feature-OFF build: `cargo build -p wc-sketches` (no `templates`) compiles — the always-on sampler/GPU code defaults to identity/off.
- [ ] `cargo xtask capture <line scenario>` with a known template + non-default adjustments; review the PNG (density reshapes, colour tints).
- [ ] `cargo deny check` + `cargo xtask check-secrets`.
- [ ] Commit any gate-fix work.

---

## Notes for the implementer

- **No-regression is the load-bearing invariant.** Default adjustments must leave the sketch bit-identical: the remap `/255` change means the *absolute* CDF weights differ from the old `luminance*alpha`, but the *relative* distribution (what inverse-CDF sampling cares about) is unchanged at defaults, and the colour uniform at 0 is a provable no-op. Keep the default-layout (no-template) path untouched except for `spawn_color = white`.
- **Particle literal lives in two places** (default + heatmap branches of `spawn_line`, and the re-seed builder). Extract a `fn make_particle(i, x, y, color_packed) -> Particle` helper to keep them in sync (DRY).
- **Count formula** is duplicated between spawn and re-seed — extract `fn particle_count(density: f32, width: f32) -> u32` (the clamped `(density*width).round()`), and reuse.
- **Confirm real enum/tab names** before Task 5/6: the `SettingsTab` variants and which tab Line settings render under (`tab_for_storage_key`). The plan uses placeholders `SettingsTab::Sketch`.
