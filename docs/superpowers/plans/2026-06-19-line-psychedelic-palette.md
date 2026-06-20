# Line Psychedelic Color Palette Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a configurable psychedelic color palette to the Line sketch — particle hue keyed to a per-particle property (velocity or spawn-hash scatter), crossfading from the per-image color tint toward an Inigo Quilez cosine rainbow that gently cycles over time.

**Architecture:** A `PaletteMode` enum + three tuning floats on `LineSettings` (autosaved on the existing rails) drive a single change-gated `Vec4` material uniform (`palette_params`). The render shader branches on the uniform (constant across the draw, no divergence) and, when enabled, crossfades the existing image-tinted `base` toward a cosine-palette color whose phase reads `globals.time` directly — so animation costs no per-frame uniform write, and the default-off path is bit-exact and free.

**Tech Stack:** Rust, Bevy 0.18 (`Material2d`, `bevy_reflect`, `AsBindGroup`), WGSL (`bevy_sprite_render::mesh2d_view_bindings`), `serde`/`toml` for persistence.

## Global Constraints

Copied from `AGENTS.md`; every task implicitly includes these.

- **No new dependencies.** Everything needed is already in the graph (`bevy`, `serde`, `toml`, `bytemuck`). Run `cargo tree -i <crate>` before ever adding one.
- **Dev build / run:** `cargo rund` (fast dynamic-linked debug). Never launch the bare `target/` binary. Plain fallback: `cargo run -p waveconductor`.
- **Verification gates (run before claiming done):**
  - `cargo fmt --all -- --check`
  - `cargo clippy --all-targets --all-features --workspace -- -D warnings` (warnings are hard errors)
  - `cargo nextest run --workspace --all-features` (does NOT run doctests)
  - `cargo test --doc --workspace` (doctests)
  - `cargo doc --no-deps --workspace --document-private-items` (~29 pre-existing link warnings are non-fatal)
  - `cargo deny check`
  - `cargo xtask check-secrets` (blocks home-dir paths, emails, secret prefixes)
- **Rustdoc** `///` on every public item; `//!` on module roots; inline `//` for shader uniform contracts and DSP/math. Never strip comments during refactors.
- **No `unwrap()`/`expect()`** in non-test code unless a documented invariant. **No `as` casts** on numerics where `From`/`TryFrom` works.
- **One concept per file; files under ~300 lines.** Public API top, private helpers bottom, tests in a `#[cfg(test)] mod tests` footer.
- **Never allocate in a hot path** (per-frame systems, audio callback, worker loops). The render shader allocates nothing; the driver writes a stack `Vec4`.
- **Shaders live in `assets/shaders/<sketch>/<name>.wgsl`** — never inline WGSL in Rust.
- **No hardcoded local paths / PII** in source, comments, configs.

---

## File Structure

**Modify:**
- `crates/wc-sketches/src/line/settings.rs` — add `PaletteMode` enum + four `LineSettings` palette fields + serde defaults + tests.
- `crates/wc-sketches/src/line/material.rs` — add `palette_params: Vec4` `@uniform(6)` + `palette_off()` helper + doc + test.
- `crates/wc-sketches/src/line/systems/spawn.rs` — seed `palette_params` with `palette_off()`.
- `crates/wc-sketches/src/line/systems/mod.rs` — `pub mod palette;`.
- `crates/wc-sketches/src/line/mod.rs` — register `drive_palette` under both activity gates.
- `assets/shaders/line/render.wgsl` — import `globals`, pipe `spawn_hash` to the fragment stage, add the cosine palette + uniform-mode branch + `@binding(6)`.

**Create:**
- `crates/wc-sketches/src/line/systems/palette.rs` — pure `palette_params()` helper + `drive_palette` system + tests.

---

## Task 1: `PaletteMode` enum + `LineSettings` palette fields

**Files:**
- Modify: `crates/wc-sketches/src/line/settings.rs`
- Test: same file, `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `pub enum PaletteMode { Off, Velocity, Scatter }` with `pub fn index(self) -> f32` (`Off→0.0, Velocity→1.0, Scatter→2.0`); new `LineSettings` fields `palette_mode: PaletteMode`, `palette_strength: f32`, `palette_cycle: f32`, `palette_scale: f32`.

- [ ] **Step 1: Write the failing tests**

In the `#[cfg(test)] mod tests` block of `settings.rs`, add:

