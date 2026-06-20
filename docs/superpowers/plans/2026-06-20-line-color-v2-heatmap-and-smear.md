# Line Color v2 (Heatmap Palette + Configurable Smear) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Redesign the Line particle palette into value-normalized Turbo heatmaps (Velocity = per-particle speed; Spectrum = center-peak creation index), drop the time-cycle strobe, and make the gravity-smear fringe coloring configurable while keeping its >1 additive glow.

**Architecture:** Two independent components. **Component 1** (Tasks 1–2): the per-particle palette in `render.wgsl` + its `LineSettings`/driver — a uniform-branch heatmap, value-normalized so it never darkens. **Component 2** (Task 3): the smear's two chromatic fringe end-tints become `color × gain` settings written into `LinePostParams`, consumed by `gravity.wgsl`. Task 4 is the gate run + live tuning.

**Tech Stack:** Rust, Bevy 0.18 (`Material2d`, `AsBindGroup`, `bevy_reflect`), WGSL, `serde`/`toml`.

## Global Constraints

Copied from the spec / AGENTS.md; every task implicitly includes these.

- **No new dependencies.** Reuse what's in the graph.
- **Dev run:** `cargo rund`. Plain fallback: `cargo run -p waveconductor`.
- **Verification gates (full run is Task 4):** `cargo fmt --all -- --check`; `cargo clippy --all-targets --all-features --workspace -- -D warnings`; `cargo nextest run --workspace --all-features`; `cargo test --doc --workspace`; `cargo doc --no-deps --workspace --document-private-items`; `cargo deny check`; `cargo xtask check-secrets`.
- **Per-task focused verification:** `cargo test -p wc-sketches --lib <module>`; `cargo fmt --all`; `cargo clippy -p wc-sketches --tests -- -D warnings`; `cargo build -p waveconductor` for shader/plugin changes.
- `///` on public items, `//!` on modules, inline `//` for shader/math contracts. Never strip comments.
- No `unwrap()`/`expect()`/`panic!` in non-test code (test code may, behind `#[allow(...)]`). No `as` casts where `From`/`TryFrom` works — **exception:** GPU-layout `as u64` casts already present in `post_process.rs` are the established pattern; match it.
- Shaders live in `assets/shaders/`; never inline WGSL in Rust.
- **Off path stays bit-exact:** palette `Off` (`palette_params.x == 0`) skips the uniform branch; rendering is unchanged from pre-feature.
- **Smear default preserves the look:** default `color × gain` reproduces the legacy fringe factors within float epsilon (capture tolerance is mean-abs-diff ≤ 6.0).
- Pre-release, one operator: no migration shims for the `Scatter`→`Spectrum` rename or the dropped `palette_cycle` (serde ignores unknown keys; missing keys fall back to defaults).

---

## File Structure

**Component 1 (Tasks 1–2):**
- `crates/wc-sketches/src/line/settings.rs` — rename `Scatter`→`Spectrum`; remove `palette_cycle`; redoc `palette_scale`.
- `crates/wc-sketches/src/line/systems/palette.rs` — `palette_params()` drops the `cycle` arg; `drive_palette` reads `palette_scale`.
- `crates/wc-sketches/src/line/material.rs` — `palette_params` doc (z = spread, no cycle).
- `assets/shaders/line/render.wgsl` — Turbo + value-normalize; Velocity clamp; Spectrum tent via `arrayLength`+index; remove `globals`.

**Component 2 (Task 3):**
- `crates/wc-sketches/src/line/settings.rs` — add `smear_outgoing_color`, `smear_incoming_color`, `smear_chroma_gain`.
- `crates/wc-sketches/src/line/post_process.rs` — add two `[f32;4]` tint fields to `LinePostParams`.
- `crates/wc-sketches/src/line/systems/sim_params.rs` — add `bake_smear_tints`; call it in `update_sim_params`.
- `crates/wc-sketches/src/line/screensaver/mod.rs` — call `bake_smear_tints`.
- `assets/shaders/line/gravity.wgsl` — `PostParams` mirror; per-step factor from end-tint.

---

## Task 1: Component 1 — palette settings + driver (Rust)

