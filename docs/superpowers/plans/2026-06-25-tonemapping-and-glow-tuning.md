# Per-Sketch Tonemapping + Glow Tuning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the camera's tonemapping per-sketch (default ReinhardLuminance, Home stays SDR), expose per-sketch bloom knobs, and remove the now-obsolete cymatics screensaver colour compensation.

**Architecture:** A shared `TonemapChoice` enum in `wc-core` maps to Bevy's `Tonemapping`. The main `Camera2d` spawns with `Tonemapping::None` (SDR base for Home). Each sketch carries `tonemapping` / `bloom_intensity` / `bloom_threshold` settings; a per-sketch `Update` system (gated on its `AppState`) writes them onto the camera's `Tonemapping` + `Bloom` components each frame (live tuning), and a shared `OnExit` system resets the camera to the SDR base so Home/picker render un-tonemapped.

**Tech Stack:** Rust, Bevy 0.19 (`Camera2d`, `Tonemapping`, `Bloom`), the in-house `SketchSettings` derive (`wc-core-macros`), egui dev panel.

## Global Constraints

- CI gates (run all before claiming done): `cargo fmt --all -- --check`; `cargo clippy --all-targets --all-features --workspace -- -D warnings`; `cargo nextest run --workspace --all-features`; `cargo test --doc --workspace`; `cargo doc --no-deps --workspace --document-private-items`; `cargo xtask check-secrets`.
- `///` rustdoc on every public item; `//!` module docs on new modules; inline `//` for uniform/shader contracts.
- No `unwrap()`/`expect()` in non-test code unless a documented invariant.
- No `as` casts on numeric types where `From`/`TryFrom` works.
- No new dependencies — reuse what's in the graph.
- Per-field serde defaults MUST match the `#[setting(default = ...)]` value (update both sites together).
- Commit after each task. Do NOT push; do NOT commit unless on a feature branch (currently `v5-alpha`; the operator commits — leave commits staged/described unless told otherwise).
- Rendering changes are verified with `cargo xtask capture <scenario>` (operator reviews PNGs), not unit tests. Capture returns black frames if the app window is not foregrounded — note that and have the operator run it.

---

## File Structure

- `crates/wc-core/src/render/mod.rs` — **new.** `TonemapChoice` enum + `to_bevy()`, base-bloom constants, and `set_camera_render_profile` / `reset_camera_render_profile` helpers. One responsibility: the camera render-profile vocabulary shared by every sketch.
- `crates/wc-core/src/lib.rs` — **modify.** Register the new `render` module.
- `crates/waveconductor/src/main.rs` — **modify.** Camera spawns `Tonemapping::None` + base bloom constants; remove `WC_DEBUG_REF_SWATCHES` scaffolding.
- `crates/wc-sketches/src/cymatics/settings.rs` — **modify.** Add `tonemapping`/`bloom_intensity`/`bloom_threshold`; remove `attract_brightness`/`attract_saturation`.
- `crates/wc-sketches/src/cymatics/mod.rs` — **modify.** Add apply-profile system; simplify `update_cymatics_material` (plain `master_brightness`, drop saturation lane).
- `assets/shaders/cymatics/render.wgsl` — **modify.** Drop the `skew.w` saturation branch.
- `crates/wc-sketches/src/line/settings.rs`, `crates/wc-sketches/src/line/mod.rs` — **modify.** Add the three render settings + apply system.
- `crates/wc-sketches/src/dots/settings.rs`, `crates/wc-sketches/src/dots/mod.rs` — **modify.** Same.

---

## Task 1: Shared `TonemapChoice` + camera render-profile helpers

**Files:**
- Create: `crates/wc-core/src/render/mod.rs`
- Modify: `crates/wc-core/src/lib.rs` (add `pub mod render;`)
- Test: inline `#[cfg(test)] mod tests` in `render/mod.rs`