```rust
#[test]
fn palette_mode_default_is_off() {
    assert_eq!(PaletteMode::default(), PaletteMode::Off);
}

#[test]
fn palette_mode_index_encodes_uniform_channel() {
    assert!((PaletteMode::Off.index() - 0.0).abs() < f32::EPSILON);
    assert!((PaletteMode::Velocity.index() - 1.0).abs() < f32::EPSILON);
    assert!((PaletteMode::Scatter.index() - 2.0).abs() < f32::EPSILON);
}

#[test]
fn palette_mode_setting_is_enum_combobox() {
    use wc_core::settings::{SettingKind, SettingsCategory, SketchSettings};
    let defs = LineSettings::settings_def();
    let def = defs
        .iter()
        .find(|d| d.field_name == "palette_mode")
        .expect("palette_mode setting def must exist");
    assert_eq!(def.category, SettingsCategory::User);
    match &def.kind {
        SettingKind::Enum { variants } => {
            assert_eq!(*variants, &["Off", "Velocity", "Scatter"]);
        }
        other => panic!("expected Enum kind, got {other:?}"),
    }
}
```

Then extend the existing `missing_field_preserves_sibling_values` test — after the final existing assert, add:

```rust
        assert_eq!(parsed.palette_mode, PaletteMode::Off, "palette_mode not default");
        assert!(
            (parsed.palette_strength - 0.8).abs() < 1e-6,
            "palette_strength not default"
        );
        assert!(
            (parsed.palette_cycle - 0.03).abs() < 1e-6,
            "palette_cycle not default"
        );
        assert!(
            (parsed.palette_scale - 1.0).abs() < 1e-6,
            "palette_scale not default"
        );
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p wc-sketches --lib line::settings 2>&1 | tail -20`
Expected: compile error (`cannot find type PaletteMode` / no field `palette_mode`).

- [ ] **Step 3: Add the `PaletteMode` enum**

Above the `LineSettings` struct (after the `use` lines, near the top of `settings.rs`), add:

```rust
/// Which per-particle property drives the psychedelic color palette.
///
/// Unit variants only: the settings `ComboBox` writes a selection back through
/// reflection as a payload-less `DynamicEnum`, which cannot construct a payload
/// variant (see [`wc_core::settings::def::enum_variant_names`]). Mirrors the
/// existing `HandProviderChoice` enum-setting pattern; no separate
/// `register_type` is needed (`register_sketch_settings` registers the owning
/// struct, exactly as for `HandProviderChoice`).
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PaletteMode {
    /// Palette off — particle color is exactly the pre-palette path (image
    /// color-influence tint over the star sprite). The render shader's
    /// uniform-mode branch is not taken, so this is a provable no-op.
    #[default]
    Off,
    /// Hue keyed to `|velocity|`: calm particles sit at one end of the palette,
    /// stirred-up particles sweep through it, so color traces motion/energy.
    Velocity,
    /// Hue keyed to the stable per-particle `spawn_hash` (`0..=1`): a static
    /// rainbow-confetti scatter where each particle keeps its own color.
    Scatter,
}

impl PaletteMode {
    /// Encode the mode as the `palette_params.x` uniform channel the render
    /// shader branches on: `Off → 0.0`, `Velocity → 1.0`, `Scatter → 2.0`.
    #[must_use]
    pub fn index(self) -> f32 {
        match self {
            PaletteMode::Off => 0.0,
            PaletteMode::Velocity => 1.0,
            PaletteMode::Scatter => 2.0,
        }
    }
}
```

- [ ] **Step 4: Add the four `LineSettings` fields**

Inside the `LineSettings` struct, immediately after the `gamma` field (the `pub gamma: f32,` line and its attributes), insert:

```rust
    /// Psychedelic color-palette mode: which per-particle property drives the
    /// particle hue. `Off` (default) leaves color exactly as the pre-palette
    /// path (image tint over the star sprite). See [`PaletteMode`].
    #[setting(default = PaletteMode::Off, ty = Enum, section = "Palette", category = User)]
    #[serde(default = "default_palette_mode")]
    pub palette_mode: PaletteMode,

    /// Palette crossfade strength: `0.0` keeps each particle's image-influence
    /// color, `1.0` is the full palette color. Ignored when `palette_mode` is
    /// `Off`. Defaults to `0.8` so enabling a mode immediately shows color.
    #[setting(
        default = 0.8_f32,
        min = 0.0_f32,
        max = 1.0_f32,
        step = 0.05_f32,
        label = "Palette strength",
        section = "Palette",
        category = User
    )]
    #[serde(default = "default_palette_strength")]
    pub palette_strength: f32,

    /// Palette time-cycle rate (cycles per second): the whole palette scrolls
    /// over time so the field slowly shifts hue. `0.0` = static. The shader
    /// reads the phase from `globals.time`, so animating it costs no per-frame
    /// uniform write. Ignored when `palette_mode` is `Off`.
    #[setting(
        default = 0.03_f32,
        min = 0.0_f32,
        max = 0.5_f32,
        step = 0.01_f32,
        label = "Palette cycle speed",
        section = "Palette",
        category = User
    )]
    #[serde(default = "default_palette_cycle")]
    pub palette_cycle: f32,

    /// Palette spread: how far the driving property stretches across the palette.
    /// `Velocity` mode scales the speed→hue mapping (≈`180 / scale` px/s spans one
    /// palette cycle); `Scatter` mode multiplies the per-particle hash so the
    /// rainbow repeats more often across the field. Dev tuning knob. Ignored when
    /// `palette_mode` is `Off`.
    #[setting(
        default = 1.0_f32,
        min = 0.1_f32,
        max = 5.0_f32,
        step = 0.1_f32,
        label = "Palette spread",
        category = Dev
    )]
    #[serde(default = "default_palette_scale")]
    pub palette_scale: f32,
```

- [ ] **Step 5: Add the serde default free functions**

In the "Per-field serde defaults" block (after `fn default_gamma()`), add:

```rust
fn default_palette_mode() -> PaletteMode {
    PaletteMode::Off
}

fn default_palette_strength() -> f32 {
    0.8
}

fn default_palette_cycle() -> f32 {
    0.03
}

fn default_palette_scale() -> f32 {
    1.0
}
```

- [ ] **Step 6: Add module-doc bullets**

In the module-level `//!` doc's field bullet list (after the `**gamma**` bullet), add:

```rust
//! - **`palette_mode`** — psychedelic color-palette driver: `Off` / `Velocity`
//!   / `Scatter`. `Off` is the bit-exact pre-palette path.
//! - **`palette_strength`** — crossfade from the image-influence color (0) to the
//!   full palette color (1). Ignored when the mode is `Off`.
//! - **`palette_cycle`** — palette time-cycle rate (cycles/s); `0` = static.
//! - **`palette_scale`** — how far the driving property spreads across the
//!   palette. Dev knob.
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p wc-sketches --lib line::settings 2>&1 | tail -20`
Expected: PASS (all settings tests, including the four new asserts in `missing_field_preserves_sibling_values` and the three new tests).

- [ ] **Step 8: Commit**

```bash
git add crates/wc-sketches/src/line/settings.rs
git commit -F - <<'EOF'
feat(line): add PaletteMode enum + palette settings fields

PaletteMode { Off, Velocity, Scatter } plus palette_strength/cycle/scale on
LineSettings, in a new "Palette" section on the existing autosave rails.
Mirrors the HandProviderChoice enum-setting pattern; per-field serde defaults
keep legacy TOML forward-compatible.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
EOF
```

---

## Task 2: `LineMaterial::palette_params` uniform + spawn seed

**Files:**
- Modify: `crates/wc-sketches/src/line/material.rs`
- Modify: `crates/wc-sketches/src/line/systems/spawn.rs:239-249` (the `LineMaterial { .. }` literal)
- Test: `material.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces: `LineMaterial::palette_params: Vec4` (`@uniform(6)`), `LineMaterial::palette_off() -> Vec4` (= `Vec4::ZERO`).

- [ ] **Step 1: Write the failing test**

In `material.rs`'s `#[cfg(test)] mod tests`, add:

```rust
#[test]
fn default_palette_params_is_off() {
    // mode channel (x) == 0 means "palette off" — the shader branch is skipped.
    assert_eq!(LineMaterial::palette_off(), Vec4::ZERO);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p wc-sketches --lib line::material 2>&1 | tail -20`
Expected: compile error (`no function palette_off`).

- [ ] **Step 3: Add the uniform field**

In the `LineMaterial` struct, after the `template_color` field, add:

```rust
    /// Psychedelic palette params: `x` = mode index (`PaletteMode::index()`:
    /// `0` Off / `1` Velocity / `2` Scatter), `y` = crossfade strength `0..=1`,
    /// `z` = time-cycle rate (cycles/s, read against `globals.time`), `w` =
    /// palette spread. Driven by [`crate::line::systems::palette::drive_palette`].
    /// [`Vec4::ZERO`] ([`Self::palette_off`]) sets mode `0`, so the render
    /// shader's uniform-mode branch is skipped and color is the pre-palette path
    /// bit-exactly.
    #[uniform(6)]
    pub palette_params: Vec4,
```

- [ ] **Step 4: Add the `palette_off()` helper**

In `impl LineMaterial`, after `template_color_off()`, add:

```rust
    /// The `palette_params` value meaning "palette off" (mode index `0`). Shared
    /// by the spawn site, the palette driver, and tests.
    pub fn palette_off() -> Vec4 {
        Vec4::ZERO
    }
```

- [ ] **Step 5: Update the struct rustdoc binding list**

In the `LineMaterial` struct's doc comment (the "Bind-group layout" paragraph), append to the binding list sentence: change the trailing `... and \`@binding(5)\` is the per-image colour-influence params.` to also mention binding 6 — append ` \`@binding(6)\` is the psychedelic palette params.` Do the same in the module-level `//!` / the `#[derive(... AsBindGroup ...)]` doc if it enumerates bindings.

- [ ] **Step 6: Seed the uniform at spawn**

In `spawn.rs`, in the `materials.add(LineMaterial { .. })` literal (around line 239), after the `template_color: LineMaterial::template_color_off(),` line, add:

```rust
        // Palette off at spawn (mode index 0); the palette driver writes the
        // active LineSettings palette values each frame (change-gated).
        palette_params: LineMaterial::palette_off(),
```

- [ ] **Step 7: Run the test + build to verify**

Run: `cargo test -p wc-sketches --lib line::material 2>&1 | tail -20`
Expected: PASS.
Run: `cargo build -p wc-sketches 2>&1 | tail -5`
Expected: builds (spawn.rs literal now has all fields).

- [ ] **Step 8: Commit**

```bash
git add crates/wc-sketches/src/line/material.rs crates/wc-sketches/src/line/systems/spawn.rs
git commit -F - <<'EOF'
feat(line): add palette_params uniform to LineMaterial

New @uniform(6) Vec4 (mode, strength, cycle, spread) seeded to palette_off()
(Vec4::ZERO) at spawn. Provides the binding the render shader branches on;
unused until the shader reads it.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
EOF
```

---

## Task 3: `palette_params()` helper + `drive_palette` system + wiring

**Files:**
- Create: `crates/wc-sketches/src/line/systems/palette.rs`
- Modify: `crates/wc-sketches/src/line/systems/mod.rs`
- Modify: `crates/wc-sketches/src/line/mod.rs`
- Test: in `palette.rs`

**Interfaces:**
- Consumes: `PaletteMode`, `LineSettings.palette_*` (Task 1); `LineMaterial::palette_off()` (Task 2); the existing `LineRoot`, `sketch_active`, `in_screensaver` wiring.
- Produces: `pub fn palette_params(mode: PaletteMode, strength: f32, cycle: f32, scale: f32) -> Vec4`; `pub fn drive_palette(...)` system.

- [ ] **Step 1: Write the failing test (create the file with the helper test)**

Create `crates/wc-sketches/src/line/systems/palette.rs`:

```rust
//! Live palette-uniform driver.
//!
//! Maps the `LineSettings` palette knobs onto [`LineMaterial`]`::palette_params`
//! (`x` = mode index, `y` = strength, `z` = cycle, `w` = spread). Change-gated:
//! the uniform is written only when the packed value differs from the material's
//! current value, so dragging an unrelated slider or sitting idle costs one
//! float compare per frame with no asset re-upload. The palette's time animation
//! reads `globals.time` in the shader, so cycling does NOT churn this uniform.

use bevy::prelude::*;
use bevy::sprite_render::MeshMaterial2d;

use crate::line::material::LineMaterial;
use crate::line::settings::{LineSettings, PaletteMode};
use crate::line::LineRoot;

/// Pack the palette settings into the `palette_params` uniform value
/// (`x` = mode index, `y` = strength, `z` = cycle, `w` = spread). `Off` returns
/// [`LineMaterial::palette_off`] (`Vec4::ZERO`) regardless of the other knobs, so
/// the shader's uniform-mode branch is skipped and color is the pre-palette path.
/// Strength is clamped to `0..=1`; cycle and spread are clamped non-negative so a
/// stray value can never invert the crossfade or run the phase backward.
#[must_use]
pub fn palette_params(mode: PaletteMode, strength: f32, cycle: f32, scale: f32) -> Vec4 {
    if mode == PaletteMode::Off {
        return LineMaterial::palette_off();
    }
    Vec4::new(
        mode.index(),
        strength.clamp(0.0, 1.0),
        cycle.max(0.0),
        scale.max(0.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_mode_is_zero_regardless_of_knobs() {
        assert_eq!(palette_params(PaletteMode::Off, 0.8, 0.03, 1.0), Vec4::ZERO);
    }

    #[test]
    fn velocity_mode_packs_channels() {
        let p = palette_params(PaletteMode::Velocity, 0.8, 0.03, 1.5);
        assert!((p.x - 1.0).abs() < 1e-6, "mode index 1 for Velocity");
        assert!((p.y - 0.8).abs() < 1e-6);
        assert!((p.z - 0.03).abs() < 1e-6);
        assert!((p.w - 1.5).abs() < 1e-6);
    }

    #[test]
    fn scatter_mode_index_is_two() {
        let p = palette_params(PaletteMode::Scatter, 1.0, 0.0, 1.0);
        assert!((p.x - 2.0).abs() < 1e-6, "mode index 2 for Scatter");
    }

    #[test]
    fn out_of_range_inputs_clamp() {
        let p = palette_params(PaletteMode::Velocity, 5.0, -1.0, -2.0);
        assert!((p.y - 1.0).abs() < 1e-6, "strength clamps to 1");
        assert!(p.z.abs() < 1e-6, "cycle clamps to 0");
        assert!(p.w.abs() < 1e-6, "spread clamps to 0");
    }
}
```

- [ ] **Step 2: Run to verify the helper tests pass (file compiles standalone)**

First register the module so it compiles — in `crates/wc-sketches/src/line/systems/mod.rs`, after the `pub mod mouse;` line, add:

```rust
pub mod palette;
```

Run: `cargo test -p wc-sketches --lib line::systems::palette 2>&1 | tail -20`
Expected: PASS (4 helper tests).

- [ ] **Step 3: Add the `drive_palette` system**

Append to `palette.rs` (after the `palette_params` fn, before the `#[cfg(test)]` block):

```rust
/// Drive [`LineMaterial::palette_params`] from the `LineSettings` palette knobs.
///
/// Runs while Line is active and while its screensaver shows (registered under
/// both gates in [`crate::line::LinePlugin`], like the attract-color driver) so
/// the palette applies live and in attract while keeping zero systems when idle.
/// Change-gated: mutating a `LineMaterial` re-prepares its bind group, so the
/// write happens only when the packed value actually moves (a settings edit).
/// `last` advances only on an actual write, so a frame where the material asset
/// isn't loaded yet retries instead of dropping the value.
pub fn drive_palette(
    settings: Res<'_, LineSettings>,
    roots: Query<'_, '_, &MeshMaterial2d<LineMaterial>, With<LineRoot>>,
    mut materials: ResMut<'_, Assets<LineMaterial>>,
    mut last: Local<'_, Option<Vec4>>,
) {
    let target = palette_params(
        settings.palette_mode,
        settings.palette_strength,
        settings.palette_cycle,
        settings.palette_scale,
    );
    if *last == Some(target) {
        return;
    }
    for handle in &roots {
        if let Some(material) = materials.get_mut(&handle.0) {
            material.palette_params = target;
            *last = Some(target);
        }
    }
}
```