**Files:**
- Modify: `crates/wc-sketches/src/line/settings.rs`
- Modify: `crates/wc-sketches/src/line/systems/palette.rs`
- Modify: `crates/wc-sketches/src/line/material.rs`

**Interfaces:**
- Produces: `PaletteMode { Off, Velocity, Spectrum }` (`Spectrum.index() == 2.0`); `palette_params(mode: PaletteMode, strength: f32, scale: f32) -> Vec4` packing `(index, strength.clamp(0,1), scale.max(0), 0.0)`.

- [ ] **Step 1: Update the failing tests first**

In `settings.rs` tests: change the `palette_mode_index_encodes_uniform_channel` test's `PaletteMode::Scatter` to `PaletteMode::Spectrum`; change `palette_mode_setting_is_enum_combobox` expected variants to `&["Off", "Velocity", "Spectrum"]`; in `missing_field_preserves_sibling_values` **remove** the `parsed.palette_cycle` assertion block and add `palette_cycle = 0.03` into the legacy TOML string (to prove an unknown key is now ignored), keeping the `palette_scale` assertion.

In `palette.rs` tests: rename `scatter_mode_index_is_two` → `spectrum_mode_index_is_two` using `PaletteMode::Spectrum`; update every `palette_params(...)` call (in `off_mode_is_zero_regardless_of_knobs`, `velocity_mode_packs_channels`, `out_of_range_inputs_clamp`, `spectrum_mode_index_is_two`, and the respawn test's `expected`) to the **3-arg** form `palette_params(mode, strength, scale)` — drop the cycle arg. In `velocity_mode_packs_channels`, the packed `Vec4` is now `(1.0, strength, scale, 0.0)`: assert `p.z == scale` (the spread) and `p.w == 0.0`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p wc-sketches --lib line:: 2>&1 | tail -20`
Expected: compile errors (`no variant Spectrum`, `palette_params` arity mismatch).

- [ ] **Step 3: Rename the enum variant and update `index()`**

In `settings.rs`, rename the `Scatter` variant to `Spectrum` with a new doc comment, and update the `index()` match arm:

```rust
    /// Hue keyed to the particle's creation index: a center-peak heatmap that is
    /// hot at the middle of the spawn list and cools toward both ends.
    Spectrum,
```

```rust
            PaletteMode::Spectrum => 2.0,
```

(Also update the `index()` rustdoc line `Scatter → 2.0` to `Spectrum → 2.0`.)

- [ ] **Step 4: Remove `palette_cycle`; redoc `palette_scale`**

In `settings.rs`, delete the entire `palette_cycle` field (its `#[setting(...)]`, `#[serde(...)]`, and `pub palette_cycle: f32,`) and the `default_palette_cycle()` fn. Remove the `//! - **`palette_cycle`** ...` module-doc bullet. Replace the `palette_scale` field doc with:

```rust
    /// Per-mode palette tuning. `Velocity`: speed sensitivity — roughly
    /// `180 / scale` px/s maps to the hot end. `Spectrum`: tent sharpness — the
    /// `pow` exponent on the center-peak ramp (>1 narrows the hot center, <1
    /// widens it). Ignored when `palette_mode` is `Off`. Dev knob.
```

Update the `//! - **`palette_scale`** ...` module-doc bullet to match.

- [ ] **Step 5: Update `palette_params()` and `drive_palette`**

In `palette.rs`, change the helper to drop `cycle` and pack `scale` into `z`:

```rust
/// Pack the palette settings into the `palette_params` uniform value
/// (`x` = mode index, `y` = strength, `z` = spread, `w` = reserved). `Off`
/// returns [`LineMaterial::palette_off`] (`Vec4::ZERO`) regardless of the other
/// knobs, so the shader's uniform-mode branch is skipped and color is the
/// pre-palette path. Strength is clamped to `0..=1`; spread is clamped
/// non-negative so a stray value can't invert the mapping.
#[must_use]
pub fn palette_params(mode: PaletteMode, strength: f32, scale: f32) -> Vec4 {
    if mode == PaletteMode::Off {
        return LineMaterial::palette_off();
    }
    Vec4::new(mode.index(), strength.clamp(0.0, 1.0), scale.max(0.0), 0.0)
}
```

In `drive_palette`, change the call to drop `palette_cycle`:

```rust
    let target = palette_params(
        settings.palette_mode,
        settings.palette_strength,
        settings.palette_scale,
    );
```

Update the module `//!` doc and the `drive_palette`/`palette_params` doc comments: replace the `z = cycle, w = spread` description with `z = spread, w = reserved`, and delete the "time animation reads `globals.time`" sentence (the cycle is gone).

- [ ] **Step 6: Update the material uniform doc**

In `material.rs`, change the `palette_params` field doc so `z` = spread and there is no cycle/`globals.time` mention:

```rust
    /// Psychedelic palette params: `x` = mode index (`PaletteMode::index()`:
    /// `0` Off / `1` Velocity / `2` Spectrum), `y` = crossfade strength `0..=1`,
    /// `z` = palette spread (per-mode tuning), `w` = reserved. Driven by
    /// [`crate::line::systems::palette::drive_palette`]. [`Vec4::ZERO`]
    /// ([`Self::palette_off`]) sets mode `0`, so the render shader's uniform-mode
    /// branch is skipped and color is the pre-palette path bit-exactly.
```

- [ ] **Step 7: Run tests + build**

Run: `cargo test -p wc-sketches --lib line:: 2>&1 | tail -20`
Expected: PASS (settings + palette tests, incl. the respawn regression test, all green).
Run: `cargo build -p waveconductor 2>&1 | tail -3`
Expected: builds. (The shader still reads the old `z`/`w` meaning, but palette defaults to `Off` so the branch is skipped — no runtime effect; Task 2 makes the shader read the new packing.)
Run: `cargo fmt --all` then `cargo clippy -p wc-sketches --tests -- -D warnings 2>&1 | tail -3`
Expected: clean.

- [ ] **Step 8: Commit**

```bash
git add crates/wc-sketches/src/line/settings.rs crates/wc-sketches/src/line/systems/palette.rs crates/wc-sketches/src/line/material.rs
git commit -F - <<'EOF'
refactor(line): palette Scatter->Spectrum, drop time-cycle

Rename the palette mode variant, remove palette_cycle (and globals-time
strobe), and repack palette_params to (mode, strength, spread, _). Shader
reads the new packing in the next task; palette defaults Off so the interim
is inert.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
EOF
```

---

## Task 2: Component 1 — Turbo heatmap shader

**Files:**
- Modify: `assets/shaders/line/render.wgsl`

**Interfaces:**
- Consumes: `palette_params = (mode, strength, spread, _)` (Task 1); `particles` storage buffer (`@binding(0)`); `Particle.spawn_hash` is no longer used by the palette.

- [ ] **Step 1: Remove the `globals` import**

Change the import line back to `view`-only:

```wgsl
#import bevy_sprite::mesh2d_view_bindings::view
```

- [ ] **Step 2: Replace the `scatter` interpolant with a normalized creation index**

In `struct VertexOutput`, replace the `scatter` member with:

```wgsl
    // Normalized creation index (particle buffer index / (count-1)), 0..1, for
    // the Spectrum palette. Flat: a per-particle constant.
    @location(4) @interpolate(flat) index_norm: f32,
```

In the `vertex` fn, replace `out.scatter = p.spawn_hash;` with (computing the index from the built-in index and the storage-buffer length — no count uniform needed):

```wgsl
    // arrayLength gives the live particle count; guard the (count-1) divide.
    let count = f32(arrayLength(&particles));
    out.index_norm = f32(particle_index) / max(count - 1.0, 1.0);
```

- [ ] **Step 3: Replace the IQ `palette()` with Turbo + value-normalize**

Replace the entire `fn palette(t: f32)` definition with:

```wgsl
// Turbo colormap (Anton Mikhailov / Google), degree-6 polynomial approximation —
// texture-free, blue (t=0) -> green (t=0.5) -> red (t=1). Output clamped to 0..1.
fn turbo(t: f32) -> vec3<f32> {
    let x = clamp(t, 0.0, 1.0);
    let c0 = vec3<f32>(0.1140890109226559, 0.06288340699912215, 0.2248337216805064);
    let c1 = vec3<f32>(6.716419496985708, 3.182286745507602, 7.571581586103393);
    let c2 = vec3<f32>(-66.09402360453038, -4.9279827041226, -10.09439367561635);
    let c3 = vec3<f32>(228.7660791526501, 25.04986699771073, -91.54105330182436);
    let c4 = vec3<f32>(-334.8351565777451, -69.31749712757485, 288.5858850615712);
    let c5 = vec3<f32>(218.7637218434795, 67.52150567819112, -305.2045772184957);
    let c6 = vec3<f32>(-52.88903478218835, -21.54527364654712, 110.5174647748972);
    let rgb = c0 + x * (c1 + x * (c2 + x * (c3 + x * (c4 + x * (c5 + x * c6)))));
    return clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
}

// Value-normalize: divide by the max channel so the palette supplies HUE only,
// never brightness. Turbo's dark cool end (~(0.19,0.07,0.23)) becomes a bright
// blue, so the star keeps supplying brightness and no particle crushes to dark.
fn value_normalize(c: vec3<f32>) -> vec3<f32> {
    let m = max(c.r, max(c.g, c.b));
    return c / max(m, 1e-4);
}
```

- [ ] **Step 4: Rewrite the palette branch (Velocity clamp + Spectrum tent)**

Replace the existing `if (palette_params.x > 0.5) { ... }` block (the one using `cycle`, `globals.time`, `in.scatter`, and `palette(t)`) with:

```wgsl
    var base = img_base;
    if (palette_params.x > 0.5) {
        let strength = palette_params.y;
        let spread = palette_params.z;
        var t: f32;
        if (palette_params.x < 1.5) {
            // Velocity: clamped cool->hot; ~180/spread px/s maps to full hot.
            t = clamp(in.speed * spread / 180.0, 0.0, 1.0);
        } else {
            // Spectrum: center-peak tent over creation index, sharpened by spread.
            let tent = 1.0 - abs(2.0 * in.index_norm - 1.0);
            t = pow(tent, spread);
        }
        // Palette = hue only (value-normalized); star supplies brightness.
        let pal_base = texel.rgb * value_normalize(turbo(t));
        base = mix(img_base, pal_base, strength);
    }
```

(Leave the `img_base`, `wake`, `tinted`, `rgb`, and `return` lines unchanged.)

- [ ] **Step 5: Build and confirm the shader compiles**

Run: `cargo run -p waveconductor 2>&1 | tail -5`
Expected: builds; no WGSL validation panic in the log (let it start, then stop it).

- [ ] **Step 6: Off-path no-regression capture**

Run: `cargo xtask capture line-synthetic --json 2>&1 | tail -20`
Expected: exit 0. Palette defaults to `Off`, so the uniform branch is skipped and the render is unchanged. The `index_norm` interpolant is computed but unused at `Off`. If any frame is flagged as a regression (not merely `NEW`), Read it — the off path must be bit-exact; investigate rather than adopting a baseline.

- [ ] **Step 7: Visually confirm the ON path (temporary default flip, then revert)**

Temporarily default `palette_mode` to `Velocity` to exercise the heatmap in the clean-config capture, then revert:

```bash
python3 - <<'PY'
import pathlib
p = pathlib.Path("crates/wc-sketches/src/line/settings.rs")
s = p.read_text()
s = s.replace("default = PaletteMode::Off, ty = Enum", "default = PaletteMode::Velocity, ty = Enum")
s = s.replace("fn default_palette_mode() -> PaletteMode {\n    PaletteMode::Off\n}", "fn default_palette_mode() -> PaletteMode {\n    PaletteMode::Velocity\n}")
p.write_text(s); print("temp -> Velocity")
PY
cargo xtask capture line-synthetic --json >/dev/null 2>&1
git checkout -- crates/wc-sketches/src/line/settings.rs
```

Read `target/capture/line-synthetic/frame_0240.png`: confirm particles show the Turbo ramp (cool→hot by speed) and are **bright, not dark** (the value-normalization guarantee). If they read dark/broken, the value-normalize or branch is wrong — fix before committing. Report what you saw.

- [ ] **Step 8: Commit**

```bash
git add assets/shaders/line/render.wgsl
git commit -F - <<'EOF'
feat(line): value-normalized Turbo heatmap palette modes

Velocity keys each particle's hue to its own |velocity| (clamped cool->hot);
Spectrum keys it to a center-peak tent over creation index (via arrayLength).
Turbo is value-normalized so the palette supplies hue only and never darkens
particles. Drops the globals.time strobe. Off path unchanged.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
EOF
```

---

## Task 3: Component 2 — configurable smear coloring

**Files:**
- Modify: `crates/wc-sketches/src/line/settings.rs`
- Modify: `crates/wc-sketches/src/line/post_process.rs`
- Modify: `crates/wc-sketches/src/line/systems/sim_params.rs`
- Modify: `crates/wc-sketches/src/line/screensaver/mod.rs`
- Modify: `assets/shaders/line/gravity.wgsl`

**Interfaces:**
- Consumes: `LineSettings` (Task 1's revised struct); `LinePostParams`, `bake_post_base` (existing).
- Produces: `LineSettings.smear_outgoing_color/smear_incoming_color: [f32;4]`, `smear_chroma_gain: f32`; `LinePostParams.smear_outgoing_tint/smear_incoming_tint: [f32;4]`; `bake_smear_tints(post: &mut LinePostParams, settings: &LineSettings)`.

**Atomicity note:** `POST_PARAMS_SIZE = size_of::<LinePostParams>()` sizes the uniform buffer, so the `LinePostParams` struct and `gravity.wgsl`'s `PostParams` MUST gain the two `vec4` fields together in this one task, or the binding size mismatches and the app panics.

- [ ] **Step 1: Write the failing test for `bake_smear_tints`**

In `sim_params.rs` `#[cfg(test)] mod tests`, add:

```rust
#[test]
fn bake_smear_tints_scales_color_by_gain() {
    let mut post = LinePostParams::default();
    let settings = LineSettings {
        smear_chroma_gain: 2.0,
        smear_outgoing_color: [0.5, 0.25, 1.0, 1.0],
        smear_incoming_color: [1.0, 0.25, 0.5, 1.0],
        ..LineSettings::default()
    };
    bake_smear_tints(&mut post, &settings);
    assert_eq!(post.smear_outgoing_tint, [1.0, 0.5, 2.0, 0.0]);
    assert_eq!(post.smear_incoming_tint, [2.0, 0.5, 1.0, 0.0]);
}

#[test]
fn bake_smear_tints_default_reproduces_legacy_endtints() {
    // Legacy gravity.wgsl compounded outgoing (0.96,1,1.042) and incoming
    // (1.042,1,0.96) over 11 steps -> end-tints ~ (0.638,1,1.567) / (1.567,1,0.638).
    let mut post = LinePostParams::default();
    bake_smear_tints(&mut post, &LineSettings::default());
    let approx = |a: f32, b: f32| (a - b).abs() < 1e-2;
    assert!(approx(post.smear_outgoing_tint[0], 0.638) && approx(post.smear_outgoing_tint[2], 1.567),
        "outgoing end-tint should reproduce the legacy blue-shifted trail");
    assert!(approx(post.smear_incoming_tint[0], 1.567) && approx(post.smear_incoming_tint[2], 0.638),
        "incoming end-tint should reproduce the legacy orange-shifted trail");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p wc-sketches --lib line::systems::sim_params 2>&1 | tail -20`
Expected: compile errors (`no field smear_chroma_gain`, `no function bake_smear_tints`, `no field smear_outgoing_tint`).

- [ ] **Step 3: Add the smear settings fields**

In `settings.rs`, after the `palette_scale` field, add (the defaults reproduce the legacy fringe: `(0.4074,0.6383,1.0)·1.5667 ≈ (0.638,1,1.567)`, mirror for incoming):

```rust
    /// Outgoing-trail smear fringe color (normalized hue/ratio). Scaled by
    /// [`Self::smear_chroma_gain`] into the HDR end-tint the gravity-smear
    /// ray-march compounds toward. Default reproduces the legacy cool-blue trail.
    #[setting(
        default = [0.4074_f32, 0.6383, 1.0, 1.0],
        ty = Color,
        label = "Smear outgoing color",
        section = "Smear",
        category = User
    )]
    #[serde(default = "default_smear_outgoing_color")]
    pub smear_outgoing_color: [f32; 4],

    /// Incoming-trail smear fringe color (normalized hue/ratio). Default
    /// reproduces the legacy warm-orange trail.
    #[setting(
        default = [1.0_f32, 0.6383, 0.4074, 1.0],
        ty = Color,
        label = "Smear incoming color",
        section = "Smear",
        category = User
    )]
    #[serde(default = "default_smear_incoming_color")]
    pub smear_incoming_color: [f32; 4],

    /// Smear chromatic gain: scales the fringe colors into HDR (>1) so the
    /// dominant channel boosts past 1 — the additive glow that makes the trails
    /// luminous. `1.5667` reproduces the legacy fringe intensity. With both
    /// colors white, gain `1.0` is a neutral (uncolored) smear.
    #[setting(
        default = 1.5667_f32,
        min = 0.0_f32,
        max = 3.0_f32,
        step = 0.05_f32,
        label = "Smear chroma gain",
        section = "Smear",
        category = User
    )]
    #[serde(default = "default_smear_chroma_gain")]
    pub smear_chroma_gain: f32,
```

Add the serde default fns (after `default_palette_scale`):

```rust
fn default_smear_outgoing_color() -> [f32; 4] {
    [0.4074, 0.6383, 1.0, 1.0]
}

fn default_smear_incoming_color() -> [f32; 4] {
    [1.0, 0.6383, 0.4074, 1.0]
}

fn default_smear_chroma_gain() -> f32 {
    1.5667
}
```

Add three module-doc `//!` bullets describing them.

- [ ] **Step 4: Add the `LinePostParams` tint fields**

In `post_process.rs`, after the `gamma` field of `LinePostParams`, add:

```rust
    /// Outgoing-trail smear HDR end-tint (`xyz`; `w` pad). The gravity smear
    /// derives its per-step chromatic factor as `pow(this, 1/NUM_STEPS)`, so the
    /// trail compounds toward this tint. Written each in-Line frame by
    /// [`crate::line::systems::sim_params::bake_smear_tints`] from `LineSettings`
    /// (`color × gain`). Default zero is inert: the smear is gated by
    /// `g_constant` (also 0 by default), so an unwritten tint never renders.
    pub smear_outgoing_tint: [f32; 4],
    /// Incoming-trail smear HDR end-tint (`xyz`; `w` pad). See
    /// [`Self::smear_outgoing_tint`].
    pub smear_incoming_tint: [f32; 4],
```

(These sit at byte offsets 32 and 48 in the `#[repr(C)]` struct — both 16-byte aligned, total size 64, a multiple of 16 — matching the WGSL `vec4` layout. `POST_PARAMS_SIZE` updates automatically via `size_of`.)

- [ ] **Step 5: Add `bake_smear_tints` and call it in the live writer**

In `sim_params.rs`, add the helper (public, near `bake_post_base`):

```rust
/// Write the configured smear fringe end-tints into [`LinePostParams`] from
/// `LineSettings`: `end = color.rgb × gain` (HDR — the dominant channel boosts
/// past 1 for the additive glow). Shared by the live (`update_sim_params`) and
/// screensaver writers so the two cannot drift. `w` is padding (0).
pub fn bake_smear_tints(post: &mut LinePostParams, settings: &LineSettings) {
    let gain = settings.smear_chroma_gain.max(0.0);
    let o = settings.smear_outgoing_color;
    let i = settings.smear_incoming_color;
    post.smear_outgoing_tint = [o[0] * gain, o[1] * gain, o[2] * gain, 0.0];
    post.smear_incoming_tint = [i[0] * gain, i[1] * gain, i[2] * gain, 0.0];
}
```

In `update_sim_params`, immediately after the `bake_post_base(&mut post, ...)` call, add:

```rust
    bake_smear_tints(&mut post, &settings);
```

- [ ] **Step 6: Call `bake_smear_tints` in the screensaver writer**

In `screensaver/mod.rs::drive_line_attract`, immediately after its `bake_post_base(...)` call, add the same line (the system already has `settings: Res<'_, LineSettings>` and `mut post`):

```rust
    bake_smear_tints(&mut post, &settings);
```

Add `bake_smear_tints` to the existing `use crate::line::systems::sim_params::{...}` import in that file.

- [ ] **Step 7: Run the Rust tests + build**

Run: `cargo test -p wc-sketches --lib line::systems::sim_params 2>&1 | tail -20`
Expected: PASS (both `bake_smear_tints` tests).
Run: `cargo build -p waveconductor 2>&1 | tail -3`
Expected: builds (the WGSL struct is updated next; build here only checks Rust — proceed to Step 8 before running the app).

- [ ] **Step 8: Update `gravity.wgsl` to consume the tints**

In `gravity.wgsl`, add the two fields to `struct PostParams` (after `gamma`):

```wgsl
    smear_outgoing_tint: vec4<f32>,
    smear_incoming_tint: vec4<f32>,
```

Replace the hardcoded factor constants in `smear()`:

```wgsl
    // Per-step chromatic factor derived from the configured HDR end-tint:
    // accumulated over NUM_STEPS it compounds to the end-tint. max(_,0) guards
    // pow of a negative. Defaults reproduce the legacy (0.96,1,1.042) trail.
    let inv_steps = 1.0 / f32(NUM_STEPS);
    let outgoing_factor = pow(max(params.smear_outgoing_tint.rgb, vec3<f32>(0.0)), vec3<f32>(inv_steps));
    let incoming_factor = pow(max(params.smear_incoming_tint.rgb, vec3<f32>(0.0)), vec3<f32>(inv_steps));
```

(The two `let ..._factor = vec4<f32>(...)` lines are removed. The factors are now `vec3`; the accumulation uses them as `vec4`? No — they were `vec4` before with `.a = 1.0`. Change `v_incoming_accum`/`v_outgoing_accum` to `vec3<f32>` and apply to `color.rgb`: see next.)

Update the accumulation to operate in `vec3` (alpha is unaffected by chroma). Replace the accumulator declarations and the two `color = color + textureSample(...) * intensity * v_*_accum;` lines so the tint multiplies only RGB:

```wgsl
    var v_incoming_accum = incoming_factor;
    var v_outgoing_accum = outgoing_factor;
```

```wgsl
        let in_sample = textureSample(scene_texture, scene_sampler, in_uv) * intensity;
        color = color + vec4<f32>(in_sample.rgb * v_incoming_accum, in_sample.a);
        let out_sample = textureSample(scene_texture, scene_sampler, out_uv) * intensity;
        color = color + vec4<f32>(out_sample.rgb * v_outgoing_accum, out_sample.a);
```

```wgsl
        v_incoming_accum = v_incoming_accum * incoming_factor;
        v_outgoing_accum = v_outgoing_accum * outgoing_factor;
```

(This preserves the legacy behavior at the default tints: previously the `vec4` factors had `.a = 1.0`, so alpha accumulated unscaled — exactly what `in_sample.a`/`out_sample.a` now do. RGB compounding is unchanged.)

- [ ] **Step 9: Build + default-smear visual check**

Run: `cargo run -p waveconductor 2>&1 | tail -5`
Expected: builds, no WGSL validation panic.
Run: `cargo xtask capture line-synthetic --json >/dev/null 2>&1` then Read `target/capture/line-synthetic/frame_0240.png`.
Expected: the smear looks like today (cool-blue/orange chromatic trails) — the `bake_smear_tints_default_reproduces_legacy_endtints` test already proved the math; this confirms no gross breakage. If the smear is black/wrong, the accumulation edit is wrong — fix before committing.

- [ ] **Step 10: Commit**

```bash
git add crates/wc-sketches/src/line/settings.rs crates/wc-sketches/src/line/post_process.rs crates/wc-sketches/src/line/systems/sim_params.rs crates/wc-sketches/src/line/screensaver/mod.rs assets/shaders/line/gravity.wgsl
git commit -F - <<'EOF'
feat(line): configurable smear fringe colors

The smear's two chromatic-aberration fringe end-tints become color + shared
HDR gain settings, written into LinePostParams (color x gain) and consumed by
gravity.wgsl as pow(end, 1/NUM_STEPS) per-step factors. The >1 additive glow is
preserved; default color/gain reproduces the legacy cool-blue/orange look.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
EOF
```

---

## Task 4: Full gate run + live tuning checkpoint

**Files:** none (verification + operator tuning).

- [ ] **Step 1: Run every CI gate**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
cargo doc --no-deps --workspace --document-private-items
cargo deny check
cargo xtask check-secrets
```

Expected: all pass (the ~29 pre-existing doc-link warnings are non-fatal; confirm none reference the new items). Fix anything that fails before continuing.

- [ ] **Step 2: Live tuning checkpoint (operator: Madison)**

Run `cargo rund`, enter Line, open the settings panel.
- **Palette** section: try **Velocity** (slow = bright blue, fast = red; confirm particles stay bright, not dark) and **Spectrum** (hot center of the field, cooling to the spawn-list ends). Sweep **strength** and the Dev **spread**.
- **Smear** section: confirm the default looks like today; try shifting `smear_outgoing_color` / `smear_incoming_color` and `smear_chroma_gain` (toward white + gain 1.0 = neutral smear).

Judge defaults. This is the subjective checkpoint — see [[project_audio_tuning_pending]] for the analogous practice.

- [ ] **Step 3: (If tuning changes a default) update and re-verify**

Update BOTH the `#[setting(default = ...)]` and the matching `default_*()` fn (they must agree), re-run `cargo test -p wc-sketches --lib line::` and the forward-compat test, and commit:

```bash
git add crates/wc-sketches/src/line/settings.rs
git commit -F - <<'EOF'
tune(line): color v2 defaults after live checkpoint

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
EOF
```

- [ ] **Step 4: Final confirmation**

Confirm the feature works end to end, all gates green, palette `Off` + default smear render the established look. Done.

---

## Self-Review (completed by plan author)

**Spec coverage:**
- Turbo, texture-free polynomial → Task 2 Step 3. ✓
- Value-normalize (brightness guarantee) → Task 2 Step 3 (`value_normalize`) + Step 4 (applied) + Step 7 (visual confirm). ✓
- Velocity clamped cool→hot, no strobe → Task 2 Step 4; `globals` removed Step 1. ✓
- Spectrum center-peak tent via creation index + `arrayLength` → Task 2 Steps 2, 4. ✓
- Drop `palette_cycle`, rename `Scatter`→`Spectrum`, repack `palette_params` → Task 1. ✓
- Smear two fringe colors + shared HDR gain, `>1` preserved, default reproduces legacy → Task 3 (settings Step 3, `bake_smear_tints` Step 5, shader Step 8, tests Step 1). ✓
- `LinePostParams`/`gravity.wgsl` atomic change → Task 3 (atomicity note + Steps 4, 8). ✓
- Shared `bake_smear_tints` for both writers → Task 3 Steps 5, 6. ✓
- Forward-compat (dropped/renamed keys, new smear keys) → Task 1 Step 1, Task 3 (serde defaults Step 3). ✓
- Off-path bit-exact + no-regression → Task 2 Step 6; default-smear preservation → Task 3 Steps 1, 9. ✓
- Performance, zero-when-idle → unchanged (no new systems; `bake_smear_tints` folds into existing gated writers). ✓

**Placeholder scan:** no TBD/TODO; every code step shows complete code; commands have expected output. ✓

**Type consistency:** `palette_params(mode, strength, scale) -> Vec4` defined Task 1 Step 5, tests updated Task 1 Step 1; `PaletteMode::Spectrum`/`index()==2.0` consistent across Tasks 1–2; `index_norm @location(4)` defined and read within Task 2; `bake_smear_tints(&mut LinePostParams, &LineSettings)` defined Task 3 Step 5, called Steps 5–6, tested Step 1; `smear_*_color: [f32;4]` / `smear_chroma_gain: f32` / `smear_*_tint: [f32;4]` names consistent across settings, helper, and shader. ✓