**Interfaces:**
- Produces: `wc_core::render::TonemapChoice` (enum, `Default` = `ReinhardLuminance`); `TonemapChoice::to_bevy(self) -> bevy::core_pipeline::tonemapping::Tonemapping`; consts `BASE_BLOOM_INTENSITY: f32 = 0.15`, `BASE_BLOOM_THRESHOLD: f32 = 0.0`; `set_camera_render_profile(&mut Tonemapping, &mut Bloom, TonemapChoice, intensity: f32, threshold: f32)`; `reset_camera_render_profile(&mut Tonemapping, &mut Bloom)`.

- [ ] **Step 1: Write the failing test**

In a new file `crates/wc-core/src/render/mod.rs`, put the tests at the footer (module body filled in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bevy::core_pipeline::tonemapping::Tonemapping;

    #[test]
    fn default_is_reinhard_luminance() {
        assert_eq!(TonemapChoice::default(), TonemapChoice::ReinhardLuminance);
    }

    #[test]
    fn every_variant_maps_to_bevy() {
        assert_eq!(TonemapChoice::ReinhardLuminance.to_bevy(), Tonemapping::ReinhardLuminance);
        assert_eq!(TonemapChoice::TonyMcMapface.to_bevy(), Tonemapping::TonyMcMapface);
        assert_eq!(TonemapChoice::AgX.to_bevy(), Tonemapping::AgX);
        assert_eq!(TonemapChoice::AcesFitted.to_bevy(), Tonemapping::AcesFitted);
        assert_eq!(TonemapChoice::None.to_bevy(), Tonemapping::None);
    }

    #[test]
    fn reset_restores_sdr_base() {
        let mut tm = Tonemapping::AgX;
        let mut bloom = Bloom { intensity: 0.9, ..Bloom::NATURAL };
        reset_camera_render_profile(&mut tm, &mut bloom);
        assert_eq!(tm, Tonemapping::None);
        assert!((bloom.intensity - BASE_BLOOM_INTENSITY).abs() < f32::EPSILON);
        assert!((bloom.prefilter.threshold - BASE_BLOOM_THRESHOLD).abs() < f32::EPSILON);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p wc-core render::tests 2>&1 | tail`
Expected: FAIL to compile — `TonemapChoice` / `reset_camera_render_profile` undefined.

- [ ] **Step 3: Write the module**

Prepend to `crates/wc-core/src/render/mod.rs` (above the test module). Mirror the `PaletteMode` enum derives from `crates/wc-sketches/src/line/settings.rs:126`:

```rust
//! Shared camera render-profile vocabulary: the tonemapping operator a sketch
//! selects, the bloom knobs it tunes, and the helpers that write them onto the
//! main `Camera2d`. Centralised here so sketch crates pick a tonemap by name
//! without depending on `bevy::core_pipeline::tonemapping` directly, and so the
//! SDR base (Home/picker) lives in exactly one place.

use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::post_process::bloom::Bloom;
use bevy::reflect::Reflect;
use serde::{Deserialize, Serialize};

/// Bloom intensity the main camera spawns with and resets to outside any sketch
/// (Home/picker). Sketches override it live via their `bloom_intensity` setting.
pub const BASE_BLOOM_INTENSITY: f32 = 0.15;

/// Bloom prefilter threshold the main camera spawns with and resets to (bloom
/// everything). Sketches override it live via their `bloom_threshold` setting.
pub const BASE_BLOOM_THRESHOLD: f32 = 0.0;

/// The camera tonemapping operator a sketch can select, mirrored from Bevy's
/// [`Tonemapping`] so it can back a `ty = Enum` setting (a `Reflect` enum with
/// unit variants). `Default` is [`Self::ReinhardLuminance`] — the chroma-
/// preserving "neon glow" baseline. Variant names are the serialized TOML
/// strings, so do not `#[serde(rename)]` them.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TonemapChoice {
    /// Luminance-only Reinhard: preserves colour ratios as values brighten.
    #[default]
    ReinhardLuminance,
    /// Hue-preserving filmic display transform; gentler highlight rolloff.
    TonyMcMapface,
    /// Sobotka AgX: desaturates highlights (filmic, muted).
    AgX,
    /// ACES fitted: punchy/contrasty (shifts hue toward orange in highlights).
    AcesFitted,
    /// No tonemap: linear passthrough (HDR clips at the swapchain). The SDR base.
    None,
}

impl TonemapChoice {
    /// Map to the Bevy [`Tonemapping`] component variant.
    #[must_use]
    pub fn to_bevy(self) -> Tonemapping {
        match self {
            Self::ReinhardLuminance => Tonemapping::ReinhardLuminance,
            Self::TonyMcMapface => Tonemapping::TonyMcMapface,
            Self::AgX => Tonemapping::AgX,
            Self::AcesFitted => Tonemapping::AcesFitted,
            Self::None => Tonemapping::None,
        }
    }
}

/// Write a sketch's render profile onto the main camera's tonemapping + bloom.
/// Called each frame by a sketch's apply system so dev-panel edits are live.
pub fn set_camera_render_profile(
    tonemapping: &mut Tonemapping,
    bloom: &mut Bloom,
    choice: TonemapChoice,
    bloom_intensity: f32,
    bloom_threshold: f32,
) {
    let desired = choice.to_bevy();
    if *tonemapping != desired {
        *tonemapping = desired;
    }
    if bloom.intensity != bloom_intensity {
        bloom.intensity = bloom_intensity;
    }
    if bloom.prefilter.threshold != bloom_threshold {
        bloom.prefilter.threshold = bloom_threshold;
    }
}

/// Reset the camera to the SDR base (Home/picker): no tonemap, spawn-default
/// bloom. Called on `OnExit` of every sketch.
pub fn reset_camera_render_profile(tonemapping: &mut Tonemapping, bloom: &mut Bloom) {
    if *tonemapping != Tonemapping::None {
        *tonemapping = Tonemapping::None;
    }
    if bloom.intensity != BASE_BLOOM_INTENSITY {
        bloom.intensity = BASE_BLOOM_INTENSITY;
    }
    if bloom.prefilter.threshold != BASE_BLOOM_THRESHOLD {
        bloom.prefilter.threshold = BASE_BLOOM_THRESHOLD;
    }
}
```

Add to `crates/wc-core/src/lib.rs` near the other `pub mod` lines:

```rust
pub mod render;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p wc-core render::tests 2>&1 | tail`
Expected: PASS (3 tests). If `Tonemapping::AcesFitted` does not resolve, check the Bevy 0.19 variant name with `cargo doc --open -p bevy` and adjust the enum + mapping together.

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/render/mod.rs crates/wc-core/src/lib.rs
git commit -m "feat(render): shared TonemapChoice + camera render-profile helpers"
```

---

## Task 2: Camera spawns SDR base

**Files:**
- Modify: `crates/waveconductor/src/main.rs` (the `spawn_camera` fn, ~line 214-244, and the `apply_debug_bloom_toggle` import context)

**Interfaces:**
- Consumes: `wc_core::render::{BASE_BLOOM_INTENSITY, BASE_BLOOM_THRESHOLD}`.

- [ ] **Step 1: Change the camera tonemapping + bloom defaults**

In `spawn_camera`, replace the `debug_tonemapping(),` line (the spike override) and the inline bloom literals with the SDR base. The bloom literals `0.15` / `0.0` become the shared constants:

```rust
        // SDR base: Home/picker render un-tonemapped (their art is already SDR).
        // Each sketch overrides this on enter via its render-profile apply
        // system (see `wc_core::render`); `WC_DEBUG_TONEMAP` still overrides for
        // auditioning (debug builds only).
        debug_tonemapping(),
        Bloom {
            intensity: wc_core::render::BASE_BLOOM_INTENSITY,
            low_frequency_boost: 0.7,
            prefilter: BloomPrefilter {
                threshold: wc_core::render::BASE_BLOOM_THRESHOLD,
                threshold_softness: 0.0,
            },
            ..Bloom::NATURAL
        },
```

Then change the `debug_tonemapping()` release fallback (the `#[cfg(not(debug_assertions))]` arm) from `Tonemapping::AgX` to `Tonemapping::None`, and the `_ => Tonemapping::AgX` default arm in the debug version to `_ => Tonemapping::None`, so the base is SDR in every build:

```rust
        _ => Tonemapping::None,
```
```rust
#[cfg(not(debug_assertions))]
fn debug_tonemapping() -> Tonemapping {
    Tonemapping::None
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p waveconductor 2>&1 | tail -3`
Expected: `Finished`.

- [ ] **Step 3: Commit**

```bash
git add crates/waveconductor/src/main.rs
git commit -m "feat(camera): spawn with SDR base tonemapping + shared bloom constants"
```

---

## Task 3: Cymatics tonemapping + bloom settings and apply system

**Files:**
- Modify: `crates/wc-sketches/src/cymatics/settings.rs` (add 3 fields + serde defaults + the defaults test)
- Modify: `crates/wc-sketches/src/cymatics/mod.rs` (apply system + plugin wiring)

**Interfaces:**
- Consumes: `wc_core::render::{TonemapChoice, set_camera_render_profile, reset_camera_render_profile}`.
- Produces: `CymaticsSettings::{tonemapping, bloom_intensity, bloom_threshold}`; system `apply_cymatics_render_profile`; shared exit system `reset_render_profile` (define in `wc_core::render` if not present — see note).

- [ ] **Step 1: Add the settings fields**

In `crates/wc-sketches/src/cymatics/settings.rs`, add to the `// ── Visual ──` section of `CymaticsSettings` (after `skew_curve`):

```rust
    /// Camera tonemapping operator for this sketch. Default `ReinhardLuminance`
    /// (chroma-preserving "neon glow"). Applied to the main camera while Cymatics
    /// is active; Home resets to SDR. Live, no restart.
    #[setting(
        default = wc_core::render::TonemapChoice::ReinhardLuminance,
        ty = Enum,
        label = "Tonemapping",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_tonemapping")]
    pub tonemapping: wc_core::render::TonemapChoice,

    /// Bloom intensity for this sketch (main camera). Default `0.35` — stronger
    /// glow than the SDR base 0.15. Live, no restart.
    #[setting(
        default = 0.35_f32,
        min = 0.0_f32,
        max = 1.0_f32,
        step = 0.05_f32,
        label = "Bloom intensity",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_bloom_intensity")]
    pub bloom_intensity: f32,

    /// Bloom prefilter threshold for this sketch. Default `0.7` — only HDR cores
    /// bloom (crisp midtones + glowing highlights). `0.0` blooms everything.
    #[setting(
        default = 0.7_f32,
        min = 0.0_f32,
        max = 3.0_f32,
        step = 0.05_f32,
        label = "Bloom threshold",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_bloom_threshold")]
    pub bloom_threshold: f32,
```

Add the serde defaults near the other `default_*` fns:

```rust
fn default_tonemapping() -> wc_core::render::TonemapChoice {
    wc_core::render::TonemapChoice::ReinhardLuminance
}
fn default_bloom_intensity() -> f32 {
    0.35
}
fn default_bloom_threshold() -> f32 {
    0.7
}
```

Add assertions to the existing `default_values_match_serde_defaults` test:

```rust
        assert_eq!(d.tonemapping, default_tonemapping(), "tonemapping default mismatch");
        assert!((d.bloom_intensity - default_bloom_intensity()).abs() < f32::EPSILON, "bloom_intensity");
        assert!((d.bloom_threshold - default_bloom_threshold()).abs() < f32::EPSILON, "bloom_threshold");
```

- [ ] **Step 2: Run the settings test to verify it passes**

Run: `cargo test -p wc-sketches cymatics::settings 2>&1 | tail`
Expected: PASS.

- [ ] **Step 3: Add the apply + reset systems**

In `crates/wc-sketches/src/cymatics/mod.rs`, add imports at the top:

```rust
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::post_process::bloom::Bloom;
```

Add the systems (place near `update_cymatics_material`):

```rust
/// Write Cymatics' tonemapping + bloom settings onto the main camera each frame
/// while Cymatics is active (live dev-panel tuning). Change-gated inside
/// `set_camera_render_profile`, so an unchanged profile is a no-op.
fn apply_cymatics_render_profile(
    settings: Res<'_, CymaticsSettings>,
    mut camera: Query<'_, '_, (&mut Tonemapping, &mut Bloom), With<Camera2d>> // `Camera2d` via the crate's `bevy::prelude::*` import,
) {
    for (mut tonemapping, mut bloom) in &mut camera {
        wc_core::render::set_camera_render_profile(
            &mut tonemapping,
            &mut bloom,
            settings.tonemapping,
            settings.bloom_intensity,
            settings.bloom_threshold,
        );
    }
}

/// `OnExit(AppState::Cymatics)` — restore the SDR camera base so Home/picker is
/// un-tonemapped.
fn reset_cymatics_render_profile(
    mut camera: Query<'_, '_, (&mut Tonemapping, &mut Bloom), With<Camera2d>> // `Camera2d` via the crate's `bevy::prelude::*` import,
) {
    for (mut tonemapping, mut bloom) in &mut camera {
        wc_core::render::reset_camera_render_profile(&mut tonemapping, &mut bloom);
    }
}
```

Wire them in `CymaticsPlugin::build`: add `apply_cymatics_render_profile` to an `Update` block gated on `sketch_active(AppState::Cymatics).or_else(in_idle(AppState::Cymatics)).or_else(in_screensaver(AppState::Cymatics))` (so the camera stays correct through idle + screensaver), and add `reset_cymatics_render_profile` to the existing `OnExit(AppState::Cymatics)` block:

```rust
        app.add_systems(
            Update,
            apply_cymatics_render_profile.run_if(
                sketch_active(AppState::Cymatics)
                    .or_else(in_idle(AppState::Cymatics))
                    .or_else(in_screensaver(AppState::Cymatics)),
            ),
        );
```

(Add `reset_cymatics_render_profile` into the existing `OnExit(AppState::Cymatics)` tuple at `crates/wc-sketches/src/cymatics/mod.rs:206-213`.)

- [ ] **Step 4: Verify it compiles + capture**

Run: `cargo check -p wc-sketches 2>&1 | tail -3` → `Finished`.
Then operator runs `cargo rund`, enters Cymatics, and confirms the Reinhard look + that returning Home shows SDR picker tiles. (Capture is black when backgrounded — operator must foreground.)

- [ ] **Step 5: Commit**

```bash
git add crates/wc-sketches/src/cymatics/settings.rs crates/wc-sketches/src/cymatics/mod.rs
git commit -m "feat(cymatics): per-sketch tonemapping + bloom applied to the camera"
```

---

## Task 4: Line tonemapping + bloom settings and apply system

**Files:**
- Modify: `crates/wc-sketches/src/line/settings.rs`
- Modify: `crates/wc-sketches/src/line/mod.rs`

**Interfaces:** identical shape to Task 3, for `LineSettings` / `LinePlugin`.

- [ ] **Step 1: Add the three settings fields**

In `crates/wc-sketches/src/line/settings.rs`, add to `LineSettings` (a Visual section; mirror the exact field blocks from Task 3 Step 1 verbatim — same attributes, same `default_tonemapping`/`default_bloom_intensity`/`default_bloom_threshold` serde fns, same defaults `ReinhardLuminance`/`0.35`/`0.7`). Add the same three assertions to Line's `default_values_match_serde_defaults` test (the function name may differ — find the test asserting LineSettings defaults and append).

- [ ] **Step 2: Run the settings test**

Run: `cargo test -p wc-sketches line::settings 2>&1 | tail` → PASS.

- [ ] **Step 3: Add the apply + reset systems**

In `crates/wc-sketches/src/line/mod.rs`, add the same imports (`bevy::core_pipeline::tonemapping::Tonemapping`, `bevy::post_process::bloom::Bloom`) and the two systems, renamed `apply_line_render_profile` / `reset_line_render_profile`, reading `LineSettings`. Register `apply_line_render_profile` on `Update` gated on `in_state(AppState::Line)` (Line has no separate idle/screensaver gating needs for this — use `in_state` so it also covers Line's screensaver sub-state), and `reset_line_render_profile` on the existing `OnExit(AppState::Line)` block (`crates/wc-sketches/src/line/mod.rs:162-165`). Use `in_state(AppState::Line)` from `wc_core::lifecycle::state::AppState`.

```rust
fn apply_line_render_profile(
    settings: Res<'_, crate::line::settings::LineSettings>,
    mut camera: Query<'_, '_, (&mut Tonemapping, &mut Bloom), With<Camera2d>> // `Camera2d` via the crate's `bevy::prelude::*` import,
) {
    for (mut tonemapping, mut bloom) in &mut camera {
        wc_core::render::set_camera_render_profile(
            &mut tonemapping, &mut bloom,
            settings.tonemapping, settings.bloom_intensity, settings.bloom_threshold,
        );
    }
}

fn reset_line_render_profile(
    mut camera: Query<'_, '_, (&mut Tonemapping, &mut Bloom), With<Camera2d>> // `Camera2d` via the crate's `bevy::prelude::*` import,
) {
    for (mut tonemapping, mut bloom) in &mut camera {
        wc_core::render::reset_camera_render_profile(&mut tonemapping, &mut bloom);
    }
}
```

```rust
        app.add_systems(
            Update,
            apply_line_render_profile.run_if(in_state(AppState::Line)),
        );
```

- [ ] **Step 4: Verify + commit**

Run: `cargo check -p wc-sketches 2>&1 | tail -3` → `Finished`. Operator confirms Line glow under Reinhard via `cargo rund`.

```bash
git add crates/wc-sketches/src/line/settings.rs crates/wc-sketches/src/line/mod.rs
git commit -m "feat(line): per-sketch tonemapping + bloom applied to the camera"
```

---

## Task 5: Dots tonemapping + bloom settings and apply system

**Files:**
- Modify: `crates/wc-sketches/src/dots/settings.rs`
- Modify: `crates/wc-sketches/src/dots/mod.rs`

**Interfaces:** identical shape, for `DotsSettings` / `DotsPlugin`.

- [ ] **Step 1-4:** Repeat Task 4 verbatim for Dots: same three settings fields + serde defaults + test assertions in `dots/settings.rs`; `apply_dots_render_profile` / `reset_dots_render_profile` in `dots/mod.rs` reading `DotsSettings`; register apply on `Update.run_if(in_state(AppState::Dots))`; register reset on the existing `OnExit(AppState::Dots)` block (`crates/wc-sketches/src/dots/mod.rs:109-112`).

- [ ] **Step 5: Verify + commit**

```bash
cargo check -p wc-sketches 2>&1 | tail -3   # Finished
git add crates/wc-sketches/src/dots/settings.rs crates/wc-sketches/src/dots/mod.rs
git commit -m "feat(dots): per-sketch tonemapping + bloom applied to the camera"
```

---

## Task 6: Remove cymatics screensaver colour compensation

**Files:**
- Modify: `crates/wc-sketches/src/cymatics/settings.rs` (remove `attract_brightness`, `attract_saturation` fields + serde defaults + their test assertions)
- Modify: `crates/wc-sketches/src/cymatics/mod.rs` (`update_cymatics_material`: brightness becomes plain `master_brightness`; drop saturation packing)
- Modify: `assets/shaders/cymatics/render.wgsl` (remove the `skew.w` saturation branch)

**Interfaces:**
- `update_cymatics_material` still packs `Vec4(skew_intensity, brightness, gamma, w)`; `w` is now a fixed `1.0` (unused — kept so the uniform layout is unchanged).

- [ ] **Step 1: Update `update_cymatics_material`**

In `crates/wc-sketches/src/cymatics/mod.rs`, replace the brightness + saturation lines (currently `let brightness = settings.master_brightness * (1.0 + (settings.attract_brightness - 1.0) * fade.alpha());` and `let saturation = 1.0 + (settings.attract_saturation - 1.0) * fade.alpha();`) with:

```rust
    // Brightness is the plain master setting now — the screensaver brightness
    // lift was a workaround for the AgX-bypass bug (fixed in the blur node) and
    // for AgX's muting (gone now that the operator-chosen tonemap is applied
    // consistently). `skew.w` (formerly screensaver saturation) is pinned to the
    // identity 1.0 and ignored by the shader.
    let brightness = settings.master_brightness;
```

Update the `Vec4::new(...)` pack to `Vec4::new(skew_intensity, brightness, settings.gamma, 1.0)` and remove the now-unused `fade: Res<'_, ScreensaverFade>` system param and the `saturation` local. Remove the `use ... ScreensaverFade;` import if no longer used in the file (check: `grep -n ScreensaverFade crates/wc-sketches/src/cymatics/mod.rs` — if only the removed lines referenced it, drop the import).

- [ ] **Step 2: Remove the shader saturation branch**

In `assets/shaders/cymatics/render.wgsl`, delete the entire `if skew.w != 1.0 { ... }` block (the luma-preserving saturation step) and its doc comment. Update the `@binding(1)` doc comment for `.w` to read: `// .w = unused (formerly screensaver saturation; removed)`.

- [ ] **Step 3: Remove the settings fields**

In `crates/wc-sketches/src/cymatics/settings.rs`, delete the `attract_brightness` and `attract_saturation` `#[setting]` fields, their `default_attract_brightness` / `default_attract_saturation` serde fns, and their assertions in `default_values_match_serde_defaults` and `missing_field_preserves_sibling_values`. Update the module-doc bullet that lists the raindrop fields to drop both names.

- [ ] **Step 4: Verify**

Run: `cargo test -p wc-sketches cymatics 2>&1 | tail` → PASS.
Run: `cargo check -p wc-sketches 2>&1 | tail -3` → `Finished`.
Operator runs `cargo rund` → Cymatics screensaver: confirm no colour shift vs active, no compile-time references to removed fields.

- [ ] **Step 5: Commit**

```bash
git add crates/wc-sketches/src/cymatics/settings.rs crates/wc-sketches/src/cymatics/mod.rs assets/shaders/cymatics/render.wgsl
git commit -m "refactor(cymatics): drop screensaver colour compensation (obsoleted by tonemap fix)"
```

---

## Task 7: Remove the `WC_DEBUG_REF_SWATCHES` scaffolding

**Files:**
- Modify: `crates/waveconductor/src/main.rs` (remove the `spawn_debug_ref_swatches` fn + its `Startup` registration)

- [ ] **Step 1: Delete the scaffolding**

In `crates/waveconductor/src/main.rs`, remove the `#[cfg(debug_assertions)] app.add_systems(Startup, spawn_debug_ref_swatches);` registration block (and its comment), and delete the entire `spawn_debug_ref_swatches` fn. Leave `debug_tonemapping` / `WC_DEBUG_TONEMAP` in place (still useful).

- [ ] **Step 2: Verify**

Run: `cargo check -p waveconductor 2>&1 | tail -3` → `Finished`.
Run: `cargo xtask check-secrets 2>&1 | tail -3` (no home paths/secrets introduced).

- [ ] **Step 3: Commit**

```bash
git add crates/waveconductor/src/main.rs
git commit -m "chore: remove WC_DEBUG_REF_SWATCHES spike scaffolding"
```

---

## Final gate (after all tasks)

- [ ] Run the full CI gate set from Global Constraints. Fix any clippy `-D warnings` (likely `doc_markdown` on `AgX`/`ReinhardLuminance` — wrap in backticks) inline.
- [ ] Operator visual pass: each sketch under Reinhard (glows keep colour), Home SDR, no screensaver pop, dev panel shows the new Tonemapping dropdown + bloom sliders (bool/enum settings render correctly).

---

## Deferred (separate follow-up plan — flag to operator)

**Per-sketch `master_brightness` for Line and Dots.** The spec lists standardising `master_brightness` across all three sketches. Cymatics already routes it through its material `skew` lane; Line and Dots route only `gamma`, through their **post-process param resources + shaders** (`dots`/`line` post_process), so adding a brightness lane means a settings field + a params-struct field + a WGSL uniform + the apply wiring **per sketch**. That is independent of the camera-level tonemapping/bloom work above and not required for the neon-glow look (they already have gamma). Recommend a separate small plan once the Reinhard look is dialed in. Confirm with the operator whether to fold it in or defer.