- [ ] **Step 4: Register the system under both activity gates**

In `crates/wc-sketches/src/line/mod.rs`, in `LinePlugin::build`, after the colour-influence registration block (the `#[cfg(feature = "templates")] app.add_systems(Update, systems::color_influence::drive_color_influence...)` block, around line 199), add:

```rust
        // Live palette-uniform driver: maps the LineSettings palette knobs into
        // the LineMaterial::palette_params uniform. Registered under both the
        // active and screensaver gates (mirrors drive_attract_color) so the
        // palette applies live AND in attract while running zero systems when
        // idle. Change-gated internally, so it is a single float compare per
        // frame in the settled state.
        app.add_systems(
            Update,
            systems::palette::drive_palette.run_if(sketch_active(AppState::Line)),
        );
        app.add_systems(
            Update,
            systems::palette::drive_palette
                .run_if(wc_core::lifecycle::screensaver::in_screensaver(AppState::Line)),
        );
```

(`sketch_active` and `AppState` are already imported in `mod.rs`; `in_screensaver` is referenced by its full path to avoid touching the import block.)

- [ ] **Step 5: Run tests + build to verify wiring**

Run: `cargo test -p wc-sketches --lib line::systems::palette 2>&1 | tail -20`
Expected: PASS.
Run: `cargo build -p waveconductor 2>&1 | tail -5`
Expected: builds (system wired into the plugin).

- [ ] **Step 6: Commit**

```bash
git add crates/wc-sketches/src/line/systems/palette.rs crates/wc-sketches/src/line/systems/mod.rs crates/wc-sketches/src/line/mod.rs
git commit -F - <<'EOF'
feat(line): drive_palette writes palette settings into the material uniform

Pure palette_params() packer (Off -> ZERO no-op, clamps strength/cycle/spread)
plus a change-gated driver registered under the active and screensaver gates,
mirroring drive_attract_color. Uniform unused by the shader until the next task.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
EOF
```

---

## Task 4: Render shader — cosine palette behind the uniform-mode branch

**Files:**
- Modify: `assets/shaders/line/render.wgsl`

**Interfaces:**
- Consumes: `LineMaterial::palette_params` (`@binding(6)`, Task 2); the `Particle.spawn_hash` field already present in the WGSL `Particle` struct.
- Produces: rendered color. No Rust test (WGSL); verified by build + the no-regression capture (palette defaults `Off`).

- [ ] **Step 1: Import `globals` alongside `view`**

At the top of `render.wgsl`, replace:

```wgsl
#import bevy_sprite::mesh2d_view_bindings::view
```

with:

```wgsl
#import bevy_sprite::mesh2d_view_bindings::{view, globals}
```

(`globals` is bound by the 2D pipeline at `@group(0) @binding(1)`; `globals.time` is elapsed seconds — used for the palette phase.)

- [ ] **Step 2: Declare the palette uniform binding**

After the `@group(2) @binding(5) var<uniform> template_color: vec4<f32>;` line and its doc comment, add:

```wgsl
// Psychedelic palette params (LineMaterial::palette_params). x = mode index
// (0 off / 1 velocity / 2 scatter); y = crossfade strength 0..1; z = time-cycle
// rate (cycles/s, multiplied by globals.time for the phase); w = palette spread.
// x = 0 (Vec4(0), the Active/no-palette value) skips the palette branch below,
// so color is the pre-palette path bit-exactly.
@group(2) @binding(6) var<uniform> palette_params: vec4<f32>;
```

- [ ] **Step 3: Pipe `spawn_hash` to the fragment stage**

In `struct VertexOutput`, after the `spawn_color` member, add:

```wgsl
    // Per-particle spawn hash (0..1), carried for the Scatter palette mode.
    // Flat because it is a per-particle constant (no meaningful interpolation).
    @location(4) @interpolate(flat) scatter: f32,
```

In the `vertex` fn, after `out.spawn_color = p.spawn_color;`, add:

```wgsl
    out.scatter = p.spawn_hash;
```

- [ ] **Step 4: Add the cosine `palette` helper**

Above the `@fragment` function, add:

```wgsl
// Inigo Quilez cosine palette — smooth, tunable, the de-facto psychedelic ramp.
// color(t) = a + b * cos(2*pi*(c*t + d)). Each term: a = per-channel bias,
// b = amplitude, c = frequency, d = per-channel phase. These coefficients are
// the canonical rainbow (full hue sweep over t in [0,1]).
fn palette(t: f32) -> vec3<f32> {
    let a = vec3<f32>(0.5, 0.5, 0.5);
    let b = vec3<f32>(0.5, 0.5, 0.5);
    let c = vec3<f32>(1.0, 1.0, 1.0);
    let d = vec3<f32>(0.0, 0.33, 0.67);
    return a + b * cos(6.28318530718 * (c * t + d));
}
```

- [ ] **Step 5: Insert the crossfade between `base` and the wake tint**

In the `fragment` fn, find:

```wgsl
    let base = mix(texel.rgb, texel.rgb * img_rgb, template_color.x);
    // Attract-only velocity tint applies on top of the image-coloured base.
    let wake = smoothstep(WAKE_SPEED_LO, WAKE_SPEED_HI, in.speed) * attract_color.x;
    let tinted = mix(base, base * WAKE_TINT, wake);
```

Replace the `let base = mix(...)` line (the first line only) with:

```wgsl
    // img_base is the pre-palette path: image color-influence tint over the star.
    let img_base = mix(texel.rgb, texel.rgb * img_rgb, template_color.x);
    // Psychedelic palette (uniform-mode branch). palette_params is a uniform —
    // constant across the whole draw — so every fragment takes the same branch
    // (no warp divergence) and the Off case never runs the cos/fract math.
    var base = img_base;
    if (palette_params.x > 0.5) {
        let strength = palette_params.y;
        let cycle = palette_params.z;
        let scale = palette_params.w;
        var driver_t: f32;
        if (palette_params.x < 1.5) {
            // Velocity: ~180 px/s spans one palette cycle at spread 1 (the wake band).
            driver_t = in.speed * scale / 180.0;
        } else {
            // Scatter: stable per-particle hash, repeated `scale` times across 0..1.
            driver_t = in.scatter * scale;
        }
        // fract wraps the ramp; globals.time * cycle scrolls the whole palette.
        let t = fract(driver_t + globals.time * cycle);
        let pal_base = texel.rgb * palette(t);
        // Crossfade image-coloured -> palette by strength (0 = image, 1 = palette).
        base = mix(img_base, pal_base, strength);
    }
    // Attract-only velocity tint applies on top of the (palette-or-image) base.
    let wake = smoothstep(WAKE_SPEED_LO, WAKE_SPEED_HI, in.speed) * attract_color.x;
    let tinted = mix(base, base * WAKE_TINT, wake);
```

(Leave the subsequent `let rgb = tinted * (1.0 + attract_color.y);` and the `return` untouched.)

- [ ] **Step 6: Build and run the no-regression capture**

Run: `cargo run -p waveconductor 2>&1 | tail -5` (or `cargo rund`) — confirm the app builds and the shader compiles (no WGSL validation panic in the log).
Run: `cargo xtask capture line-synthetic --json 2>&1 | tail -20`
Expected: exit 0, no frame regresses — palette defaults to `Off`, so the uniform-mode branch is skipped and the render is pixel-identical to the baseline. If any frame is flagged, Read the flagged PNG: a difference here is a real regression (the off path is supposed to be bit-exact), NOT a baseline to update — investigate before proceeding.

- [ ] **Step 7: Commit**

```bash
git add assets/shaders/line/render.wgsl
git commit -F - <<'EOF'
feat(line): cosine psychedelic palette in the render shader

Crossfade the image-tinted base toward an Inigo Quilez cosine rainbow keyed to
velocity or spawn-hash, cycling via globals.time. Guarded by a uniform-mode
branch so the default-off path runs no palette math and renders bit-exactly as
before (verified: line-synthetic capture unchanged).

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
EOF
```

---

## Task 5: Full gate run + live visual checkpoint + default tuning

**Files:** none (verification + optional default tweak in `settings.rs` if tuning changes a default).

**Interfaces:** consumes the whole feature.

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

Expected: all pass (clippy clean — no `unwrap`/`expect`/`as` introduced; the ~29 pre-existing doc-link warnings are non-fatal). Fix anything that fails before continuing.

- [ ] **Step 2: Live visual checkpoint (operator: Madison)**

Run: `cargo rund`
In the settings panel, open the **Palette** section and:
- Switch `palette_mode` to **Velocity**: stir the field with the pointer — fast particles should sweep through the rainbow while the calm field holds one end. Confirm the palette slowly cycles (cycle = 0.03).
- Switch to **Scatter**: the field should read as stable rainbow confetti (each particle a fixed hue), slowly drifting with the cycle.
- Sweep `palette_strength` 0→1: at 0 the color matches the pre-palette look (or the image tint if a template is loaded); at 1 it is fully palette.
- (With a template loaded) confirm the crossfade dials from image color toward palette.
- Set `palette_mode` back to **Off**: confirm the look returns exactly to today's warm-white field.

Judge the defaults (`strength = 0.8`, `cycle = 0.03`). This is the tuning checkpoint — see [[project_audio_tuning_pending]] for the analogous ear-tuning practice.

- [ ] **Step 3: (If tuning changed a default) update `settings.rs` and re-verify**

If the checkpoint settles on different defaults, update BOTH the `#[setting(default = ...)]` attribute and the matching `default_palette_*()` free fn (they must agree), then re-run the four asserts in `missing_field_preserves_sibling_values` (`cargo test -p wc-sketches --lib line::settings`). Commit:

```bash
git add crates/wc-sketches/src/line/settings.rs
git commit -F - <<'EOF'
tune(line): palette default <field> = <value> after live checkpoint

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
EOF
```

- [ ] **Step 4: Final confirmation**

Confirm the whole feature works end to end and all gates are green. The feature is complete: palette mode persists across restarts (autosaved), defaults to Off (zero regression), and adds no cost when off.

---

## Self-Review (completed by plan author)

**Spec coverage:**
- Palette modes Off/Velocity/Scatter → Task 1 (enum) + Task 4 (shader driver_t). ✓ (Position/Age correctly excluded per spec.)
- "Palette" settings section, User/Dev categories, autosave, forward-compat defaults → Task 1. ✓
- `palette_params` change-gated uniform + driver under both gates → Tasks 2, 3. ✓
- Crossfade image↔palette compositing → Task 4 Step 5. ✓
- Cosine rainbow palette → Task 4 Step 4. ✓
- `globals.time` animation, no per-frame uniform churn → Task 3 (cycle not in uniform path) + Task 4 Steps 1, 5. ✓
- Uniform-mode branch, off-path free + bit-exact → Task 4 Step 5 + Task 4 Step 6 (no-regression capture). ✓
- `spawn_hash` piped to fragment, no GPU struct change → Task 4 Step 3. ✓
- Performance (no hot-path alloc, zero-when-idle driver) → Task 3 (stack Vec4, both gates). ✓
- Testing: helper unit tests, enum reflect/serde, forward-compat, no-regression visual → Tasks 1, 3, 4. ✓

**Placeholder scan:** no TBD/TODO; every code step shows complete code; commands have expected output. ✓

**Type consistency:** `palette_params(mode, strength, cycle, scale) -> Vec4` defined in Task 3 and called identically in `drive_palette`; `PaletteMode::index()` defined Task 1, used Task 3; `palette_off() -> Vec4` defined Task 2, used Tasks 2, 3; uniform `@binding(6)` matches between material (Task 2) and shader (Task 4); `VertexOutput.scatter @location(4)` defined and read within Task 4. ✓
