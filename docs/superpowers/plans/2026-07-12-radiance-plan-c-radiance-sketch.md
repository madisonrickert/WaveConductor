# Radiance Plan C: The Radiance Sketch — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the Radiance sketch end-to-end — `AppState::Radiance`, the silhouette-edge particle aura (own WGSL kernel + additive HDR billboards), the stylized silhouette fill, audio + limb-motion drive, camera arbitration, screensaver phantom performer, settings, and capture scenarios — exactly per the approved 2026-07-11 design spec.

**Architecture:** A new `crates/wc-sketches/src/radiance/` module in the canonical sketch shape (flame is the structural reference): main-world systems bake `AudioAnalysis` + `BodyTrackingState` + `SilhouetteEdges` into a `RadianceSimParams` extract resource each frame; a render-world compute plugin (particles/flame idiom: persistent uniform buffer, BufferId-keyed cached bind group, explicit `remove_*_if_absent`) dispatches `assets/shaders/radiance/simulate.wgsl` over a particle storage buffer that an additive `Material2d` billboard pass reads; a second fullscreen `Material2d` quad samples `MaskTexture` for the dark glassy silhouette + emissive rim. A deterministic synthetic-body module feeds unit tests, the attract-mode phantom, and the capture scenarios through the same `MaskTexture`/`SilhouetteEdges` resources the real tracker writes.

**Tech Stack:** bevy (existing workspace version), shared particle-engine *patterns* (compute-plugin structure, billboard vertex-index technique, POD parity-test style), WGSL; consumes Plan A (`AudioAnalysis`, `AudioCaptureRequest`, runtime-enum key `"audio_input_devices"`) + Plan B (`BodyTrackingState`, `BodyTrackingRequest`, `MaskTexture`, `SilhouetteEdges`, `EdgePoint`, `MAX_EDGE_POINTS`, `MASK_SIZE`); NO new dependencies.

**Depends on:** Plans A and B merged (pinned contracts available in wc-core). The pinned shapes are in `docs/superpowers/specs/2026-07-11-radiance-dancer-aura-sketch-design.md` and the cross-plan contracts file; this plan consumes them verbatim and treats the Plan A/B types as exported unconditionally (contract: "always present once the plugin is added").

## Global Constraints

- CI gates (run before claiming done): `cargo fmt --all -- --check`; `cargo clippy --all-targets --all-features --workspace -- -D warnings`; `cargo nextest run --workspace --all-features` plus `cargo test --doc --workspace`; `cargo doc --no-deps --workspace --document-private-items` with `RUSTDOCFLAGS="-D warnings"`; `cargo deny check`; `cargo xtask check-secrets`.
- Zero systems when `SketchActivity::Idle` except the sanctioned always-on listeners (`restart_on_settings_change`, `reload_on_resize_settled`, the window-resize debounce) and the two narrow additions this plan documents inline (the deferred hand-camera restore one-branch listener; the egui overlays that self-gate, mirroring flame's).
- No allocation in hot paths: per-frame Update systems, render-world extract/prepare systems, egui hooks. Pre-allocate at init; refill with `clear()`; persistent GPU buffers + `write_buffer`.
- No `unwrap()`/`expect()` outside tests (tests carry the house `#[allow(clippy::expect_used, reason = "test assertions")]`).
- No `as` numeric casts where `From`/`TryFrom` works; where f32→u32 sizing is inherent, use the flame-style scoped `#[allow]` with a reason naming the bound.
- WGSL never inlined in Rust — all shaders in `assets/shaders/radiance/`.
- POD structs `#[repr(C)]` + `offset_of!` parity tests (particles/particle.rs is the model) + compile-time 16-byte-multiple asserts.
- `ExtractResource` removals need explicit `remove_*_if_absent` systems in `ExtractSchedule` (removals do not propagate).
- Render-world `Local`/cached bind groups keyed on `BufferId`/`TextureViewId`, bounded by construction (single-slot replace-on-change).
- `///` rustdoc on every public item; module `//!` docs; signal/data flow documented at `RadiancePlugin::build`. Doc gate builds **default features only** — never intra-doc-link to feature-gated or lower-visibility items (use plain code spans for wc-core body/audio items).
- Commit messages contain NO backticks, are passed with plain `git commit -m` (use a second `-m` for the trailer), and end with the line: Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
- Execution note: this plan was written under a disk-critical constraint (no builds run while planning). Before executing Task 1, confirm free disk for a debug `target/` (see the spec's risk table). Pure-wiring steps verify with `cargo check -p wc-sketches` (or `-p wc-core`); full `nextest` runs are per-task as written.
---

## File Structure

**Created**

| Path | Responsibility |
|---|---|
| `crates/wc-sketches/src/radiance/mod.rs` | `RadiancePlugin` — single source of truth for wiring order; manifest registration |
| `crates/wc-sketches/src/radiance/settings.rs` | `RadianceSettings` (storage key `"radiance"`), `RadiancePalette`, `SketchLifecycle` impl |
| `crates/wc-sketches/src/radiance/compute/mod.rs` | compute submodule root |
| `crates/wc-sketches/src/radiance/compute/sim_params.rs` | GPU PODs (`RadianceParticle`, `RadianceImpulse`, `RadianceSimParamsGpu`) + `RadianceSimParams` extract resource + parity tests |
| `crates/wc-sketches/src/radiance/compute/pipeline.rs` | `RadianceComputePlugin`: pipeline init, prepare (cached bind group), dispatch, removal companion |
| `crates/wc-sketches/src/radiance/compute/edge_upload.rs` | `SilhouetteEdges` → persistent edge storage buffer, generation-keyed (no per-frame copy) |
| `crates/wc-sketches/src/radiance/render.rs` | `RadianceMaterial` (additive billboards), `RadianceSilhouetteMaterial` (mask quad), per-frame material driver |
| `crates/wc-sketches/src/radiance/systems/mod.rs` | systems submodule root |
| `crates/wc-sketches/src/radiance/systems/spawn.rs` | `RadianceRoot`, spawn/teardown, `AudioCaptureRequest`/`BodyTrackingRequest` insert/remove |
| `crates/wc-sketches/src/radiance/systems/arbitration.rs` | MediaPipe hand-camera suspend on enter / deferred restore on exit |
| `crates/wc-sketches/src/radiance/systems/sim_params.rs` | `RadianceState`, mask-UV↔world mapping, audio drive, the single `bake_radiance_sim` baker, live writer, idle freeze |
| `crates/wc-sketches/src/radiance/systems/activity.rs` | `SketchActivity` → `paused`/`idle_throttle` sync on the two request resources |
| `crates/wc-sketches/src/radiance/systems/debug.rs` | edge-point gizmos, inference readout egui overlay, synthetic-body capture driver (debug builds) |
| `crates/wc-sketches/src/radiance/synthetic.rs` | deterministic synthetic body performer: pose, mask rasterizer, edge extractor, synthetic audio |
| `crates/wc-sketches/src/radiance/screensaver.rs` | `RadianceScreensaverPlugin`: phantom performer + attract-mode sim writer |
| `assets/shaders/radiance/simulate.wgsl` | edge-respawn + curl-noise + buoyancy + impulse compute kernel |
| `assets/shaders/radiance/render.wgsl` | additive soft-disc billboard render (gradient palette, lifetime fade, sparkle) |
| `assets/shaders/radiance/silhouette.wgsl` | dark glassy fill + emissive rim, sampling `MaskTexture` |
| `assets/sketches/radiance/screenshot.png` | picker-tile image (placeholder now; real capture at smoke-test time) |

**Modified**

| Path | Change |
|---|---|
| `crates/wc-core/src/lifecycle/state.rs` | `Radiance` variant, `SKETCH_ORDER`, `from_name`, `next_sketch`/`prev_sketch`, `SketchActivity` `#[source]` arm, tests |
| `crates/wc-core/tests/ui_picker.rs` | `KNOWN_IMPLEMENTED_SKETCHES` + partition-count test |
| `crates/wc-core/src/debug/mod.rs` | `force_radiance_synthetic_body` toggle (+ its tests) |
| `crates/wc-core/src/capture/system.rs` | `toggles_json` branch + exhaustive test literal |
| `crates/wc-core/src/lifecycle/screensaver/mod.rs` | exhaustive `DebugToggles` test literal (~line 594) |
| `crates/wc-sketches/src/dots/mod.rs`, `crates/wc-sketches/src/line/mod.rs` | exhaustive `DebugToggles` test literals |
| `crates/wc-core/src/input/body/mod.rs` | three tuning fields added to `BodyTrackingRequest` (contract-sanctioned additive change) |
| `crates/wc-sketches/src/lib.rs` | `pub mod radiance;` + plugin/material/compute registrations |
| `tests/visual/scenarios.toml` | `radiance-synthetic`, `radiance-screensaver` scenarios |
| `tests/visual/CLAUDE.md` | scenario table rows + review guidance + new toggle row |

---

### Task 1: `AppState::Radiance` navigation wiring

**Files:**
- Modify: `crates/wc-core/src/lifecycle/state.rs` (enum ~line 13, `SKETCH_ORDER` ~line 37, `from_name` ~line 57, `next_sketch` ~line 86, `prev_sketch` ~line 102, `#[source]` ~line 116, tests ~line 126)
- Modify: `crates/wc-core/tests/ui_picker.rs` (`KNOWN_IMPLEMENTED_SKETCHES`, partition test)

**Interfaces:**
- Consumes: existing `AppState` / `SketchActivity` (read in full above).
- Produces: `AppState::Radiance` (all later tasks), 5-entry `SKETCH_ORDER`, `from_name("radiance")`.

- [ ] **Step 1: Update the tests first (they encode the new cycle)**

In `crates/wc-core/src/lifecycle/state.rs` tests module, replace the affected tests:

```rust
    #[test]
    fn next_sketch_wraps() {
        assert_eq!(AppState::Line.next_sketch(), AppState::Flame);
        assert_eq!(AppState::Cymatics.next_sketch(), AppState::Radiance);
        assert_eq!(AppState::Radiance.next_sketch(), AppState::Line);
    }

    #[test]
    fn prev_sketch_wraps() {
        assert_eq!(AppState::Flame.prev_sketch(), AppState::Line);
        assert_eq!(AppState::Line.prev_sketch(), AppState::Radiance);
        assert_eq!(AppState::Radiance.prev_sketch(), AppState::Cymatics);
    }

    #[test]
    fn home_navigation_returns_to_endpoints() {
        assert_eq!(AppState::Home.next_sketch(), AppState::Line);
        assert_eq!(AppState::Home.prev_sketch(), AppState::Radiance);
    }

    /// Waves stays a de-routed seam (2026-07 audit T5); Radiance entered the
    /// cycle in the 2026-07-12 Radiance plan.
    #[test]
    fn waves_arms_are_present_but_unreachable_from_the_cycle() {
        assert!(AppState::SKETCH_ORDER.contains(&AppState::Radiance));
        assert!(!AppState::SKETCH_ORDER.contains(&AppState::Waves));
        assert_eq!(AppState::Waves.next_sketch(), AppState::Line);
        assert_eq!(AppState::Waves.prev_sketch(), AppState::Radiance);
    }
```

And extend `from_name_parses_every_sketch_case_insensitively` with:

```rust
        assert_eq!(AppState::from_name("Radiance"), Some(AppState::Radiance));
```

(`next_prev_cycle_matches_sketch_order` and `is_sketch_excludes_home` adapt automatically.)

In `crates/wc-core/tests/ui_picker.rs`:
- Rename `sketch_order_iteration_yields_one_active_three_placeholder_when_only_line_registered` to `..._one_active_four_placeholder_...`, update its doc comment ("`SKETCH_ORDER` has 5 entries"), and change the assertion to `assert_eq!(placeholder.len(), 4);`.
- In `manifest_distinguishes_registered_vs_unregistered_sketches`, extend the unregistered loop to `[AppState::Dots, AppState::Cymatics, AppState::Radiance, AppState::Waves]`.
- In `sketch_order_entries_are_all_known_implemented_sketches`, grow the const:

```rust
    const KNOWN_IMPLEMENTED_SKETCHES: [AppState; 5] = [
        AppState::Line,
        AppState::Flame,
        AppState::Dots,
        AppState::Cymatics,
        AppState::Radiance,
    ];
```

(This plan implements the Radiance plugin + manifest registration in Task 2, so the deliberate-acknowledgement contract of that test is honored.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p wc-core state:: ui_picker`
Expected: FAIL — compile error (`AppState` has no variant `Radiance`).

- [ ] **Step 3: Write the implementation**

In `crates/wc-core/src/lifecycle/state.rs`:

Add the variant after `Cymatics` (keep `Waves` last, it is the reserved seam):

```rust
pub enum AppState {
    #[default]
    Home,
    Line,
    Flame,
    Dots,
    Cymatics,
    Radiance,
    Waves,
}
```

Replace `SKETCH_ORDER` (keep the existing doc comment, updating the count prose):

```rust
    pub const SKETCH_ORDER: [Self; 5] = [
        Self::Line,
        Self::Flame,
        Self::Dots,
        Self::Cymatics,
        Self::Radiance,
    ];
```

In `from_name`, add before the `_ => None` arm:

```rust
            "radiance" => Some(Self::Radiance),
```

Replace the two cycle functions' match bodies (doc comments unchanged apart from mentioning Radiance):

```rust
    #[must_use]
    pub fn next_sketch(self) -> Self {
        match self {
            Self::Home | Self::Radiance | Self::Waves => Self::Line,
            Self::Line => Self::Flame,
            Self::Flame => Self::Dots,
            Self::Dots => Self::Cymatics,
            Self::Cymatics => Self::Radiance,
        }
    }

    #[must_use]
    pub fn prev_sketch(self) -> Self {
        match self {
            Self::Home | Self::Line | Self::Waves => Self::Radiance,
            Self::Flame => Self::Line,
            Self::Dots => Self::Flame,
            Self::Cymatics => Self::Dots,
            Self::Radiance => Self::Cymatics,
        }
    }
```

Extend the `SketchActivity` source arm:

```rust
#[source(AppState = AppState::Line | AppState::Flame | AppState::Dots
                  | AppState::Cymatics | AppState::Radiance | AppState::Waves)]
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p wc-core state:: ui_picker`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/lifecycle/state.rs crates/wc-core/tests/ui_picker.rs
git commit -m "feat(radiance): add AppState::Radiance to the sketch cycle" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: `RadianceSettings`, palette, minimal plugin + picker tile

**Files:**
- Create: `crates/wc-sketches/src/radiance/settings.rs`
- Create: `crates/wc-sketches/src/radiance/mod.rs` (minimal `RadiancePlugin`; grows in later tasks)
- Create: `assets/sketches/radiance/screenshot.png` (generated placeholder)
- Modify: `crates/wc-sketches/src/lib.rs` (module decl + plugin registration)

**Interfaces:**
- Consumes: `wc_core::settings::RegisterSketchSettingsExt`, `wc_core::sketch::{SketchLifecycle, RenderProfile, register_sketch_tile, restart_on_settings_change, reload_on_resize_settled, apply_render_profile}`, `wc_core::render::{TonemapChoice, BloomComposite}`, `wc_core_macros::SketchSettings`, `AppState::Radiance` (Task 1). The `audio_input_device` field binds `SettingKind::RuntimeEnum { options_key: "audio_input_devices" }` — Plan A registers the options source under that key.
- Produces: `RadianceSettings` (all later tasks), `RadiancePalette` + `RadiancePalette::stops()`, `RadiancePlugin`, `register_radiance_manifest`.

- [ ] **Step 1: Write the failing tests (inside the new settings.rs, plus the manifest test in mod.rs)**

The tests are written together with the structs below (single new file); the failing state is "module does not exist yet" — proceed to write both files, then run.

- [ ] **Step 2: Write `settings.rs` (complete)**

```rust
//! Radiance sketch settings.
//!
//! Storage key `"radiance"`. Radiance *listens* rather than plays: there is no
//! synth section. `audio_input_device` is the app's first `RuntimeEnum`
//! setting — its option list comes from Plan A's device enumeration registered
//! under the `"audio_input_devices"` options key — and is `requires_restart`
//! so a device change tears down and rebuilds the capture stream via the
//! standard reload path. `particle_count` is `requires_restart` because the
//! GPU particle buffer and billboard mesh are sized once at spawn.
//!
//! Per-field serde defaults follow the house pattern: every field carries
//! `#[serde(default = "default_<name>")]` so legacy TOML deserializes cleanly,
//! and the two defaults-match tests below keep both sites in sync.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

/// Curated psychedelic gradient palettes for the aura particles. Each palette
/// is three linear-HDR gradient stops (values may exceed 1.0 — the additive
/// pipeline + bloom read them as emissive headroom); the render shader
/// interpolates a→b→c over the per-particle gradient coordinate, and the audio
/// drive slowly shifts that coordinate along the gradient.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RadiancePalette {
    /// Violet → magenta → gold. The default "prismatic" look.
    #[default]
    Prism,
    /// Deep red → orange → warm white. Also the screensaver's ember override.
    Ember,
    /// Teal → green → violet.
    Aurora,
    /// Deep blue → cyan → pale ice.
    Ocean,
}

impl RadiancePalette {
    /// The three linear-HDR gradient stops `[a, b, c]` (w unused, kept 1.0).
    #[must_use]
    pub fn stops(self) -> [Vec4; 3] {
        match self {
            Self::Prism => [
                Vec4::new(0.35, 0.10, 1.00, 1.0),
                Vec4::new(1.00, 0.25, 0.85, 1.0),
                Vec4::new(1.00, 0.85, 0.30, 1.0),
            ],
            Self::Ember => [
                Vec4::new(0.50, 0.08, 0.02, 1.0),
                Vec4::new(1.00, 0.35, 0.05, 1.0),
                Vec4::new(1.00, 0.80, 0.35, 1.0),
            ],
            Self::Aurora => [
                Vec4::new(0.05, 0.60, 0.50, 1.0),
                Vec4::new(0.20, 0.90, 0.40, 1.0),
                Vec4::new(0.60, 0.40, 1.00, 1.0),
            ],
            Self::Ocean => [
                Vec4::new(0.05, 0.25, 0.90, 1.0),
                Vec4::new(0.10, 0.70, 1.00, 1.0),
                Vec4::new(0.70, 0.95, 1.00, 1.0),
            ],
        }
    }
}

/// User-tunable parameters for the Radiance sketch.
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "radiance")]
pub struct RadianceSettings {
    /// GPU particle budget. The storage buffer and billboard mesh are sized
    /// once at spawn, so this requires a restart (reload fade) to apply.
    #[setting(
        default = 120_000.0_f32,
        min = 10_000.0_f32,
        max = 300_000.0_f32,
        step = 10_000.0_f32,
        label = "Particle count",
        section = "Simulation",
        category = User,
        requires_restart
    )]
    #[serde(default = "default_particle_count")]
    pub particle_count: f32,

    /// Baseline emission: the per-second respawn pressure on dead particles
    /// (scaled by the bass drive). 0 = no new particles.
    #[setting(
        default = 0.5_f32,
        min = 0.0_f32,
        max = 1.0_f32,
        step = 0.01_f32,
        label = "Emission",
        section = "Simulation",
        category = User
    )]
    #[serde(default = "default_emission_rate")]
    pub emission_rate: f32,

    /// Curl-noise flow advection speed in world px/s (scaled by the highs
    /// drive). The primary "how alive is the aura" knob.
    #[setting(
        default = 90.0_f32,
        min = 0.0_f32,
        max = 400.0_f32,
        step = 5.0_f32,
        label = "Flow strength",
        section = "Simulation",
        category = User
    )]
    #[serde(default = "default_flow_strength")]
    pub flow_strength: f32,

    /// Constant upward acceleration in world px/s² — the flame-like rise
    /// (pulsed by the bass drive).
    #[setting(
        default = 60.0_f32,
        min = 0.0_f32,
        max = 300.0_f32,
        step = 5.0_f32,
        label = "Buoyancy",
        section = "Simulation",
        category = User
    )]
    #[serde(default = "default_buoyancy")]
    pub buoyancy: f32,

    /// Curl-noise octave count (1–3). More octaves = finer swirl detail at a
    /// small per-particle ALU cost.
    #[setting(
        default = 3_u32,
        min = 1_u32,
        max = 3_u32,
        step = 1_u32,
        label = "Curl octaves",
        section = "Simulation",
        category = Dev
    )]
    #[serde(default = "default_curl_octaves")]
    pub curl_octaves: u32,

    /// Aura gradient palette.
    #[setting(
        default = RadiancePalette::Prism,
        ty = Enum,
        label = "Palette",
        section = "Look",
        category = User
    )]
    #[serde(default = "default_palette")]
    pub palette: RadiancePalette,

    /// Silhouette fill intensity: strength of the dark glassy body fill.
    #[setting(
        default = 0.8_f32,
        min = 0.0_f32,
        max = 2.0_f32,
        step = 0.05_f32,
        label = "Silhouette fill",
        section = "Look",
        category = User
    )]
    #[serde(default = "default_silhouette_fill")]
    pub silhouette_fill: f32,

    /// Emissive rim brightness in the mask's edge band (HDR — feeds bloom).
    #[setting(
        default = 1.2_f32,
        min = 0.0_f32,
        max = 4.0_f32,
        step = 0.05_f32,
        label = "Rim glow",
        section = "Look",
        category = User
    )]
    #[serde(default = "default_rim_glow")]
    pub rim_glow: f32,

    /// Mirror the image horizontally (it is a mirror for the dancer). On by
    /// default per the spec.
    #[setting(
        default = true,
        label = "Mirror",
        section = "Look",
        category = User
    )]
    #[serde(default = "default_mirror")]
    pub mirror: bool,

    /// Master scale on every audio→visual coupling (emission, buoyancy,
    /// turbulence, burst, intensity). 0 = motion-drive only.
    #[setting(
        default = 1.0_f32,
        min = 0.0_f32,
        max = 3.0_f32,
        step = 0.05_f32,
        label = "Audio sensitivity",
        section = "Audio",
        category = User
    )]
    #[serde(default = "default_audio_sensitivity")]
    pub audio_sensitivity: f32,

    /// Capture device name. Empty = system default input. Options come from
    /// the runtime-enum source registered under "audio_input_devices" (Plan
    /// A's cpal enumeration); restart rebuilds the stream on the new device.
    #[setting(
        default = String::new(),
        ty = RuntimeEnum,
        options_key = "audio_input_devices",
        label = "Audio input",
        section = "Audio",
        category = User,
        requires_restart
    )]
    #[serde(default = "default_audio_input_device")]
    pub audio_input_device: String,

    /// Mask threshold for the silhouette fill/rim edge (render-side; the edge
    /// *point* extraction threshold is fixed at 0.5 by the body-tracking
    /// contract).
    #[setting(
        default = 0.5_f32,
        min = 0.05_f32,
        max = 0.95_f32,
        step = 0.01_f32,
        label = "Mask threshold",
        section = "Tracking",
        category = Dev
    )]
    #[serde(default = "default_mask_threshold")]
    pub mask_threshold: f32,

    /// Worker-side temporal EMA factor on the segmentation mask (higher =
    /// steadier, laggier). Routed through the body-tracking request on
    /// restart (Task 14).
    #[setting(
        default = 0.6_f32,
        min = 0.0_f32,
        max = 0.98_f32,
        step = 0.02_f32,
        label = "Mask smoothing",
        section = "Tracking",
        category = Dev,
        requires_restart
    )]
    #[serde(default = "default_mask_ema")]
    pub mask_ema: f32,

    /// One-Euro landmark filter min-cutoff (Hz). Routed like mask smoothing.
    #[setting(
        default = 1.0_f32,
        min = 0.01_f32,
        max = 10.0_f32,
        step = 0.01_f32,
        label = "One-Euro min cutoff",
        section = "Tracking",
        category = Dev,
        requires_restart
    )]
    #[serde(default = "default_one_euro_min_cutoff")]
    pub one_euro_min_cutoff: f32,

    /// One-Euro landmark filter beta (speed coefficient). Routed like mask
    /// smoothing.
    #[setting(
        default = 0.05_f32,
        min = 0.0_f32,
        max = 1.0_f32,
        step = 0.005_f32,
        label = "One-Euro beta",
        section = "Tracking",
        category = Dev,
        requires_restart
    )]
    #[serde(default = "default_one_euro_beta")]
    pub one_euro_beta: f32,

    /// Draw the raw segmentation mask grayscale instead of the styled fill.
    #[setting(
        default = false,
        label = "Mask debug overlay",
        section = "Debug",
        category = Dev
    )]
    #[serde(default = "default_mask_debug_overlay")]
    pub mask_debug_overlay: bool,

    /// Draw a gizmo tick + outward normal at every silhouette edge point.
    #[setting(
        default = false,
        label = "Edge-point debug",
        section = "Debug",
        category = Dev
    )]
    #[serde(default = "default_edge_debug")]
    pub edge_debug: bool,

    /// Show the tracking/audio readout overlay (presence, confidence, body
    /// frame rate, edge count, RMS/onset).
    #[setting(
        default = false,
        label = "Inference readouts",
        section = "Debug",
        category = Dev
    )]
    #[serde(default = "default_inference_readouts")]
    pub inference_readouts: bool,

    /// Camera tonemapping operator while Radiance is active. House default.
    #[setting(
        default = wc_core::render::TonemapChoice::ReinhardLuminance,
        ty = Enum,
        label = "Tonemapping",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_tonemapping")]
    pub tonemapping: wc_core::render::TonemapChoice,

    /// Bloom intensity for this sketch (main camera).
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

    /// Bloom prefilter threshold (0.0 pairs with `EnergyConserving`).
    #[setting(
        default = 0.0_f32,
        min = 0.0_f32,
        max = 3.0_f32,
        step = 0.05_f32,
        label = "Bloom threshold",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_bloom_threshold")]
    pub bloom_threshold: f32,

    /// Bloom composite mode.
    #[setting(
        default = wc_core::render::BloomComposite::EnergyConserving,
        ty = Enum,
        label = "Bloom composite",
        section = "Visual",
        category = Dev
    )]
    #[serde(default = "default_bloom_composite")]
    pub bloom_composite: wc_core::render::BloomComposite,
}

/// Ties `RadianceSettings` to the shared sketch lifecycle glue.
impl wc_core::sketch::SketchLifecycle for RadianceSettings {
    const STATE: wc_core::lifecycle::state::AppState =
        wc_core::lifecycle::state::AppState::Radiance;

    fn render_profile(&self) -> wc_core::sketch::RenderProfile {
        wc_core::sketch::RenderProfile {
            tonemapping: self.tonemapping,
            bloom_intensity: self.bloom_intensity,
            bloom_threshold: self.bloom_threshold,
            bloom_composite: self.bloom_composite,
        }
    }
}

// Per-field serde defaults. Values MUST match the `#[setting(default = ...)]`
// attributes above; update both sites together.
fn default_particle_count() -> f32 {
    120_000.0
}
fn default_emission_rate() -> f32 {
    0.5
}
fn default_flow_strength() -> f32 {
    90.0
}
fn default_buoyancy() -> f32 {
    60.0
}
fn default_curl_octaves() -> u32 {
    3
}
fn default_palette() -> RadiancePalette {
    RadiancePalette::Prism
}
fn default_silhouette_fill() -> f32 {
    0.8
}
fn default_rim_glow() -> f32 {
    1.2
}
fn default_mirror() -> bool {
    true
}
fn default_audio_sensitivity() -> f32 {
    1.0
}
fn default_audio_input_device() -> String {
    String::new()
}
fn default_mask_threshold() -> f32 {
    0.5
}
fn default_mask_ema() -> f32 {
    0.6
}
fn default_one_euro_min_cutoff() -> f32 {
    1.0
}
fn default_one_euro_beta() -> f32 {
    0.05
}
fn default_mask_debug_overlay() -> bool {
    false
}
fn default_edge_debug() -> bool {
    false
}
fn default_inference_readouts() -> bool {
    false
}
fn default_tonemapping() -> wc_core::render::TonemapChoice {
    wc_core::render::TonemapChoice::ReinhardLuminance
}
fn default_bloom_intensity() -> f32 {
    0.35
}
fn default_bloom_threshold() -> f32 {
    0.0
}
fn default_bloom_composite() -> wc_core::render::BloomComposite {
    wc_core::render::BloomComposite::EnergyConserving
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Legacy persisted TOML missing fields still deserializes cleanly;
    /// siblings preserved (per-field serde defaults, the house pattern).
    #[test]
    #[allow(clippy::expect_used, reason = "test-only")]
    fn missing_field_preserves_sibling_values() {
        let legacy = r#"
            emission_rate = 0.7
            mirror = false
        "#;
        let parsed: RadianceSettings = toml::from_str(legacy).expect("legacy TOML must parse");
        assert!((parsed.emission_rate - 0.7).abs() < 1e-6);
        assert!(!parsed.mirror);
        assert!((parsed.particle_count - 120_000.0).abs() < 1e-6, "sibling default");
        assert!((parsed.flow_strength - 90.0).abs() < 1e-6, "sibling default");
        assert_eq!(parsed.palette, RadiancePalette::Prism, "sibling default");
        assert!(
            parsed.audio_input_device.is_empty(),
            "missing device falls back to system default"
        );
    }

    /// Every `#[setting(default = ...)]` matches its `default_*` serde fn.
    #[test]
    fn default_values_match_serde_defaults() {
        let d = RadianceSettings::default();
        assert!((d.particle_count - default_particle_count()).abs() < f32::EPSILON);
        assert!((d.emission_rate - default_emission_rate()).abs() < f32::EPSILON);
        assert!((d.flow_strength - default_flow_strength()).abs() < f32::EPSILON);
        assert!((d.buoyancy - default_buoyancy()).abs() < f32::EPSILON);
        assert_eq!(d.curl_octaves, default_curl_octaves());
        assert_eq!(d.palette, default_palette());
        assert!((d.silhouette_fill - default_silhouette_fill()).abs() < f32::EPSILON);
        assert!((d.rim_glow - default_rim_glow()).abs() < f32::EPSILON);
        assert_eq!(d.mirror, default_mirror());
        assert!((d.audio_sensitivity - default_audio_sensitivity()).abs() < f32::EPSILON);
        assert_eq!(d.audio_input_device, default_audio_input_device());
        assert!((d.mask_threshold - default_mask_threshold()).abs() < f32::EPSILON);
        assert!((d.mask_ema - default_mask_ema()).abs() < f32::EPSILON);
        assert!((d.one_euro_min_cutoff - default_one_euro_min_cutoff()).abs() < f32::EPSILON);
        assert!((d.one_euro_beta - default_one_euro_beta()).abs() < f32::EPSILON);
        assert_eq!(d.mask_debug_overlay, default_mask_debug_overlay());
        assert_eq!(d.edge_debug, default_edge_debug());
        assert_eq!(d.inference_readouts, default_inference_readouts());
        assert_eq!(d.tonemapping, default_tonemapping());
        assert!((d.bloom_intensity - default_bloom_intensity()).abs() < f32::EPSILON);
        assert!((d.bloom_threshold - default_bloom_threshold()).abs() < f32::EPSILON);
        assert_eq!(d.bloom_composite, default_bloom_composite());
    }

    /// Every palette returns three finite stops (HDR values allowed above 1).
    #[test]
    fn palette_stops_are_finite() {
        for p in [
            RadiancePalette::Prism,
            RadiancePalette::Ember,
            RadiancePalette::Aurora,
            RadiancePalette::Ocean,
        ] {
            for stop in p.stops() {
                assert!(stop.is_finite(), "{p:?} stop {stop:?}");
                assert!(stop.min_element() >= 0.0, "{p:?} stop {stop:?}");
            }
        }
    }
}
```

- [ ] **Step 3: Write the minimal `mod.rs` (grows one stage at a time, flame-style)**

```rust
//! Radiance sketch: a webcam-tracked dancer's silhouette rendered as a dark
//! glassy form with an emissive rim, wrapped in an aura of additive HDR
//! particles born on the silhouette edge and driven by curl-noise flow,
//! buoyancy, limb motion, and live audio input. Radiance does not generate
//! audio; it listens (Plan A's input analysis) and watches (Plan B's body
//! tracking).
//!
//! ## Data flow (grows stage by stage; see the 2026-07-12 Plan C document)
//!
//! 1. Settings register with the shared panel/persistence system; the
//!    `RenderProfile` applier drives the main camera's tonemapping/bloom
//!    while Radiance is active.
//! 2. Later tasks add: spawn/teardown, the sim baker, the render-world
//!    compute pipeline, materials, camera arbitration, activity sync, the
//!    screensaver phantom, and debug/capture drivers. `build` below stays the
//!    single source of truth for wiring order.

pub mod settings;

use bevy::prelude::*;
use wc_core::lifecycle::state::AppState;
use wc_core::settings::RegisterSketchSettingsExt;

/// Plugin that registers the Radiance sketch.
pub struct RadiancePlugin;

impl Plugin for RadiancePlugin {
    fn build(&self, app: &mut App) {
        // Settings: panel + persistence (storage key "radiance").
        app.register_sketch_settings::<settings::RadianceSettings>();

        // Picker-tile manifest entry (async screenshot load).
        register_radiance_manifest(app);

        // Restart listener (requires_restart fields fade out/in via the
        // shared reload overlay). Always-on sanctioned listener.
        app.add_systems(
            Update,
            wc_core::sketch::restart_on_settings_change::<settings::RadianceSettings>,
        );

        // Re-run the spawn path at the new window size when a resize settles
        // (silent/instant reload). Always-on sanctioned listener; defensive
        // add_message mirrors FlamePlugin (Bevy dedups; LifecyclePlugin is
        // canonical).
        app.add_message::<wc_core::lifecycle::window_resize::WindowResizeSettled>();
        app.add_systems(
            Update,
            wc_core::sketch::reload_on_resize_settled::<settings::RadianceSettings>,
        );

        // Tonemapping + bloom profile onto the main camera while Radiance is
        // up (live dev-panel tuning), via the shared generic applier.
        app.add_systems(
            Update,
            wc_core::sketch::apply_render_profile::<settings::RadianceSettings>
                .run_if(in_state(AppState::Radiance)),
        );
    }
}

/// Register Radiance's picker-tile metadata. Factored out of
/// `RadiancePlugin::build` so it is unit-testable without rendering plugins
/// (mirrors `register_flame_manifest`).
pub(crate) fn register_radiance_manifest(app: &mut App) {
    use wc_core::settings::SketchSettings as _;
    wc_core::sketch::register_sketch_tile(
        app,
        AppState::Radiance,
        "Radiance",
        settings::RadianceSettings::STORAGE_KEY,
        "sketches/radiance/screenshot.png",
    );
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use wc_core::sketch::SketchManifest;

    /// Mirrors `register_flame_manifest_appends_entry`: the free-function path
    /// registers a Radiance tile without needing a `RenderApp`.
    #[test]
    fn register_radiance_manifest_appends_entry() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::asset::AssetPlugin::default());
        app.add_plugins(bevy::image::ImagePlugin::default());
        register_radiance_manifest(&mut app);
        let manifest = app.world().resource::<SketchManifest>();
        let entry = manifest
            .get(AppState::Radiance)
            .expect("Radiance manifest entry should be registered");
        assert_eq!(entry.display_name, "Radiance");
        assert_eq!(entry.settings_key, "radiance");
    }
}
```

In `crates/wc-sketches/src/lib.rs`, add the module declaration alongside the others:

```rust
pub mod radiance;
```

and register the plugin at the end of `SketchesPlugin::build`, after `CymaticsPlugin`:

```rust
        // Radiance lifecycle (settings, tile; sim/render/attract arrive in
        // later Plan C tasks — compute + material plugins are registered
        // above once they exist).
        app.add_plugins(radiance::RadiancePlugin);
```

- [ ] **Step 4: Generate the placeholder tile PNG (stdlib-only Python, no deps)**

```bash
mkdir -p assets/sketches/radiance
python3 - <<'PY'
import struct, zlib
W, H = 800, 450
rows = []
for y in range(H):
    row = bytearray([0])
    for x in range(W):
        # Dark indigo field with a soft warm radial glow low-center: a
        # placeholder that reads as "Radiance" until the real capture lands.
        dx, dy = (x - W / 2) / W, (y - H * 0.62) / H
        g = max(0.0, 1.0 - (dx * dx + dy * dy) * 9.0)
        r = int(14 + 160 * g * g)
        gr = int(10 + 60 * g * g)
        b = int(28 + 90 * g)
        row += bytes((min(r, 255), min(gr, 255), min(b, 255)))
    rows.append(bytes(row))
raw = b"".join(rows)
def chunk(t, d):
    c = struct.pack(">I", len(d)) + t + d
    return c + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
png = b"\x89PNG\r\n\x1a\n"
png += chunk(b"IHDR", struct.pack(">IIBBBBB", W, H, 8, 2, 0, 0, 0))
png += chunk(b"IDAT", zlib.compress(raw, 9))
png += chunk(b"IEND", b"")
open("assets/sketches/radiance/screenshot.png", "wb").write(png)
print("wrote placeholder", W, "x", H)
PY
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p wc-sketches radiance::`
Expected: PASS (settings defaults tests, TOML test, manifest test).

- [ ] **Step 6: Commit**

```bash
git add crates/wc-sketches/src/radiance crates/wc-sketches/src/lib.rs assets/sketches/radiance/screenshot.png
git commit -m "feat(radiance): settings, palette, picker tile, minimal plugin" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: GPU PODs + extract resource (`compute/sim_params.rs`)

**Files:**
- Create: `crates/wc-sketches/src/radiance/compute/mod.rs`
- Create: `crates/wc-sketches/src/radiance/compute/sim_params.rs`
- Modify: `crates/wc-sketches/src/radiance/mod.rs` (add `pub mod compute;`)

**Interfaces:**
- Consumes: `bevy::render::extract_resource::ExtractResource`, `bevy::render::storage::ShaderBuffer`, `bytemuck::{Pod, Zeroable}`.
- Produces: `RadianceParticle` (32 B), `RadianceImpulse` (32 B), `RadianceSimParamsGpu` (336 B), `MAX_IMPULSES = 8`, `RadianceSimParams` extract resource — consumed by Tasks 4–13.

- [ ] **Step 1: Write the file with tests included (failing state = module absent; the offset tests ARE the spec)**

`crates/wc-sketches/src/radiance/compute/mod.rs`:

```rust
//! Render-world compute for the Radiance aura: GPU POD mirrors
//! ([`sim_params`]), the dispatch pipeline ([`pipeline`]), and the
//! silhouette-edge storage-buffer upload ([`edge_upload`]).

pub mod sim_params;
```

(`pipeline` and `edge_upload` module lines are added by Task 6.)

`crates/wc-sketches/src/radiance/compute/sim_params.rs`:

```rust
//! GPU-side POD mirrors for the Radiance particle kernel, plus the extract
//! resource the render world reads.
//!
//! Layout contract with `assets/shaders/radiance/simulate.wgsl` and
//! `assets/shaders/radiance/render.wgsl` (kernel parity discipline: change
//! all copies together, field for field). All structs are 16-byte-multiple
//! sized, compile-time asserted, and locked by `offset_of!` tests below.

use bevy::prelude::*;
use bevy::render::extract_resource::ExtractResource;
use bevy::render::storage::ShaderBuffer;
use bytemuck::{Pod, Zeroable};

/// One aura particle. 32 bytes, matching the WGSL `struct Particle` in both
/// radiance shaders.
///
/// A particle is **dead** when `age >= lifespan`; `Zeroable::zeroed()` (age 0,
/// lifespan 0) is therefore dead, so the spawn-time buffer needs no CPU
/// seeding — the kernel's edge-respawn path births every particle.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct RadianceParticle {
    /// World-space X/Y position (Camera2d units: 1 unit = 1 px, origin center).
    pub position: [f32; 2],
    /// X/Y velocity in world px/s.
    pub velocity: [f32; 2],
    /// Seconds since this particle's last respawn.
    pub age: f32,
    /// Seconds this particle lives; kernel-assigned at respawn from a hash in
    /// `[lifespan_min, lifespan_max]` so deaths stagger instead of pulsing.
    pub lifespan: f32,
    /// Deterministic per-respawn hash in `0..=1`: the render shader's gradient
    /// coordinate and sparkle phase.
    pub seed: f32,
    /// Padding to a 16-byte multiple for WGSL storage rules.
    #[allow(
        clippy::pub_underscore_fields,
        reason = "GPU struct layout padding must be pub for bytemuck"
    )]
    pub _pad: f32,
}

/// One limb impulse slot — the fixed-slot idiom of the shared particle
/// engine's `Attractor[8]`. 32 bytes, matching WGSL `struct Impulse`.
///
/// `gain == 0.0` means inactive; the kernel skips zero-gain entries.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct RadianceImpulse {
    /// World-space X/Y position of the limb.
    pub position: [f32; 2],
    /// Limb velocity in world px/s — particles near the limb inherit a
    /// locally-weighted share of it, so a fast limb sheds a burst.
    pub velocity: [f32; 2],
    /// Influence radius in world px; the coupling fades to zero by `radius`.
    pub radius: f32,
    /// Coupling gain `0..=1` (CPU-derived from limb speed).
    pub gain: f32,
    /// Padding to a 32-byte (16-multiple) stride for the WGSL uniform array.
    #[allow(
        clippy::pub_underscore_fields,
        reason = "GPU struct layout padding must be pub for bytemuck"
    )]
    pub _pad: [f32; 2],
}

/// Maximum simultaneous limb impulses. Seven landmark slots are used today
/// (nose, wrists, hips, ankles — see `systems::sim_params::IMPULSE_LANDMARKS`);
/// the eighth is headroom, same shape as the particle engine's
/// `MAX_ATTRACTORS`.
pub const MAX_IMPULSES: usize = 8;

/// Compute-kernel uniforms pushed every frame.
///
/// Field order matches the WGSL `struct SimParams` in `simulate.wgsl`
/// exactly; the layout is `#[repr(C)]` so `bytemuck::bytes_of` produces the
/// correct byte sequence. The scalar header totals 80 bytes — a 16-byte
/// multiple — so the `impulses` array (16-byte-aligned per WGSL uniform
/// rules, 32-byte stride) begins aligned at offset 80. Total size 336.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct RadianceSimParamsGpu {
    /// Frame time in seconds (capped to 50 ms to avoid blow-up on pauses).
    pub dt: f32,
    /// Elapsed virtual time in seconds — scrolls the curl field and salts the
    /// respawn hash per frame.
    pub time: f32,
    /// Per-dead-particle respawn probability THIS FRAME (already
    /// `rate × dt`-baked and clamped to `0..=1` by the CPU baker). `0.0`
    /// freezes emission (the Idle hook's write).
    pub emission_prob: f32,
    /// Live entries in the edge storage buffer. `0` = no silhouette this
    /// frame → the respawn path is skipped entirely.
    pub edge_count: u32,
    /// Particle buffer length; the kernel also guards with `arrayLength`.
    pub particle_count: u32,
    /// Spawn offset along the outward normal, world px.
    pub spawn_offset: f32,
    /// Initial speed along the outward normal, world px/s.
    pub spawn_speed: f32,
    /// Extra outward speed from the onset burst envelope, world px/s.
    pub burst_speed: f32,
    /// Upward acceleration, world px/s² (bass-pulsed by the baker).
    pub buoyancy: f32,
    /// Curl-flow advection speed, world px/s (highs-scaled by the baker).
    pub flow_strength: f32,
    /// Curl spatial frequency, radians per world px.
    pub curl_scale: f32,
    /// Curl octave count, clamped `1..=3` in the kernel.
    pub curl_octaves: u32,
    /// Per-frame velocity retention, baked CPU-side as
    /// `DRAG_PER_SECOND.powf(dt)` so drag is framerate-independent.
    pub drag_baked: f32,
    /// Respawn lifespan range, seconds (kernel hashes within it).
    pub lifespan_min: f32,
    /// See `lifespan_min`.
    pub lifespan_max: f32,
    /// `1` = mirror horizontally (flip mask-UV x); `0` = as-captured.
    pub mirror: u32,
    /// Mask-UV → world scale: `world = ((u - 0.5) * x, (0.5 - v) * y)`.
    /// The CPU-side twin is `systems::sim_params::mask_uv_to_world`; both
    /// sides must stay term-for-term identical.
    pub uv_to_world: [f32; 2],
    /// Live impulse slots (`impulses[0..impulse_count]`), capped at
    /// [`MAX_IMPULSES`].
    pub impulse_count: u32,
    /// Padding: keeps the scalar header at 80 bytes (16-multiple) so the
    /// `impulses` array stays aligned.
    #[allow(
        clippy::pub_underscore_fields,
        reason = "GPU struct layout padding must be pub for bytemuck"
    )]
    pub _pad0: f32,
    /// Impulse slots; entries past `impulse_count` are zero-gain and ignored.
    pub impulses: [RadianceImpulse; MAX_IMPULSES],
}

const _: () = {
    assert!(std::mem::size_of::<RadianceParticle>().is_multiple_of(16));
    assert!(std::mem::size_of::<RadianceImpulse>().is_multiple_of(16));
    assert!(std::mem::size_of::<RadianceSimParamsGpu>().is_multiple_of(16));
};

/// Extract resource mirrored into the render world each frame. POD fields +
/// one `Handle`, so the `ExtractResourcePlugin` clone is a memcpy (no heap —
/// the Cymatics F2 lesson).
#[derive(Resource, Clone, ExtractResource)]
pub struct RadianceSimParams {
    /// Per-frame kernel uniforms (baked by `systems::sim_params`).
    pub params: RadianceSimParamsGpu,
    /// The particle storage buffer (owned here; the render material clones it).
    pub particles: Handle<ShaderBuffer>,
    /// Particle buffer length — determines dispatch size.
    pub particle_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `RadianceParticle` field offsets must match the WGSL `struct Particle`
    /// in `simulate.wgsl` / `render.wgsl` exactly.
    #[test]
    fn particle_field_offsets_match_wgsl() {
        assert_eq!(std::mem::offset_of!(RadianceParticle, position), 0);
        assert_eq!(std::mem::offset_of!(RadianceParticle, velocity), 8);
        assert_eq!(std::mem::offset_of!(RadianceParticle, age), 16);
        assert_eq!(std::mem::offset_of!(RadianceParticle, lifespan), 20);
        assert_eq!(std::mem::offset_of!(RadianceParticle, seed), 24);
        assert_eq!(std::mem::offset_of!(RadianceParticle, _pad), 28);
        assert_eq!(std::mem::size_of::<RadianceParticle>(), 32);
    }

    /// A zeroed particle is dead (age 0 >= lifespan 0): the spawn buffer
    /// needs no CPU seeding.
    #[test]
    fn zeroed_particle_is_dead() {
        let p = RadianceParticle::zeroed();
        assert!(p.age >= p.lifespan);
    }

    /// `RadianceImpulse` offsets must match WGSL `struct Impulse`.
    #[test]
    fn impulse_field_offsets_match_wgsl() {
        assert_eq!(std::mem::offset_of!(RadianceImpulse, position), 0);
        assert_eq!(std::mem::offset_of!(RadianceImpulse, velocity), 8);
        assert_eq!(std::mem::offset_of!(RadianceImpulse, radius), 16);
        assert_eq!(std::mem::offset_of!(RadianceImpulse, gain), 20);
        assert_eq!(std::mem::offset_of!(RadianceImpulse, _pad), 24);
        assert_eq!(std::mem::size_of::<RadianceImpulse>(), 32);
    }

    /// `RadianceSimParamsGpu` offsets must match the WGSL `struct SimParams`
    /// in `simulate.wgsl` exactly; a reorder silently corrupts every
    /// dispatch's uniforms.
    #[test]
    fn sim_params_field_offsets_match_wgsl() {
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, dt), 0);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, time), 4);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, emission_prob), 8);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, edge_count), 12);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, particle_count), 16);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, spawn_offset), 20);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, spawn_speed), 24);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, burst_speed), 28);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, buoyancy), 32);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, flow_strength), 36);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, curl_scale), 40);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, curl_octaves), 44);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, drag_baked), 48);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, lifespan_min), 52);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, lifespan_max), 56);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, mirror), 60);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, uv_to_world), 64);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, impulse_count), 72);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, _pad0), 76);
        assert_eq!(std::mem::offset_of!(RadianceSimParamsGpu, impulses), 80);
    }

    /// Locks the "header 80 bytes, total 336" claim to the real const, so a
    /// change to `MAX_IMPULSES` cannot silently shift the size expectations.
    #[test]
    fn sim_params_size_tracks_max_impulses() {
        const HEADER_BYTES: usize = 80;
        const IMPULSE_STRIDE: usize = 32;
        assert_eq!(
            std::mem::size_of::<RadianceSimParamsGpu>(),
            HEADER_BYTES + MAX_IMPULSES * IMPULSE_STRIDE
        );
    }
}
```

Add `pub mod compute;` to `crates/wc-sketches/src/radiance/mod.rs` (module list at the top).

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo nextest run -p wc-sketches radiance::compute::sim_params`
Expected: PASS (5 tests). If an offset assertion fails, the struct is wrong, not the test — the offsets above ARE the WGSL contract.

- [ ] **Step 3: Commit**

```bash
git add crates/wc-sketches/src/radiance
git commit -m "feat(radiance): GPU POD layouts and extract resource with offset parity tests" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: Simulation kernel `assets/shaders/radiance/simulate.wgsl`

**Files:**
- Create: `assets/shaders/radiance/simulate.wgsl`

**Interfaces:**
- Consumes: the POD layouts locked in Task 3; `EdgePoint { pos: vec2, normal: vec2 }` from the pinned Plan B contract (16-byte stride).
- Produces: entry point `main`, `@workgroup_size(64)`, bind group 0 = { 0: SimParams uniform, 1: Particle storage rw, 2: EdgePoint storage ro } — consumed by Task 6's pipeline.

- [ ] **Step 1: Write the shader (complete)**

```wgsl
// Radiance aura simulation — one workgroup per 64 particles.
//
// Reads SimParams from a uniform buffer at @group(0) @binding(0).
// Reads + writes Particles in a storage buffer at @group(0) @binding(1).
// Reads the silhouette edge list (CPU-extracted where the smoothed person
// mask crosses 0.5) at @group(0) @binding(2). The edge buffer is allocated at
// full MAX_EDGE_POINTS capacity and only the first edge_count entries are
// live, so `% edge_count` indexing never leaves the allocation.
//
// Life cycle: a particle is DEAD when age >= lifespan (a zeroed buffer is all
// dead). Each frame a dead particle rolls a hash against emission_prob; on a
// win it respawns at a hashed edge point, offset along the outward normal,
// with initial velocity = normal * (spawn_speed + burst_speed). Alive
// particles advance under buoyancy + limb impulses + drag, then are advected
// along a divergence-free curl-noise flow. There is no OOB teleport — a
// particle that drifts off-screen simply dies at end of life and respawns on
// the silhouette.
//
// CPU parity: RadianceSimParamsGpu / RadianceParticle / RadianceImpulse in
// crates/wc-sketches/src/radiance/compute/sim_params.rs mirror these structs
// field for field (offset_of! tests lock them); the mask-UV -> world mapping
// below must stay term-for-term identical to
// systems::sim_params::mask_uv_to_world.

struct Particle {
    position: vec2<f32>,
    velocity: vec2<f32>,
    age: f32,
    lifespan: f32,
    seed: f32,
    _pad: f32,
};

// Plan B contract shape: mask-UV position (0..1, y down) + outward unit
// normal in the same space.
struct EdgePoint {
    pos: vec2<f32>,
    normal: vec2<f32>,
};

struct Impulse {
    position: vec2<f32>,
    velocity: vec2<f32>,
    radius: f32,
    gain: f32,
    _pad: vec2<f32>,
};

const MAX_IMPULSES: u32 = 8u;

struct SimParams {
    dt: f32,
    time: f32,
    emission_prob: f32,
    edge_count: u32,
    particle_count: u32,
    spawn_offset: f32,
    spawn_speed: f32,
    burst_speed: f32,
    buoyancy: f32,
    flow_strength: f32,
    curl_scale: f32,
    curl_octaves: u32,
    drag_baked: f32,
    lifespan_min: f32,
    lifespan_max: f32,
    mirror: u32,
    uv_to_world: vec2<f32>,
    impulse_count: u32,
    _pad0: f32,
    impulses: array<Impulse, MAX_IMPULSES>,
};

@group(0) @binding(0) var<uniform> params: SimParams;
@group(0) @binding(1) var<storage, read_write> particles: array<Particle>;
@group(0) @binding(2) var<storage, read> edges: array<EdgePoint>;

// How strongly a particle inside an impulse radius couples to the limb
// velocity, per second. 6.0 means a particle sitting on a limb reaches ~the
// limb's velocity within a couple of frames without hard-snapping to it.
const IMPULSE_COUPLING: f32 = 6.0;

// PCG-style integer hash (Jarzynski & Olano) — cheap, well-distributed.
fn pcg(v: u32) -> u32 {
    var state = v * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

fn hash2(a: u32, b: u32) -> u32 {
    return pcg(a ^ pcg(b));
}

fn rand01(h: u32) -> f32 {
    // Top 24 bits -> [0, 1). Keeps full float precision.
    return f32(h >> 8u) * (1.0 / 16777216.0);
}

// Divergence-free curl-noise flow at a world-space point: the 2D curl of a
// scalar stream function psi built from sine octaves. Curl of a scalar field
// has zero divergence by construction, so the flow swirls without sources or
// sinks (the shared particle engine's turbulence, generalized to a
// param-driven 1..3 octave sum). Per octave the frequency doubles and the
// weight halves; each octave drifts along its own incommensurate direction so
// the field never visibly loops.
fn curl_flow(pos: vec2<f32>, scale: f32, t: f32, octaves: u32) -> vec2<f32> {
    // Per-octave time-drift directions (incommensurate, matching the shared
    // engine's 0.13/0.11 family).
    var drifts = array<vec2<f32>, 3>(
        vec2<f32>(0.13, -0.11),
        vec2<f32>(-0.17, 0.15),
        vec2<f32>(0.07, 0.19),
    );
    var flow = vec2<f32>(0.0);
    var freq = 1.0;
    var amp = 1.0;
    var total = 0.0;
    let n = clamp(octaves, 1u, 3u);
    for (var i = 0u; i < n; i = i + 1u) {
        let a = pos.x * scale * freq + drifts[i].x * t;
        let b = pos.y * scale * freq + drifts[i].y * t;
        // psi = sin(a)cos(b); curl = (d psi/dy, -d psi/dx). The chain-rule
        // scale*freq factor is folded into flow_strength by the caller.
        let dpsi_dx = cos(a) * cos(b);
        let dpsi_dy = -sin(a) * sin(b);
        flow = flow + vec2<f32>(dpsi_dy, -dpsi_dx) * amp;
        total = total + amp;
        freq = freq * 2.0;
        amp = amp * 0.5;
    }
    return flow / max(total, 1e-4);
}

// Mask-UV (0..1, y down) -> world px (origin center, y up), with the mirror
// flip. MUST stay identical to systems::sim_params::mask_uv_to_world.
fn mask_uv_to_world(uv: vec2<f32>) -> vec2<f32> {
    var u = uv.x;
    if (params.mirror == 1u) {
        u = 1.0 - u;
    }
    return vec2<f32>(
        (u - 0.5) * params.uv_to_world.x,
        (0.5 - uv.y) * params.uv_to_world.y,
    );
}

// Mask-UV direction -> world direction (mirror sign on x, y flip), normalized.
fn mask_dir_to_world(dir: vec2<f32>) -> vec2<f32> {
    var sx = params.uv_to_world.x;
    if (params.mirror == 1u) {
        sx = -sx;
    }
    let d = vec2<f32>(dir.x * sx, -dir.y * params.uv_to_world.y);
    let len = length(d);
    if (len < 1e-6) {
        return vec2<f32>(0.0, 1.0);
    }
    return d / len;
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let idx = id.x;
    let count = min(arrayLength(&particles), params.particle_count);
    if (idx >= count) {
        return;
    }
    var p = particles[idx];

    // --- Dead: roll for an edge respawn --------------------------------
    if (p.age >= p.lifespan) {
        if (params.edge_count == 0u) {
            return; // no silhouette this frame; stay dead, write nothing
        }
        // Salt the roll with the frame index so a losing particle re-rolls
        // fresh every frame (emission_prob is already rate*dt baked).
        let frame = u32(params.time * 60.0);
        if (rand01(hash2(idx, frame)) >= params.emission_prob) {
            return;
        }
        let e = edges[hash2(idx * 2654435769u, frame) % params.edge_count];
        let n = mask_dir_to_world(e.normal);
        p.position = mask_uv_to_world(e.pos) + n * params.spawn_offset;
        p.velocity = n * (params.spawn_speed + params.burst_speed);
        p.age = 0.0;
        p.lifespan = mix(
            params.lifespan_min,
            params.lifespan_max,
            rand01(hash2(idx, frame ^ 2654435769u)),
        );
        p.seed = rand01(hash2(idx, 2246822519u));
        particles[idx] = p;
        return;
    }

    // --- Alive: forces -> drag -> integrate -> curl advection ----------
    p.age = p.age + params.dt;

    // Buoyancy: constant upward acceleration (world +Y is up).
    var accel = vec2<f32>(0.0, params.buoyancy);

    // Limb impulses: locally-weighted coupling toward each limb's velocity,
    // fading to zero by the slot radius — a fast limb sheds a burst.
    let live_impulses = min(params.impulse_count, MAX_IMPULSES);
    for (var i = 0u; i < live_impulses; i = i + 1u) {
        let imp = params.impulses[i];
        if (imp.gain <= 0.0) {
            continue;
        }
        let dist = length(p.position - imp.position);
        let w = 1.0 - smoothstep(0.0, max(imp.radius, 1.0), dist);
        accel = accel + imp.velocity * (imp.gain * w * IMPULSE_COUPLING);
    }

    p.velocity = p.velocity + accel * params.dt;
    // Framerate-independent drag, baked CPU-side as pow(retention, dt).
    p.velocity = p.velocity * params.drag_baked;
    p.position = p.position + p.velocity * params.dt;

    // Curl advection: position (not force) so the drift speed is exactly
    // flow_strength px/s regardless of the drag regime, and the
    // divergence-free field can never collapse the aura inward.
    if (params.flow_strength > 0.0) {
        let turb = curl_flow(p.position, params.curl_scale, params.time, params.curl_octaves);
        p.position = p.position + turb * params.flow_strength * params.dt;
    }

    particles[idx] = p;
}
```

- [ ] **Step 2: Validate**

Run: `cargo xtask validate-shaders`
Expected: PASS — `simulate.wgsl` is self-contained (no `#import`), so naga parses + validates it. (The Task 7/8 shaders use `#import` and are runtime-validated only.)

- [ ] **Step 3: Cross-check the layout against Task 3's offsets**

Manual review step: confirm the WGSL `SimParams` field order matches the `offset_of!` test list one-for-one (dt, time, emission_prob, edge_count, particle_count, spawn_offset, spawn_speed, burst_speed, buoyancy, flow_strength, curl_scale, curl_octaves, drag_baked, lifespan_min, lifespan_max, mirror, uv_to_world, impulse_count, _pad0, impulses). WGSL uniform rules place `uv_to_world` (align 8) at 64 and the struct array (align 16, stride 32) at 80 — exactly the Rust `repr(C)` offsets.

- [ ] **Step 4: Commit**

```bash
git add assets/shaders/radiance/simulate.wgsl
git commit -m "feat(radiance): edge-respawn curl-noise simulation kernel" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: Mask-UV mapping, audio drive, and the single sim baker

**Files:**
- Create: `crates/wc-sketches/src/radiance/systems/mod.rs`
- Create: `crates/wc-sketches/src/radiance/systems/sim_params.rs`
- Modify: `crates/wc-sketches/src/radiance/mod.rs` (add `pub mod systems;`)

**Interfaces:**
- Consumes: pinned Plan A `wc_core::audio::input::AudioAnalysis { rms, gain, bands: [f32; 8], onset, beat_confidence, active }` (Copy); pinned Plan B `wc_core::input::body::{BodyTrackingState, SilhouetteEdges, MAX_EDGE_POINTS}` with landmark indices nose=0, wrists 15/16, hips 23/24, ankles 27/28; Task 3 PODs.
- Produces: `RadianceState`, `neutral_audio()`, `mask_uv_to_world`, `mask_dir_to_world`, `AudioDrive`, `audio_drive()`, `bake_radiance_sim()` (one baker — two writers: the live writer here, the screensaver writer in Task 12), `update_radiance_sim`, `freeze_radiance_emission`, `IMPULSE_LANDMARKS`.

- [ ] **Step 1: Write the failing tests** (they live in the same new file's `#[cfg(test)]` block — shown inline below; run order is file-at-once)

- [ ] **Step 2: Write `systems/mod.rs` and `systems/sim_params.rs` (complete)**

`crates/wc-sketches/src/radiance/systems/mod.rs`:

```rust
//! Main-world Radiance systems: spawn/teardown, the per-frame sim baker,
//! activity sync, camera arbitration, and dev/debug drivers.

pub mod sim_params;
```

(`spawn`, `arbitration`, `activity`, `debug` lines are added by their tasks.)

`crates/wc-sketches/src/radiance/systems/sim_params.rs`:

```rust
//! Per-frame Radiance simulation writer plus the idle freeze.
//!
//! Owns [`RadianceState`] (the smoothed audio-drive envelopes), the pure
//! mask-UV↔world mapping (CPU twin of the kernel's), the pure
//! [`audio_drive`] mapping, and the single [`bake_radiance_sim`] baker that
//! both the live writer ([`update_radiance_sim`]) and the screensaver
//! performer call — one baker, two writers, so the audio/impulse derivation
//! cannot drift between the live and attract paths (flame's Condition A1).
//!
//! Nothing here allocates: every value is stack math over `Copy` inputs, so
//! the per-frame path is heap-free per the multi-hour soak target.

use bevy::prelude::*;
use wc_core::audio::input::AudioAnalysis;
use wc_core::input::body::{BodyTrackingState, SilhouetteEdges, MAX_EDGE_POINTS};

use crate::radiance::compute::sim_params::{
    RadianceImpulse, RadianceSimParams, RadianceSimParamsGpu, MAX_IMPULSES,
};
use crate::radiance::settings::RadianceSettings;

/// MediaPipe pose landmark indices baked into impulse slots, per the pinned
/// cross-plan contract: nose, left/right wrist, left/right hip, left/right
/// ankle. Seven of the eight slots; the eighth is headroom.
pub const IMPULSE_LANDMARKS: [usize; 7] = [0, 15, 16, 23, 24, 27, 28];

/// Frame-time cap in seconds (matches the shared particle engine's 50 ms cap).
pub const DT_CAP: f32 = 0.05;
/// Per-dead-particle respawn attempts per second at `emission_rate == 1.0`
/// and neutral audio. The baker multiplies by the bass drive and `dt`.
pub const EMISSION_BASE_HZ: f32 = 2.5;
/// Onset envelope exponential release time constant, seconds.
pub const ONSET_DECAY_SECS: f32 = 0.18;
/// Onset envelope clamp (spectral flux is unbounded above).
pub const ONSET_MAX: f32 = 2.0;
/// Outward burst speed at full onset envelope, world px/s.
pub const BURST_SPEED: f32 = 260.0;
/// Spawn offset along the outward normal, world px.
pub const SPAWN_OFFSET: f32 = 4.0;
/// Baseline spawn speed along the outward normal, world px/s.
pub const SPAWN_SPEED: f32 = 70.0;
/// Particle lifespan range, seconds.
pub const LIFESPAN_MIN: f32 = 1.2;
/// See [`LIFESPAN_MIN`].
pub const LIFESPAN_MAX: f32 = 3.4;
/// Velocity fraction remaining after one second of drag.
pub const DRAG_PER_SECOND: f32 = 0.25;
/// Curl spatial frequency, radians per world px (~785 px swirl wavelength).
pub const CURL_SCALE: f32 = 0.008;
/// Limb impulse influence radius, world px.
pub const IMPULSE_RADIUS: f32 = 140.0;
/// Limb speed (world px/s) that maps to impulse gain 1.0.
pub const IMPULSE_FULL_SPEED: f32 = 900.0;
/// Smoothing time constant for the intensity/sparkle envelopes, seconds.
pub const ENVELOPE_SMOOTH_SECS: f32 = 0.25;

/// Smoothed audio-drive envelopes and the palette-shift accumulator; also
/// read by the material driver (Task 8). Rebuilt fresh on every sketch entry.
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct RadianceState {
    /// Onset burst envelope: instant attack, exponential release.
    pub onset_env: f32,
    /// Smoothed master intensity from RMS (`~0.55..1.5`); drives the
    /// particle-material brightness.
    pub intensity: f32,
    /// Smoothed high-band energy (`0..1`); drives sparkle flicker + fill
    /// shimmer.
    pub sparkle: f32,
    /// Gradient-shift accumulator in `0..1` (wraps); bass advances it.
    pub palette_shift: f32,
}

/// The neutral [`AudioAnalysis`] used when the resource is absent (headless
/// tests, feature-less harnesses) — the same values Plan A publishes when the
/// stream is inactive. Constructed literally because Plan A's type carries no
/// `Default` in the pinned contract.
#[must_use]
pub fn neutral_audio() -> AudioAnalysis {
    AudioAnalysis {
        rms: 0.0,
        gain: 1.0,
        bands: [0.0; 8],
        onset: 0.0,
        beat_confidence: 0.0,
        active: false,
    }
}

/// Mask-UV (0..1, y down) → world px (origin center, y up), with the mirror
/// flip. CPU twin of the kernel's `mask_uv_to_world` — the two must stay
/// term-for-term identical (world = ((u − 0.5)·sx, (0.5 − v)·sy)).
#[must_use]
pub fn mask_uv_to_world(uv: Vec2, scale: Vec2, mirror: bool) -> Vec2 {
    let u = if mirror { 1.0 - uv.x } else { uv.x };
    Vec2::new((u - 0.5) * scale.x, (0.5 - uv.y) * scale.y)
}

/// Mask-UV direction → world direction (mirror sign on x, y flip). NOT
/// normalized — impulse velocities keep their magnitude (UV/s × scale =
/// px/s); the kernel normalizes separately where it needs a unit normal.
#[must_use]
pub fn mask_dir_to_world(dir: Vec2, scale: Vec2, mirror: bool) -> Vec2 {
    let sx = if mirror { -scale.x } else { scale.x };
    Vec2::new(dir.x * sx, -dir.y * scale.y)
}

/// The audio→simulation coupling, as pure multipliers/values over one
/// [`AudioAnalysis`] frame (spec: bass→emission+buoyancy, highs→turbulence+
/// sparkle, onset→radial burst, slow RMS→master intensity).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioDrive {
    /// Multiplier on the emission pressure (bass).
    pub emission_mul: f32,
    /// Multiplier on buoyancy (bass pulse).
    pub buoyancy_mul: f32,
    /// Multiplier on curl flow strength (highs).
    pub turbulence_mul: f32,
    /// Sparkle target `0..1` (highs).
    pub sparkle: f32,
    /// Master intensity target (RMS-lifted brightness).
    pub intensity: f32,
    /// Raw onset strength this frame, sensitivity-scaled and clamped.
    pub onset: f32,
}

/// Map one analysis frame into drive values. Pure and allocation-free.
/// `sensitivity == 0.0` returns the exact neutral drive (all multipliers 1.0)
/// so audio coupling is provably inert at the knob's floor.
#[must_use]
pub fn audio_drive(audio: &AudioAnalysis, sensitivity: f32) -> AudioDrive {
    let s = sensitivity.max(0.0);
    // Low three bands = bass body; top three = air/sparkle.
    let bass = (audio.bands[0] + audio.bands[1] + audio.bands[2]) / 3.0;
    let highs = (audio.bands[5] + audio.bands[6] + audio.bands[7]) / 3.0;
    AudioDrive {
        emission_mul: 1.0 + 1.5 * bass * s,
        buoyancy_mul: 1.0 + 0.8 * bass * s,
        turbulence_mul: 1.0 + 1.2 * highs * s,
        sparkle: (highs * s).clamp(0.0, 1.0),
        intensity: 0.55 + 0.9 * audio.rms * s,
        onset: (audio.onset * s).clamp(0.0, ONSET_MAX),
    }
}

/// One baker, two writers (live + screensaver) — flame's Condition A1.
///
/// Advances the [`RadianceState`] envelopes (onset attack/release, smoothed
/// intensity/sparkle, palette shift), then writes every field of the kernel
/// uniform: audio-scaled emission/buoyancy/turbulence, the onset burst, the
/// mask-UV→world transform for the current window + mirror setting, and up to
/// [`MAX_IMPULSES`] limb impulse slots from the smoothed landmark velocities.
#[allow(
    clippy::cast_possible_truncation,
    clippy::as_conversions,
    reason = "edge/particle counts are bounded (MAX_EDGE_POINTS / the 300k \
              particle slider); usize -> u32 is exact in range"
)]
pub fn bake_radiance_sim(
    settings: &RadianceSettings,
    audio: &AudioAnalysis,
    body: Option<&BodyTrackingState>,
    edge_count: usize,
    window_size: Vec2,
    dt: f32,
    elapsed: f32,
    state: &mut RadianceState,
    out: &mut RadianceSimParamsGpu,
) {
    let dt = dt.min(DT_CAP);
    let drive = audio_drive(audio, settings.audio_sensitivity);

    // Onset envelope: instant attack to the incoming strength, exponential
    // release — so one drum hit reads as one burst, not a sustained gale.
    let released = state.onset_env * (-dt / ONSET_DECAY_SECS).exp();
    state.onset_env = released.max(drive.onset);
    // Smoothed intensity/sparkle (one-pole toward the drive targets).
    let k = 1.0 - (-dt / ENVELOPE_SMOOTH_SECS).exp();
    state.intensity += (drive.intensity - state.intensity) * k;
    state.sparkle += (drive.sparkle - state.sparkle) * k;
    // Palette drifts slowly, faster under bass (audio-shifted gradient).
    state.palette_shift =
        (state.palette_shift + dt * (0.02 + 0.10 * (drive.emission_mul - 1.0))).fract();

    out.dt = dt;
    out.time = elapsed;
    out.emission_prob =
        (settings.emission_rate * drive.emission_mul * EMISSION_BASE_HZ * dt).clamp(0.0, 1.0);
    out.edge_count = edge_count.min(MAX_EDGE_POINTS) as u32;
    out.spawn_offset = SPAWN_OFFSET;
    out.spawn_speed = SPAWN_SPEED * (0.6 + 0.4 * state.intensity);
    out.burst_speed = state.onset_env * BURST_SPEED;
    out.buoyancy = settings.buoyancy * drive.buoyancy_mul;
    out.flow_strength = settings.flow_strength * drive.turbulence_mul;
    out.curl_scale = CURL_SCALE;
    out.curl_octaves = settings.curl_octaves.clamp(1, 3);
    out.drag_baked = DRAG_PER_SECOND.powf(dt);
    out.lifespan_min = LIFESPAN_MIN;
    out.lifespan_max = LIFESPAN_MAX;
    out.mirror = u32::from(settings.mirror);
    // Full-window stretch: the 256² mask covers the window rect. (v1 default;
    // if dancer proportions read wrong on very wide displays, fit-height is
    // the follow-up tune.)
    out.uv_to_world = [window_size.x.max(1.0), window_size.y.max(1.0)];

    // Limb impulses from the smoothed landmark velocities.
    let scale = Vec2::new(out.uv_to_world[0], out.uv_to_world[1]);
    let mut n = 0usize;
    if let Some(body) = body {
        if body.present {
            for &lm in &IMPULSE_LANDMARKS {
                if n >= MAX_IMPULSES {
                    break;
                }
                let landmark = body.landmarks[lm];
                if landmark.visibility < 0.5 {
                    continue;
                }
                let vel = mask_dir_to_world(
                    Vec2::new(body.velocities[lm].x, body.velocities[lm].y),
                    scale,
                    settings.mirror,
                );
                let gain = (vel.length() / IMPULSE_FULL_SPEED).clamp(0.0, 1.0);
                if gain < 0.05 {
                    continue; // resting limbs shed nothing
                }
                let pos = mask_uv_to_world(
                    Vec2::new(landmark.pos.x, landmark.pos.y),
                    scale,
                    settings.mirror,
                );
                out.impulses[n] = RadianceImpulse {
                    position: pos.into(),
                    velocity: vel.into(),
                    radius: IMPULSE_RADIUS,
                    gain,
                    _pad: [0.0; 2],
                };
                n += 1;
            }
        }
    }
    // Zero stale slots past the live count so a limb dropping out of frame
    // cannot leave a ghost impulse.
    for slot in out.impulses.iter_mut().skip(n) {
        *slot = RadianceImpulse::default();
    }
    out.impulse_count = n as u32;
    // particle_count is owned by spawn (buffer size); the baker leaves it.
}

/// `Update` (gated `sketch_active(AppState::Radiance)`): the live writer.
/// Gathers the current analysis/body/edges resources (all optional — the
/// sketch degrades to motion-only or emission-only gracefully) and bakes.
pub fn update_radiance_sim(
    time: Res<'_, Time>,
    window: Single<'_, '_, &Window>,
    settings: Res<'_, RadianceSettings>,
    audio: Option<Res<'_, AudioAnalysis>>,
    body: Option<Res<'_, BodyTrackingState>>,
    edges: Option<Res<'_, SilhouetteEdges>>,
    mut state: ResMut<'_, RadianceState>,
    mut sim: ResMut<'_, RadianceSimParams>,
) {
    let audio_frame = audio.map_or_else(neutral_audio, |a| *a);
    let edge_count = edges.map_or(0, |e| e.points.len());
    let window_size = Vec2::new(window.width(), window.height());
    bake_radiance_sim(
        &settings,
        &audio_frame,
        body.as_deref(),
        edge_count,
        window_size,
        time.delta_secs(),
        time.elapsed_secs(),
        &mut state,
        &mut sim.params,
    );
}

/// `OnEnter(SketchActivity::Idle)` (gated `in_state(AppState::Radiance)`):
/// zero emission and the burst so the aura fades out over one lifespan while
/// the throttled last frames hold — flame's freeze idiom, adapted to a
/// particle field that must die out rather than stop mid-air.
pub fn freeze_radiance_emission(mut sim: ResMut<'_, RadianceSimParams>) {
    sim.params.emission_prob = 0.0;
    sim.params.burst_speed = 0.0;
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use wc_core::input::body::{BodyLandmark, BODY_LANDMARK_COUNT};

    fn fixture_audio(bands: [f32; 8], rms: f32, onset: f32) -> AudioAnalysis {
        AudioAnalysis {
            rms,
            gain: 1.0,
            bands,
            onset,
            beat_confidence: 0.0,
            active: true,
        }
    }

    fn fixture_body(wrist_vel: Vec3) -> BodyTrackingState {
        let mut landmarks = [BodyLandmark::default(); BODY_LANDMARK_COUNT];
        for lm in &mut landmarks {
            lm.visibility = 1.0;
            lm.pos = Vec3::new(0.5, 0.5, 0.0);
        }
        // Right wrist (16) moving.
        landmarks[16].pos = Vec3::new(0.7, 0.4, 0.0);
        let mut velocities = [Vec3::ZERO; BODY_LANDMARK_COUNT];
        velocities[16] = wrist_vel;
        BodyTrackingState {
            present: true,
            confidence: 0.9,
            landmarks,
            world_landmarks: [Vec3::ZERO; BODY_LANDMARK_COUNT],
            velocities,
            timestamp: std::time::Duration::from_millis(33),
        }
    }

    fn bake(
        settings: &RadianceSettings,
        audio: &AudioAnalysis,
        body: Option<&BodyTrackingState>,
        edge_count: usize,
    ) -> (RadianceState, RadianceSimParamsGpu) {
        let mut state = RadianceState::default();
        let mut out = RadianceSimParamsGpu::default();
        bake_radiance_sim(
            settings,
            audio,
            body,
            edge_count,
            Vec2::new(1920.0, 1080.0),
            1.0 / 60.0,
            10.0,
            &mut state,
            &mut out,
        );
        (state, out)
    }

    /// Mirror on: UV x flips around center; y flips down→up. Golden points.
    #[test]
    fn mask_uv_to_world_maps_and_mirrors() {
        let scale = Vec2::new(1920.0, 1080.0);
        // Center maps to origin either way.
        assert_eq!(
            mask_uv_to_world(Vec2::new(0.5, 0.5), scale, false),
            Vec2::ZERO
        );
        assert_eq!(
            mask_uv_to_world(Vec2::new(0.5, 0.5), scale, true),
            Vec2::ZERO
        );
        // UV (0,0) is the top-left of the mask -> left edge, top of screen.
        let tl = mask_uv_to_world(Vec2::new(0.0, 0.0), scale, false);
        assert_eq!(tl, Vec2::new(-960.0, 540.0));
        // Mirrored, the same UV lands on the RIGHT edge.
        let tl_m = mask_uv_to_world(Vec2::new(0.0, 0.0), scale, true);
        assert_eq!(tl_m, Vec2::new(960.0, 540.0));
        // Directions: mask +y (down) maps to world -y; mirror negates x.
        let d = mask_dir_to_world(Vec2::new(1.0, 1.0), scale, false);
        assert!(d.x > 0.0 && d.y < 0.0);
        let d_m = mask_dir_to_world(Vec2::new(1.0, 1.0), scale, true);
        assert!(d_m.x < 0.0 && d_m.y < 0.0);
    }

    /// Sensitivity 0 (or silent input) is the exact neutral drive: every
    /// multiplier 1.0, no burst — audio coupling provably inert.
    #[test]
    fn audio_drive_neutral_at_zero_sensitivity() {
        let loud = fixture_audio([1.0; 8], 1.0, 1.0);
        let d = audio_drive(&loud, 0.0);
        assert!((d.emission_mul - 1.0).abs() < f32::EPSILON);
        assert!((d.buoyancy_mul - 1.0).abs() < f32::EPSILON);
        assert!((d.turbulence_mul - 1.0).abs() < f32::EPSILON);
        assert!(d.sparkle.abs() < f32::EPSILON);
        assert!(d.onset.abs() < f32::EPSILON);
    }

    /// Bass raises emission + buoyancy; highs raise turbulence + sparkle.
    #[test]
    fn audio_drive_routes_bands_per_spec() {
        let bassy = fixture_audio([0.9, 0.9, 0.9, 0.0, 0.0, 0.0, 0.0, 0.0], 0.3, 0.0);
        let airy = fixture_audio([0.0, 0.0, 0.0, 0.0, 0.0, 0.9, 0.9, 0.9], 0.3, 0.0);
        let db = audio_drive(&bassy, 1.0);
        let da = audio_drive(&airy, 1.0);
        assert!(db.emission_mul > 1.5 && db.buoyancy_mul > 1.2);
        assert!((db.turbulence_mul - 1.0).abs() < 1e-6, "bass must not stir turbulence");
        assert!(da.turbulence_mul > 1.5 && da.sparkle > 0.5);
        assert!((da.emission_mul - 1.0).abs() < 1e-6, "highs must not pump emission");
    }

    /// The baker scales emission with the bass drive vs the neutral bake.
    #[test]
    fn bake_bass_raises_emission_prob() {
        let settings = RadianceSettings::default();
        let quiet = neutral_audio();
        let bassy = fixture_audio([0.9, 0.9, 0.9, 0.0, 0.0, 0.0, 0.0, 0.0], 0.4, 0.0);
        let (_, base) = bake(&settings, &quiet, None, 500);
        let (_, driven) = bake(&settings, &bassy, None, 500);
        assert!(driven.emission_prob > base.emission_prob);
        assert!(driven.buoyancy > base.buoyancy);
        // Expected neutral value: rate * 1.0 * EMISSION_BASE_HZ * dt.
        let expect = 0.5 * EMISSION_BASE_HZ / 60.0;
        assert!((base.emission_prob - expect).abs() < 1e-6);
    }

    /// Onset attacks instantly and releases exponentially across frames.
    #[test]
    fn onset_envelope_attacks_then_decays() {
        let settings = RadianceSettings::default();
        let mut state = RadianceState::default();
        let mut out = RadianceSimParamsGpu::default();
        let hit = fixture_audio([0.0; 8], 0.2, 1.5);
        let silence = neutral_audio();
        let win = Vec2::new(1920.0, 1080.0);
        bake_radiance_sim(&settings, &hit, None, 100, win, 1.0 / 60.0, 0.0, &mut state, &mut out);
        let peak = out.burst_speed;
        assert!(peak > 0.0, "onset must produce a burst");
        for _ in 0..30 {
            bake_radiance_sim(
                &settings, &silence, None, 100, win, 1.0 / 60.0, 0.0, &mut state, &mut out,
            );
        }
        assert!(
            out.burst_speed < peak * 0.1,
            "burst must decay: {} vs peak {peak}",
            out.burst_speed
        );
    }

    /// A fast right wrist produces exactly one impulse slot with a mirrored
    /// world position and a bounded gain; slots past it are zeroed.
    #[test]
    fn bake_bakes_wrist_impulse_with_mirror_mapping() {
        let settings = RadianceSettings::default(); // mirror = true
        let body = fixture_body(Vec3::new(0.8, 0.0, 0.0)); // fast +u sweep
        let (_, out) = bake(&settings, &neutral_audio(), Some(&body), 500);
        assert_eq!(out.impulse_count, 1, "one moving limb -> one slot");
        let imp = out.impulses[0];
        // Wrist at UV (0.7, 0.4), mirrored: world x = (1-0.7-0.5)*1920 = -384;
        // world y = (0.5-0.4)*1080 = 108.
        assert!((imp.position[0] - -384.0).abs() < 1e-3, "{:?}", imp.position);
        assert!((imp.position[1] - 108.0).abs() < 1e-3, "{:?}", imp.position);
        // Mirrored +u velocity points -x in world.
        assert!(imp.velocity[0] < 0.0);
        assert!(imp.gain > 0.0 && imp.gain <= 1.0);
        assert!((out.impulses[1].gain).abs() < f32::EPSILON, "stale slots zeroed");
    }

    /// Absent body / present-but-still body bakes zero impulses.
    #[test]
    fn bake_no_body_means_no_impulses() {
        let settings = RadianceSettings::default();
        let (_, out) = bake(&settings, &neutral_audio(), None, 500);
        assert_eq!(out.impulse_count, 0);
        let still = fixture_body(Vec3::ZERO);
        let (_, out) = bake(&settings, &neutral_audio(), Some(&still), 500);
        assert_eq!(out.impulse_count, 0, "resting limbs shed nothing");
    }

    /// Edge count clamps to the contract capacity.
    #[test]
    fn bake_clamps_edge_count() {
        let settings = RadianceSettings::default();
        let (_, out) = bake(&settings, &neutral_audio(), None, MAX_EDGE_POINTS * 4);
        assert_eq!(out.edge_count, u32::try_from(MAX_EDGE_POINTS).expect("fits"));
    }

    /// The freeze hook zeroes emission and burst, nothing else.
    #[test]
    fn freeze_zeroes_emission() {
        let mut world = World::new();
        let settings = RadianceSettings::default();
        let (_, params) = bake(&settings, &neutral_audio(), None, 500);
        world.insert_resource(RadianceSimParams {
            params,
            particles: Handle::default(),
            particle_count: 1000,
        });
        bevy::ecs::system::RunSystemOnce::run_system_once(
            &mut world,
            freeze_radiance_emission,
        )
        .expect("freeze runs");
        let sim = world.resource::<RadianceSimParams>();
        assert!(sim.params.emission_prob.abs() < f32::EPSILON);
        assert!(sim.params.burst_speed.abs() < f32::EPSILON);
        assert!(sim.params.flow_strength > 0.0, "flow untouched (fade-out drifts)");
    }
}
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo nextest run -p wc-sketches radiance::systems::sim_params`
Expected: PASS (9 tests). (Before writing the impl the module didn't exist — the "fails first" state is the missing module; the assertions encode the contract.)

- [ ] **Step 4: Commit**

```bash
git add crates/wc-sketches/src/radiance
git commit -m "feat(radiance): audio drive, mask-UV mapping, and the single sim baker" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: Render-world compute pipeline + silhouette-edge upload

**Files:**
- Create: `crates/wc-sketches/src/radiance/compute/pipeline.rs`
- Create: `crates/wc-sketches/src/radiance/compute/edge_upload.rs`
- Modify: `crates/wc-sketches/src/radiance/compute/mod.rs` (add module lines)
- Modify: `crates/wc-sketches/src/lib.rs` (register `RadianceComputePlugin` in `SketchesPlugin::build`, next to `FlameComputePlugin`)

**Interfaces:**
- Consumes: Task 3 `RadianceSimParams`/`RadianceSimParamsGpu`; Task 4 shader; pinned `wc_core::input::body::{SilhouetteEdges, EdgePoint, MAX_EDGE_POINTS}`; the flame/particles pipeline idiom (persistent buffers, `BufferId`-keyed single-slot bind-group cache, `remove_*_if_absent`, dispatch in `RenderGraph` before `camera_driver`).
- Produces: `RadianceComputePlugin`, `RadiancePipeline` (with the pipeline-owned persistent `edges_buffer`), `ExtractedEdges`.

- [ ] **Step 1: Write `compute/edge_upload.rs` (complete)**

```rust
//! Silhouette edge list → GPU storage buffer, keyed on generation.
//!
//! `SilhouetteEdges` (main world, Plan B) is refilled in place on each body
//! frame and bumps `generation`. Extracting the `Vec` every render frame
//! would clone ~32 KB per frame (a hot-path allocation) and re-uploading it
//! through a `ShaderBuffer` asset would recreate the GPU buffer — churning
//! the bind-group cache's `BufferId` key ~30 times a second. Instead:
//!
//! 1. [`extract_silhouette_edges`] (`ExtractSchedule`) copies the points into
//!    a render-world scratch (`ExtractedEdges`, capacity `MAX_EDGE_POINTS`,
//!    refilled with `clear()` — zero steady-state allocation) ONLY when
//!    `generation` changed.
//! 2. [`upload_silhouette_edges`] (`RenderSystems::PrepareBindGroups`, before
//!    the bind-group prepare) `write_buffer`s the scratch into the persistent
//!    `edges_buffer` on [`super::pipeline::RadiancePipeline`] — a staged
//!    copy, no allocation, stable `BufferId`.
//!
//! The kernel indexes `% edge_count` into the full-capacity buffer, so a
//! frame where the count shrinks can never read past the live prefix's
//! allocation.

use bevy::prelude::*;
use bevy::render::renderer::RenderQueue;
use bevy::render::Extract;
use wc_core::input::body::{EdgePoint, SilhouetteEdges, MAX_EDGE_POINTS};

use super::pipeline::RadiancePipeline;

/// Render-world scratch copy of the newest silhouette edge list.
#[derive(Resource)]
pub struct ExtractedEdges {
    /// Generation of the copy currently held (and, once uploaded, of the GPU
    /// buffer). `u64::MAX` = "never copied" sentinel, so the first real
    /// generation (whatever Plan B starts at) always triggers a copy.
    pub generation: u64,
    /// Point scratch; capacity `MAX_EDGE_POINTS`, refilled with `clear()`.
    pub points: Vec<EdgePoint>,
    /// A fresh copy is waiting for [`upload_silhouette_edges`].
    pub dirty: bool,
}

impl Default for ExtractedEdges {
    fn default() -> Self {
        Self {
            generation: u64::MAX,
            points: Vec::with_capacity(MAX_EDGE_POINTS),
            dirty: false,
        }
    }
}

/// `ExtractSchedule`: copy the main-world edge list when (and only when) its
/// generation changed. No-ops in one compare in the steady state between
/// body frames.
pub fn extract_silhouette_edges(
    main: Extract<'_, '_, Option<Res<'_, SilhouetteEdges>>>,
    mut extracted: ResMut<'_, ExtractedEdges>,
) {
    let Some(src) = main.as_ref() else {
        return;
    };
    if src.generation == extracted.generation {
        return;
    }
    extracted.points.clear();
    // The contract caps the source at MAX_EDGE_POINTS; truncate defensively
    // so the scratch (and the fixed GPU buffer) can never overflow.
    let take = src.points.len().min(MAX_EDGE_POINTS);
    extracted.points.extend_from_slice(&src.points[..take]);
    extracted.generation = src.generation;
    extracted.dirty = true;
}

/// `Render` (`PrepareBindGroups`, ordered before the bind-group prepare):
/// stage the fresh copy into the persistent edge buffer.
pub fn upload_silhouette_edges(
    pipeline: Option<Res<'_, RadiancePipeline>>,
    render_queue: Res<'_, RenderQueue>,
    mut extracted: ResMut<'_, ExtractedEdges>,
) {
    let Some(pipeline) = pipeline else {
        return;
    };
    if !extracted.dirty {
        return;
    }
    if !extracted.points.is_empty() {
        render_queue.0.write_buffer(
            &pipeline.edges_buffer,
            0,
            bytemuck::cast_slice(&extracted.points),
        );
    }
    extracted.dirty = false;
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;

    /// The scratch starts at the never-copied sentinel with full capacity and
    /// nothing pending — so the first real generation always copies, and the
    /// steady state never allocates.
    #[test]
    fn extracted_edges_default_is_clean_sentinel() {
        let e = ExtractedEdges::default();
        assert_eq!(e.generation, u64::MAX);
        assert!(e.points.is_empty());
        assert!(e.points.capacity() >= MAX_EDGE_POINTS);
        assert!(!e.dirty);
    }
}
```

- [ ] **Step 2: Write `compute/pipeline.rs` (complete)**

```rust
//! Render-world compute plugin for the Radiance aura.
//!
//! # Signal / data flow
//!
//! 1. `ExtractResourcePlugin` clones [`RadianceSimParams`] (POD + one
//!    `Handle`, memcpy clone) from the main world each frame;
//!    [`remove_radiance_sim_params_if_absent`] mirrors removals the plugin
//!    does not propagate (the established landmine — see
//!    `particles/compute.rs`).
//! 2. [`extract_silhouette_edges`] copies the edge list generation-gated
//!    (see `edge_upload`).
//! 3. `init_radiance_pipeline` (`RenderStartup`) builds the bind-group
//!    layout, queues the compute pipeline, and allocates the persistent
//!    uniform buffer (336 B `SimParams`) and the persistent edge storage
//!    buffer (`MAX_EDGE_POINTS` × 16 B) once — never per frame.
//! 4. `prepare_radiance_bind_group` (`PrepareBindGroups`, after the edge
//!    upload) writes this frame's uniforms and builds (or reuses) the single
//!    bind group, cached keyed on the particle buffer's [`BufferId`]
//!    (bounded by construction: one slot, replaced on change so no stale
//!    buffer is retained across sketch re-entry).
//! 5. `radiance_compute` dispatches `ceil(particle_count / 64)` workgroups in
//!    the root `RenderGraph` schedule before `camera_driver`, so the buffer
//!    is current before the 2D pass draws it. Dispatch scales with the
//!    particle-count setting; no unused workgroups.

#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "u32/u64/usize casts for GPU buffer sizes are intentional and \
              bounds-checked (MAX_EDGE_POINTS and the 300k particle cap)"
)]

use std::borrow::Cow;
use std::num::NonZeroU64;

use bevy::core_pipeline::schedule::camera_driver;
use bevy::prelude::*;
use bevy::render::extract_resource::ExtractResourcePlugin;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{
    BindGroup, BindGroupEntry, BindGroupLayout, BindGroupLayoutDescriptor, BindGroupLayoutEntry,
    BindingType, Buffer, BufferBindingType, BufferDescriptor, BufferId, BufferUsages,
    CachedComputePipelineId, ComputePassDescriptor, ComputePipelineDescriptor, PipelineCache,
    ShaderStages,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderGraph, RenderQueue};
use bevy::render::storage::GpuShaderBuffer;
use bevy::render::{Extract, ExtractSchedule, Render, RenderApp, RenderStartup, RenderSystems};
use wc_core::input::body::{EdgePoint, MAX_EDGE_POINTS};

use super::edge_upload::{extract_silhouette_edges, upload_silhouette_edges, ExtractedEdges};
use super::sim_params::{RadianceSimParams, RadianceSimParamsGpu};

/// Workgroup width; must match `@workgroup_size(64)` in
/// `assets/shaders/radiance/simulate.wgsl`.
const WORKGROUP_SIZE: u32 = 64;

/// `RadianceSimParamsGpu` byte size (336) for binding 0's `min_binding_size`.
/// The `panic!` is inside a `const`, so a zero-sized regression fails at
/// compile time.
const SIM_PARAMS_SIZE: NonZeroU64 =
    match NonZeroU64::new(std::mem::size_of::<RadianceSimParamsGpu>() as u64) {
        Some(n) => n,
        None => panic!("RadianceSimParamsGpu must be non-zero-sized"),
    };

/// Full-capacity edge buffer size in bytes (`MAX_EDGE_POINTS` × 16).
const EDGES_BUFFER_SIZE: u64 =
    (MAX_EDGE_POINTS * std::mem::size_of::<EdgePoint>()) as u64;

/// Registers extraction (+ removal companion), the edge upload, pipeline
/// init, per-frame prepare, and the dispatch for the Radiance aura.
///
/// `Plugin` singleton — added exactly once by `SketchesPlugin`. Inert until
/// the sketch inserts [`RadianceSimParams`] on entry, so it costs nothing on
/// other sketches.
pub struct RadianceComputePlugin;

impl Plugin for RadianceComputePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ExtractResourcePlugin::<RadianceSimParams>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app.init_resource::<ExtractedEdges>();
        render_app.add_systems(
            ExtractSchedule,
            (remove_radiance_sim_params_if_absent, extract_silhouette_edges),
        );

        render_app
            .add_systems(RenderStartup, init_radiance_pipeline)
            .add_systems(
                Render,
                (
                    upload_silhouette_edges,
                    prepare_radiance_bind_group.run_if(resource_exists::<RadianceSimParams>),
                )
                    .chain()
                    .in_set(RenderSystems::PrepareBindGroups),
            );

        // Dispatch before camera_driver so the 2D pass reads updated
        // particles (Bevy 0.19 systems-based render graph).
        render_app.add_systems(RenderGraph, radiance_compute.before(camera_driver));
    }
}

/// Cached compute pipeline state. Initialised once in `RenderStartup`.
#[derive(Resource)]
pub struct RadiancePipeline {
    /// Retained so the prepare system can fetch the [`BindGroupLayout`] from
    /// the [`PipelineCache`] without storing it twice.
    bind_group_layout_descriptor: BindGroupLayoutDescriptor,
    /// Handle into Bevy's [`PipelineCache`].
    pipeline_id: CachedComputePipelineId,
    /// Persistent `UNIFORM | COPY_DST` buffer for the 336-byte sim params;
    /// refilled each frame via `write_buffer` (no realloc).
    sim_params_buffer: Buffer,
    /// Persistent `STORAGE | COPY_DST` buffer of `MAX_EDGE_POINTS` edge
    /// points; refilled generation-gated by `edge_upload` (stable
    /// `BufferId`, so it never churns the bind-group cache).
    pub edges_buffer: Buffer,
}

/// Per-frame bind group + dispatch size, consumed by [`radiance_compute`].
#[derive(Resource)]
pub struct RadianceComputeBindGroup {
    /// sim uniform (0), particle storage rw (1), edge storage ro (2).
    bind_group: BindGroup,
    /// `ceil(particle_count / WORKGROUP_SIZE)`.
    dispatch_size: u32,
}

/// Initialises [`RadiancePipeline`] in the render-world startup schedule.
fn init_radiance_pipeline(
    mut commands: Commands<'_, '_>,
    asset_server: Res<'_, AssetServer>,
    pipeline_cache: Res<'_, PipelineCache>,
    render_device: Res<'_, RenderDevice>,
) {
    let bind_group_layout_descriptor = BindGroupLayoutDescriptor::new(
        "radiance_compute_bgl",
        &[
            // binding 0 — SimParams uniform.
            BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::COMPUTE,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: Some(SIM_PARAMS_SIZE),
                },
                count: None,
            },
            // binding 1 — Particle storage, read_write.
            BindGroupLayoutEntry {
                binding: 1,
                visibility: ShaderStages::COMPUTE,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            // binding 2 — EdgePoint storage, read-only.
            BindGroupLayoutEntry {
                binding: 2,
                visibility: ShaderStages::COMPUTE,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    );

    let shader = asset_server.load::<bevy::shader::Shader>("shaders/radiance/simulate.wgsl");

    let pipeline_id = pipeline_cache.queue_compute_pipeline(ComputePipelineDescriptor {
        label: Some(Cow::from("radiance_compute_pipeline")),
        layout: vec![bind_group_layout_descriptor.clone()],
        shader,
        entry_point: Some(Cow::from("main")),
        ..default()
    });

    // Both persistent buffers allocated once; refilled via write_buffer.
    let sim_params_buffer = render_device.create_buffer(&BufferDescriptor {
        label: Some("radiance_sim_params_uniform"),
        size: std::mem::size_of::<RadianceSimParamsGpu>() as u64,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let edges_buffer = render_device.create_buffer(&BufferDescriptor {
        label: Some("radiance_silhouette_edges"),
        size: EDGES_BUFFER_SIZE,
        usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    commands.insert_resource(RadiancePipeline {
        bind_group_layout_descriptor,
        pipeline_id,
        sim_params_buffer,
        edges_buffer,
    });
}

/// Uploads this frame's uniforms and builds (or reuses) the compute bind
/// group.
///
/// ## Bind-group caching (always-on compute hot path)
///
/// The sim uniform and edge buffers are pipeline-owned and live for the
/// process; the particle storage buffer is recreated per sketch entry, so
/// the cache keys on its [`BufferId`] and replaces its single slot on change
/// (dropping the old bind group releases the freed buffer's reference —
/// bounded by construction, no stale retention across re-entry).
fn prepare_radiance_bind_group(
    mut commands: Commands<'_, '_>,
    render_device: Res<'_, RenderDevice>,
    render_queue: Res<'_, RenderQueue>,
    pipeline_cache: Res<'_, PipelineCache>,
    sim: Res<'_, RadianceSimParams>,
    buffers: Res<'_, RenderAssets<GpuShaderBuffer>>,
    pipeline: Option<Res<'_, RadiancePipeline>>,
    mut cached: Local<'_, Option<(BufferId, BindGroup)>>,
) {
    let Some(pipeline) = pipeline else {
        return;
    };
    let Some(particle_buffer) = buffers.get(&sim.particles) else {
        return;
    };

    // Staged copy — no allocation after init.
    render_queue.0.write_buffer(
        &pipeline.sim_params_buffer,
        0,
        bytemuck::bytes_of(&sim.params),
    );

    let buffer_id = particle_buffer.buffer.id();
    let bind_group = match &*cached {
        Some((id, bg)) if *id == buffer_id => bg.clone(),
        _ => {
            let layout: BindGroupLayout =
                pipeline_cache.get_bind_group_layout(&pipeline.bind_group_layout_descriptor);
            let bg = render_device.create_bind_group(
                "radiance_compute_bind_group",
                &layout,
                &[
                    BindGroupEntry {
                        binding: 0,
                        resource: pipeline.sim_params_buffer.as_entire_binding(),
                    },
                    BindGroupEntry {
                        binding: 1,
                        resource: particle_buffer.buffer.as_entire_binding(),
                    },
                    BindGroupEntry {
                        binding: 2,
                        resource: pipeline.edges_buffer.as_entire_binding(),
                    },
                ],
            );
            *cached = Some((buffer_id, bg.clone()));
            bg
        }
    };

    let dispatch_size = sim.particle_count.div_ceil(WORKGROUP_SIZE);
    commands.insert_resource(RadianceComputeBindGroup {
        bind_group,
        dispatch_size,
    });
}

/// Render system dispatching the aura kernel each frame.
///
/// Gates directly on [`RadianceSimParams`] (mirroring `particle_compute`):
/// the lingering [`RadianceComputeBindGroup`] is never removed, so this
/// `Option` guard — together with [`remove_radiance_sim_params_if_absent`] —
/// is what actually stops the dispatch after `OnExit`.
fn radiance_compute(
    bind_group: Option<Res<'_, RadianceComputeBindGroup>>,
    pipeline_res: Option<Res<'_, RadiancePipeline>>,
    sim: Option<Res<'_, RadianceSimParams>>,
    pipeline_cache: Res<'_, PipelineCache>,
    mut render_context: RenderContext<'_, '_>,
) {
    if sim.is_none() {
        return;
    }
    let Some(bg) = bind_group else {
        return;
    };
    let Some(pipeline_res) = pipeline_res else {
        return;
    };
    let Some(compute_pipeline) = pipeline_cache.get_compute_pipeline(pipeline_res.pipeline_id)
    else {
        return;
    };

    let mut pass = render_context
        .command_encoder()
        .begin_compute_pass(&ComputePassDescriptor {
            label: Some("radiance_compute_pass"),
            timestamp_writes: None,
        });
    pass.set_pipeline(compute_pipeline);
    pass.set_bind_group(0, &bg.bind_group, &[]);
    pass.dispatch_workgroups(bg.dispatch_size, 1, 1);
}

/// Removes [`RadianceSimParams`] from the render world when the main-world
/// source is absent (`ExtractResourcePlugin` does not propagate removals —
/// the established landmine; mirrors `remove_particle_sim_params_if_absent`).
fn remove_radiance_sim_params_if_absent(
    mut commands: Commands<'_, '_>,
    main_resource: Extract<'_, '_, Option<Res<'_, RadianceSimParams>>>,
    render_resource: Option<Res<'_, RadianceSimParams>>,
) {
    if main_resource.is_none() && render_resource.is_some() {
        commands.remove_resource::<RadianceSimParams>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build-smoke: the plugin adds cleanly under `MinimalPlugins` (no
    /// `RenderApp`) without panicking — `build` early-returns, mirroring
    /// `flame_compute_plugin_builds`.
    #[test]
    fn radiance_compute_plugin_builds() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(RadianceComputePlugin);
        app.update();
    }

    /// Binding 0's `min_binding_size` is the exact 336-byte layout, and the
    /// edge buffer holds the full contract capacity.
    #[test]
    fn buffer_size_constants_match_contracts() {
        assert_eq!(SIM_PARAMS_SIZE.get(), 336);
        assert_eq!(
            EDGES_BUFFER_SIZE,
            (MAX_EDGE_POINTS as u64) * 16,
            "EdgePoint stride is 16 bytes by the pinned contract"
        );
    }

    /// Dispatch math rounds up so the last partial workgroup still launches.
    #[test]
    fn dispatch_workgroups_round_up() {
        assert_eq!(63_u32.div_ceil(WORKGROUP_SIZE), 1);
        assert_eq!(64_u32.div_ceil(WORKGROUP_SIZE), 1);
        assert_eq!(65_u32.div_ceil(WORKGROUP_SIZE), 2);
    }
}
```

Add to `compute/mod.rs`:

```rust
pub mod edge_upload;
pub mod pipeline;
```

In `crates/wc-sketches/src/lib.rs`, register next to the flame compute plugin:

```rust
        // Radiance edge-respawn compute node, registered once (a Plugin
        // singleton). Inert until the Radiance sketch inserts
        // RadianceSimParams on entry.
        app.add_plugins(crate::radiance::compute::pipeline::RadianceComputePlugin);
```

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p wc-sketches radiance::compute`
Expected: PASS (POD tests + the three pipeline tests + the edge-upload default test).

- [ ] **Step 4: Commit**

```bash
git add crates/wc-sketches/src/radiance crates/wc-sketches/src/lib.rs
git commit -m "feat(radiance): render-world compute pipeline and generation-keyed edge upload" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: Additive billboard material + `render.wgsl`

**Files:**
- Create: `assets/shaders/radiance/render.wgsl`
- Create: `crates/wc-sketches/src/radiance/render.rs` (material half; the silhouette half and the driver arrive in Task 8)
- Modify: `crates/wc-sketches/src/radiance/mod.rs` (add `pub mod render;`)
- Modify: `crates/wc-sketches/src/lib.rs` (register `Material2dPlugin::<RadianceMaterial>`)

**Interfaces:**
- Consumes: Task 3 particle layout; flame's additive `(One, One)` specialize recipe; `bevy_sprite::mesh2d_view_bindings::view` for the projection.
- Produces: `RadianceMaterial` (consumed by Task 9's spawn and Task 8's driver).

**Why a radiance-specific render shader (decision, per the plan brief):** the shared `ParticleMaterial`/`particles/render.wgsl` path deliberately uses standard alpha blending (its glow is Line's gravity-smear post-process) and a 48-byte particle with home/attract semantics Radiance doesn't have. Radiance needs flame's pure-additive `(One, One)` HDR accumulation, a 32-byte particle whose alpha is *derived* from age/lifespan, and a curated-gradient palette — none expressible through `ParticleMaterial`'s uniforms without perturbing Line/Dots. So Radiance reuses the billboard *technique* (vertex-index quad expansion, zero-area collapse for dead particles, `@interpolate(flat)` for opaque per-particle scalars) with its own material + shader, and drops the sprite texture for a procedural soft disc (one fewer asset, no sampler).

- [ ] **Step 1: Write the shader (complete)**

```wgsl
// Radiance aura render — one additive soft-disc billboard per particle,
// driven by vertex_index (6 vertices per particle; the mesh's own vertex
// data is unused). Additive (One, One) blending is set by
// RadianceMaterial::specialize (flame's recipe): overlapping discs
// accumulate into luminous HDR cores and the global bloom + tonemap supply
// the radiance. No post-process pass.
//
// Bindings (Bevy Material2d convention, group 2):
//   @binding(0): particle storage buffer (read-only)
//   @binding(1): params_a — x master intensity (HDR), y quad half-size px,
//                z palette shift 0..1, w sparkle 0..1
//   @binding(2..4): gradient stops a, b, c (linear HDR)
//   @binding(5): params_b — x elapsed seconds, y/z/w reserved
//
// Struct parity: Particle mirrors RadianceParticle (offset_of! tested).

#import bevy_sprite::mesh2d_view_bindings::view

struct Particle {
    position: vec2<f32>,
    velocity: vec2<f32>,
    age: f32,
    lifespan: f32,
    seed: f32,
    _pad: f32,
};

@group(2) @binding(0) var<storage, read> particles: array<Particle>;
@group(2) @binding(1) var<uniform> params_a: vec4<f32>;
@group(2) @binding(2) var<uniform> color_a: vec4<f32>;
@group(2) @binding(3) var<uniform> color_b: vec4<f32>;
@group(2) @binding(4) var<uniform> color_c: vec4<f32>;
@group(2) @binding(5) var<uniform> params_b: vec4<f32>;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) alpha: f32,
    // Gradient coordinate and flicker are per-particle constants; flat
    // interpolation preserves them exactly (provoking vertex).
    @location(2) @interpolate(flat) t: f32,
    @location(3) @interpolate(flat) flicker: f32,
};

// One corner of the two-triangle quad: offset (xy) + uv (zw).
fn quad_corner(corner: u32, half: f32) -> vec4<f32> {
    switch corner {
        case 0u: { return vec4<f32>(-half, -half, 0.0, 1.0); }
        case 1u: { return vec4<f32>( half, -half, 1.0, 1.0); }
        case 2u: { return vec4<f32>( half,  half, 1.0, 0.0); }
        case 3u: { return vec4<f32>(-half, -half, 0.0, 1.0); }
        case 4u: { return vec4<f32>( half,  half, 1.0, 0.0); }
        default: { return vec4<f32>(-half,  half, 0.0, 0.0); }
    }
}

// Lifetime alpha envelope: ramp in over the first 12% of life, hold, fade
// out over the last 45%. Dead (age >= lifespan, incl. the zeroed spawn
// state) yields exactly 0.
fn life_alpha(age: f32, lifespan: f32) -> f32 {
    if (lifespan <= 0.0 || age >= lifespan) {
        return 0.0;
    }
    let lf = age / lifespan;
    return smoothstep(0.0, 0.12, lf) * (1.0 - smoothstep(0.55, 1.0, lf));
}

@vertex
fn vertex(
    @builtin(vertex_index) vertex_index: u32,
    @location(0) local_pos: vec3<f32>,
) -> VertexOutput {
    let particle_index = vertex_index / 6u;
    let corner_index = vertex_index % 6u;

    let p = particles[particle_index];
    let alpha = life_alpha(p.age, p.lifespan);
    // Collapse dead particles to a zero-area quad: the rasterizer culls
    // them and they cost no fill (particles/render.wgsl idiom).
    let live = f32(alpha > 0.0);
    let c = quad_corner(corner_index, params_a.y);
    let world_pos = vec4<f32>(p.position + c.xy * live, 0.0, 1.0);

    var out: VertexOutput;
    out.clip_position = view.clip_from_world * world_pos;
    out.uv = c.zw;
    out.alpha = alpha;
    // Audio shifts the gradient coordinate; fract wraps it along the ramp.
    out.t = fract(p.seed + params_a.z);
    // Sparkle: a per-particle deterministic flicker, amplitude = highs drive.
    out.flicker = 1.0 + params_a.w * 0.6 * sin(params_b.x * 21.0 + p.seed * 6.2831853);
    return out;
}

// Three-stop gradient a -> b -> c.
fn gradient(t: f32) -> vec3<f32> {
    if (t < 0.5) {
        return mix(color_a.rgb, color_b.rgb, t * 2.0);
    }
    return mix(color_b.rgb, color_c.rgb, (t - 0.5) * 2.0);
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // Procedural soft disc: quadratic falloff squared — a tight bright core
    // with soft skirts, no texture fetch.
    let d = length(in.uv - vec2<f32>(0.5)) * 2.0;
    let disc = pow(max(1.0 - d * d, 0.0), 2.0);
    // Additive (One, One): the fragment multiplies its own envelope in; the
    // alpha lane is ignored by the blend.
    let rgb = gradient(in.t) * (params_a.x * in.flicker * disc * in.alpha);
    return vec4<f32>(rgb, 1.0);
}
```

- [ ] **Step 2: Write the material half of `render.rs` (complete)**

```rust
//! Radiance materials: the additive aura billboards ([`RadianceMaterial`])
//! and (Task 8) the silhouette fill quad + the per-frame driver.
//!
//! ## Additive blend
//!
//! `AlphaMode2d::Blend` routes the draw into `Transparent2d`, and
//! [`RadianceMaterial::specialize`] overrides the color target to pure
//! additive `(One, One)` — flame's recipe, per-material-pipeline so it never
//! leaks into the other sketches' blends. Gradient stops are linear HDR (may
//! exceed 1.0) so cores clear the tonemapper's white knee and bloom.

use bevy::asset::Asset;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState,
    RenderPipelineDescriptor, SpecializedMeshPipelineError,
};
use bevy::render::storage::ShaderBuffer;
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d, Material2dKey};

/// Billboard half-size in world px (Camera2d: 1 unit = 1 px), passed to the
/// shader via `params_a.y`.
pub const QUAD_HALF_PX: f32 = 6.0;

/// The additive soft-disc billboard material for the aura particles.
///
/// Shares the particle `ShaderBuffer` handle with
/// [`crate::radiance::compute::sim_params::RadianceSimParams`] (compute
/// writes read-write; this vertex shader reads read-only).
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct RadianceMaterial {
    /// Particle storage buffer, read-only from the vertex shader.
    #[storage(0, read_only)]
    pub particles: Handle<ShaderBuffer>,
    /// x = master intensity (HDR, audio-lifted), y = quad half px,
    /// z = palette shift `0..1`, w = sparkle `0..1`.
    #[uniform(1)]
    pub params_a: Vec4,
    /// Gradient stop A (linear HDR).
    #[uniform(2)]
    pub color_a: Vec4,
    /// Gradient stop B.
    #[uniform(3)]
    pub color_b: Vec4,
    /// Gradient stop C.
    #[uniform(4)]
    pub color_c: Vec4,
    /// x = elapsed seconds (sparkle phase), y/z/w reserved (zero).
    #[uniform(5)]
    pub params_b: Vec4,
}

impl Material2d for RadianceMaterial {
    fn vertex_shader() -> ShaderRef {
        "shaders/radiance/render.wgsl".into()
    }

    fn fragment_shader() -> ShaderRef {
        "shaders/radiance/render.wgsl".into()
    }

    /// `Blend` routes into `Transparent2d`; [`Self::specialize`] then makes
    /// it pure additive.
    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }

    /// Override the color-target blend to pure additive `(One, One)` —
    /// flame's mechanism for HDR accumulation inside the 2D pipeline.
    fn specialize(
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: Material2dKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if let Some(fragment) = descriptor.fragment.as_mut() {
            if let Some(Some(target)) = fragment.targets.get_mut(0) {
                target.blend = Some(BlendState {
                    color: BlendComponent {
                        src_factor: BlendFactor::One,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                    alpha: BlendComponent {
                        src_factor: BlendFactor::One,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                });
            }
        }
        Ok(())
    }
}
```

Add `pub mod render;` to `radiance/mod.rs`. In `crates/wc-sketches/src/lib.rs`, next to the flame material registration:

```rust
        // Radiance additive billboard material, registered once (Plugin
        // singleton; the mesh + material entity spawns on Radiance entry).
        app.add_plugins(Material2dPlugin::<crate::radiance::render::RadianceMaterial>::default());
```

- [ ] **Step 3: Verify (pure wiring)**

Run: `cargo check -p wc-sketches`
Expected: clean. (GPU output is verified by the Task 15 capture scenarios.)

- [ ] **Step 4: Commit**

```bash
git add assets/shaders/radiance/render.wgsl crates/wc-sketches/src/radiance crates/wc-sketches/src/lib.rs
git commit -m "feat(radiance): additive soft-disc billboard material and render shader" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 8: Silhouette material, `silhouette.wgsl`, and the material driver

**Files:**
- Create: `assets/shaders/radiance/silhouette.wgsl`
- Modify: `crates/wc-sketches/src/radiance/render.rs` (append the silhouette material + the driver + tests)
- Modify: `crates/wc-sketches/src/lib.rs` (register `Material2dPlugin::<RadianceSilhouetteMaterial>`)

**Interfaces:**
- Consumes: pinned `MaskTexture` (256² `R8Unorm`, filterable — sampled with a plain sampler); `RadianceSettings` (fill/rim/threshold/mirror/palette/mask_debug_overlay); `RadianceState` (Task 5); `ScreensaverFade` (ember blend); `RadianceRoot` (defined in Task 9 — the driver's queries reference it, so this task's driver code lands but is only *registered* in Task 9's plugin step; the compile check needs `RadianceRoot`, so define it now in `systems/spawn.rs` as a stub if executing strictly in order — see Step 2 note).
- Produces: `RadianceSilhouetteMaterial`, `drive_radiance_materials`.

- [ ] **Step 1: Write the silhouette shader (complete)**

```wgsl
// Radiance silhouette fill — a window-filling quad under the particles,
// sampling the 256x256 R8Unorm person mask: a smoothstep-edged dark glassy
// body fill (deep translucent vertical gradient + audio-shimmered value
// noise) and a thin emissive rim in the mask's edge band. The rim color is
// HDR (palette-derived, scaled by rim glow) so it blooms; the fill is dark
// and mostly occludes via ordinary alpha blending.
//
// Bindings (group 2):
//   @binding(0)/(1): mask texture + sampler (R8Unorm is filterable).
//   @binding(2): fill_params — x fill intensity, y rim glow, z mask
//                threshold, w mirror (1 = flip x).
//   @binding(3): effect_params — x elapsed seconds, y shimmer amount
//                (highs-driven), z raw-mask debug (1 = draw the mask
//                grayscale), w reserved.
//   @binding(4): fill_color — deep glassy base (linear).
//   @binding(5): rim_color — emissive rim (linear HDR).

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

@group(2) @binding(0) var mask_tex: texture_2d<f32>;
@group(2) @binding(1) var mask_samp: sampler;
@group(2) @binding(2) var<uniform> fill_params: vec4<f32>;
@group(2) @binding(3) var<uniform> effect_params: vec4<f32>;
@group(2) @binding(4) var<uniform> fill_color: vec4<f32>;
@group(2) @binding(5) var<uniform> rim_color: vec4<f32>;

// 2D hash -> [0, 1) (Hoskins-style, texture-free).
fn hash21(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.xyx) * 0.1031);
    p3 = p3 + dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

// Smooth bilinear value noise over the hash lattice.
fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash21(i);
    let b = hash21(i + vec2<f32>(1.0, 0.0));
    let c = hash21(i + vec2<f32>(0.0, 1.0));
    let d = hash21(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // Quad UV == mask UV under the full-window stretch mapping; the mirror
    // flip here matches the kernel's mask_uv_to_world so fill, rim, and
    // particle spawn positions all agree.
    var uv = in.uv;
    if (fill_params.w > 0.5) {
        uv.x = 1.0 - uv.x;
    }
    let m = textureSample(mask_tex, mask_samp, uv).r;

    // Dev isolation: raw mask grayscale (mask_debug_overlay).
    if (effect_params.z > 0.5) {
        return vec4<f32>(m, m, m, 1.0);
    }

    let th = fill_params.z;
    // Soft body coverage around the threshold; the 256^2 mask is
    // impressionistic by design (aura, not cutout).
    let body = smoothstep(th - 0.06, th + 0.06, m);

    // Dark glassy fill: deep base hue, brighter toward the top (a glass
    // sheen), shimmered by slow-scrolling value noise whose amplitude rides
    // the high-band audio drive.
    let noise = value_noise(uv * 9.0 + vec2<f32>(0.0, effect_params.x * 0.15));
    let shimmer = 1.0 + effect_params.y * 0.5 * (noise - 0.5);
    let glass = fill_color.rgb * mix(1.25, 0.55, uv.y) * shimmer;

    // Emissive rim: peaks where coverage crosses the threshold band
    // (body*(1-body) is a soft bump centered on the edge).
    let rim = body * (1.0 - body) * 4.0;

    let rgb = glass * fill_params.x * body + rim_color.rgb * (rim * fill_params.y);
    // The fill occludes (alpha ~= body); the rim contribution rides the same
    // alpha-blended draw, made visible by its HDR magnitude.
    return vec4<f32>(rgb, clamp(body * 0.9, 0.0, 1.0));
}
```

- [ ] **Step 2: Append to `render.rs` (complete)**

Note on ordering: `drive_radiance_materials` queries `With<RadianceRoot>`. If executing tasks strictly in sequence, create `crates/wc-sketches/src/radiance/systems/spawn.rs` NOW containing only the marker (Task 9 fills in the rest):

```rust
//! `OnEnter(AppState::Radiance)` spawn plus the `OnExit` teardown.
//! (Populated in Task 9; the marker lands first so the material driver
//! compiles.)

use bevy::prelude::*;

/// Marker component on every entity owned by the Radiance sketch;
/// `OnExit(AppState::Radiance)` despawns everything tagged with it.
#[derive(Component)]
pub struct RadianceRoot;
```

and add `pub mod spawn;` to `systems/mod.rs`.

Append to `render.rs`:

```rust
use wc_core::lifecycle::screensaver::fade::ScreensaverFade;

use crate::radiance::settings::{RadiancePalette, RadianceSettings};
use crate::radiance::systems::sim_params::RadianceState;
use crate::radiance::systems::spawn::RadianceRoot;

/// The window-filling silhouette material sampling the person mask.
///
/// Drawn under the particles (spawned at z 0.0 vs the billboards' z 1.0) via
/// ordinary alpha blending; only the rim is HDR-emissive.
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct RadianceSilhouetteMaterial {
    /// The shared 256² `R8Unorm` person mask (Plan B writes it in place;
    /// Bevy re-uploads on mutation).
    #[texture(0)]
    #[sampler(1)]
    pub mask: Handle<Image>,
    /// x = fill intensity, y = rim glow, z = mask threshold, w = mirror.
    #[uniform(2)]
    pub fill_params: Vec4,
    /// x = elapsed seconds, y = shimmer amount, z = raw-mask debug, w = 0.
    #[uniform(3)]
    pub effect_params: Vec4,
    /// Deep glassy base color (linear).
    #[uniform(4)]
    pub fill_color: Vec4,
    /// Emissive rim color (linear HDR).
    #[uniform(5)]
    pub rim_color: Vec4,
}

impl Material2d for RadianceSilhouetteMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/radiance/silhouette.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }
}

/// The deep indigo glass base fill (linear). A constant, not a setting: the
/// palette drives the rim; the body stays a dark glassy anchor.
#[must_use]
pub fn silhouette_fill_color() -> Vec4 {
    Vec4::new(0.05, 0.03, 0.10, 1.0)
}

/// Pack the particle-material params + palette stops for one frame,
/// ember-blended by the screensaver fade envelope. Pure for testability.
///
/// Returns `(params_a, [color_a, color_b, color_c])`.
#[must_use]
pub fn particle_material_params(
    state: &RadianceState,
    palette: RadiancePalette,
    fade_alpha: f32,
) -> (Vec4, [Vec4; 3]) {
    let a = fade_alpha.clamp(0.0, 1.0);
    // Attract mode dims toward the ember: intensity eases to 70%.
    let intensity = state.intensity * (1.0 - a * 0.3);
    let params_a = Vec4::new(intensity, QUAD_HALF_PX, state.palette_shift, state.sparkle);
    let user = palette.stops();
    let ember = RadiancePalette::Ember.stops();
    let colors = [
        user[0].lerp(ember[0], a),
        user[1].lerp(ember[1], a),
        user[2].lerp(ember[2], a),
    ];
    (params_a, colors)
}

/// `Update` (gated `in_state(AppState::Radiance)` — runs through Idle and
/// the screensaver like flame's material driver, so the ember blend and the
/// held last-frame envelopes keep rendering): pack settings + state into
/// both materials every frame. The per-frame cost is a small uniform
/// re-prepare, the same class as flame's eight-uniform driver.
pub fn drive_radiance_materials(
    time: Res<'_, Time>,
    settings: Res<'_, RadianceSettings>,
    state: Res<'_, RadianceState>,
    fade: Res<'_, ScreensaverFade>,
    particle_roots: Query<'_, '_, &bevy::sprite_render::MeshMaterial2d<RadianceMaterial>, With<RadianceRoot>>,
    silhouette_roots: Query<
        '_,
        '_,
        &bevy::sprite_render::MeshMaterial2d<RadianceSilhouetteMaterial>,
        With<RadianceRoot>,
    >,
    mut particle_materials: ResMut<'_, Assets<RadianceMaterial>>,
    mut silhouette_materials: ResMut<'_, Assets<RadianceSilhouetteMaterial>>,
) {
    let (params_a, colors) = particle_material_params(&state, settings.palette, fade.alpha());
    let params_b = Vec4::new(time.elapsed_secs(), 0.0, 0.0, 0.0);
    for handle in &particle_roots {
        if let Some(material) = particle_materials.get_mut(&handle.0) {
            material.params_a = params_a;
            material.color_a = colors[0];
            material.color_b = colors[1];
            material.color_c = colors[2];
            material.params_b = params_b;
        }
    }
    // Rim takes the palette's hottest stop (ember-blended like the
    // particles); the debug lane routes the raw-mask overlay.
    let rim = colors[2];
    let fill_params = Vec4::new(
        settings.silhouette_fill,
        settings.rim_glow,
        settings.mask_threshold,
        f32::from(u8::from(settings.mirror)),
    );
    let effect_params = Vec4::new(
        time.elapsed_secs(),
        state.sparkle,
        f32::from(u8::from(settings.mask_debug_overlay)),
        0.0,
    );
    for handle in &silhouette_roots {
        if let Some(material) = silhouette_materials.get_mut(&handle.0) {
            material.fill_params = fill_params;
            material.effect_params = effect_params;
            material.fill_color = silhouette_fill_color();
            material.rim_color = rim;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fade 0 (Active) uses the user palette verbatim; fade 1 lands on the
    /// ember stops with intensity eased to 70%.
    #[test]
    fn particle_params_blend_to_ember_on_fade() {
        let state = RadianceState {
            onset_env: 0.0,
            intensity: 1.0,
            sparkle: 0.4,
            palette_shift: 0.25,
        };
        let (pa0, c0) = particle_material_params(&state, RadiancePalette::Prism, 0.0);
        assert!((pa0.x - 1.0).abs() < 1e-6);
        assert!((pa0.z - 0.25).abs() < 1e-6);
        assert_eq!(c0, RadiancePalette::Prism.stops());
        let (pa1, c1) = particle_material_params(&state, RadiancePalette::Prism, 1.0);
        assert!((pa1.x - 0.7).abs() < 1e-6, "ember intensity ease");
        assert_eq!(c1, RadiancePalette::Ember.stops());
    }

    /// The quad half-size lane is the shared constant.
    #[test]
    fn particle_params_carry_quad_half() {
        let state = RadianceState::default();
        let (pa, _) = particle_material_params(&state, RadiancePalette::Ocean, 0.0);
        assert!((pa.y - QUAD_HALF_PX).abs() < f32::EPSILON);
    }
}
```

In `crates/wc-sketches/src/lib.rs`, next to the `RadianceMaterial` registration:

```rust
        app.add_plugins(Material2dPlugin::<
            crate::radiance::render::RadianceSilhouetteMaterial,
        >::default());
```

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p wc-sketches radiance::render`
Expected: PASS (2 tests).

- [ ] **Step 4: Commit**

```bash
git add assets/shaders/radiance/silhouette.wgsl crates/wc-sketches/src/radiance crates/wc-sketches/src/lib.rs
git commit -m "feat(radiance): silhouette fill material, rim shader, and material driver" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 9: Spawn/teardown, tracking requests, camera arbitration

**Files:**
- Modify: `crates/wc-sketches/src/radiance/systems/spawn.rs` (fill in from the Task 8 stub)
- Create: `crates/wc-sketches/src/radiance/systems/arbitration.rs`
- Modify: `crates/wc-sketches/src/radiance/systems/mod.rs` (add `pub mod arbitration;`)

**Interfaces:**
- Consumes: pinned `AudioCaptureRequest { device_name: Option<String>, paused: bool }` and `BodyTrackingRequest { idle_throttle: bool, .. }` (insert-to-start / remove-to-stop activation contract; the `..` tuning fields land in Task 14 — until then construct with the fields the merged Plan B actually has, plus `..Default` if Plan B added one); pinned `MaskTexture(pub Handle<Image>)`, `SilhouetteEdges { points, generation }`, `MASK_SIZE`, `MAX_EDGE_POINTS`; `wc_core::input::provider::{ProviderRegistry, ProviderId}` (unconditional module); Tasks 3/5/7/8 types.
- Produces: `spawn_radiance`, `remove_radiance_resources`, `insert_tracking_requests`, `remove_tracking_requests`, `ensure_body_surfaces`, `suspend_mediapipe_hand_camera`, `resume_mediapipe_hand_camera` + `PendingHandCameraRestore`.

- [ ] **Step 1: Write `systems/spawn.rs` (complete, replacing the stub body — keep the `RadianceRoot` marker)**

```rust
//! `OnEnter(AppState::Radiance)` spawn plus the `OnExit` teardown.
//!
//! Allocates the particle storage buffer (zeroed = all dead; the kernel's
//! edge-respawn births every particle), the billboard mesh (count × 6
//! vertices, data unused — the vertex shader derives everything from
//! `vertex_index`), the silhouette quad, and the sim resources; inserts the
//! Plan A/B activation requests. On exit everything is dropped, the requests
//! are removed (stopping the mic stream and the body worker), and the
//! render-world `RadianceSimParams` copy dies via the compute plugin's
//! removal companion.

use bevy::asset::RenderAssetUsages;
use bevy::image::Image;
use bevy::mesh::PrimitiveTopology;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::render::storage::ShaderBuffer;
use bytemuck::{cast_slice, Zeroable};
use wc_core::input::body::{
    BodyTrackingRequest, MaskTexture, SilhouetteEdges, MASK_SIZE, MAX_EDGE_POINTS,
};
use wc_core::audio::input::AudioCaptureRequest;

use crate::radiance::compute::sim_params::{RadianceParticle, RadianceSimParams, RadianceSimParamsGpu};
use crate::radiance::render::{
    silhouette_fill_color, RadianceMaterial, RadianceSilhouetteMaterial, QUAD_HALF_PX,
};
use crate::radiance::settings::{RadiancePalette, RadianceSettings};
use crate::radiance::systems::sim_params::RadianceState;

/// Marker component on every entity owned by the Radiance sketch;
/// `OnExit(AppState::Radiance)` despawns everything tagged with it.
#[derive(Component)]
pub struct RadianceRoot;

/// Ensure the Plan B mask + edge resources exist (init-if-absent).
///
/// With the body-tracking plugin present these already exist and this is a
/// no-op; in headless tests, feature-reduced harnesses, and the synthetic
/// capture path this creates the same shapes so the silhouette material, the
/// phantom, and the edge upload always have a target. Runs first in the
/// `OnEnter` chain.
pub fn ensure_body_surfaces(
    mask: Option<Res<'_, MaskTexture>>,
    edges: Option<Res<'_, SilhouetteEdges>>,
    mut images: ResMut<'_, Assets<Image>>,
    mut commands: Commands<'_, '_>,
) {
    if mask.is_none() {
        let image = Image::new_fill(
            Extent3d {
                width: u32::try_from(MASK_SIZE).unwrap_or(256),
                height: u32::try_from(MASK_SIZE).unwrap_or(256),
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            &[0u8],
            TextureFormat::R8Unorm,
            RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
        );
        commands.insert_resource(MaskTexture(images.add(image)));
    }
    if edges.is_none() {
        commands.insert_resource(SilhouetteEdges {
            points: Vec::with_capacity(MAX_EDGE_POINTS),
            generation: 0,
        });
    }
}

/// `OnEnter(AppState::Radiance)`: allocate the buffers, spawn the two draw
/// entities, insert the sim resources.
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "particle_count is bounded by the 10k..300k settings slider, exact as f32"
)]
pub fn spawn_radiance(
    settings: Res<'_, RadianceSettings>,
    mask: Res<'_, MaskTexture>,
    mut buffers: ResMut<'_, Assets<ShaderBuffer>>,
    mut meshes: ResMut<'_, Assets<Mesh>>,
    mut particle_materials: ResMut<'_, Assets<RadianceMaterial>>,
    mut silhouette_materials: ResMut<'_, Assets<RadianceSilhouetteMaterial>>,
    window: Single<'_, '_, &Window>,
    mut commands: Commands<'_, '_>,
) {
    let count = settings.particle_count.clamp(1_000.0, 300_000.0) as u32;
    let capacity = count as usize;

    // Zeroed = all dead; the kernel births every particle at the edge list.
    // RENDER_WORLD-only: the CPU never rewrites it after this seed.
    let particles = vec![RadianceParticle::zeroed(); capacity];
    let particles_handle = buffers.add(ShaderBuffer::new(
        cast_slice::<RadianceParticle, u8>(&particles),
        RenderAssetUsages::RENDER_WORLD,
    ));

    // Billboard mesh: count × 6 origin vertices; only the draw count matters
    // (the flame/particles idiom).
    let positions: Vec<[f32; 3]> = vec![[0.0, 0.0, 0.0]; capacity * 6];
    let mut billboard_mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    );
    billboard_mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    let billboard_mesh_handle = meshes.add(billboard_mesh);

    let w = window.width().max(1.0);
    let h = window.height().max(1.0);

    // One-frame placeholder uniforms; drive_radiance_materials overwrites
    // every lane next Update.
    let stops = settings.palette.stops();
    let particle_material = particle_materials.add(RadianceMaterial {
        particles: particles_handle.clone(),
        params_a: Vec4::new(0.55, QUAD_HALF_PX, 0.0, 0.0),
        color_a: stops[0],
        color_b: stops[1],
        color_c: stops[2],
        params_b: Vec4::ZERO,
    });
    let silhouette_material = silhouette_materials.add(RadianceSilhouetteMaterial {
        mask: mask.0.clone(),
        fill_params: Vec4::new(
            settings.silhouette_fill,
            settings.rim_glow,
            settings.mask_threshold,
            f32::from(u8::from(settings.mirror)),
        ),
        effect_params: Vec4::ZERO,
        fill_color: silhouette_fill_color(),
        rim_color: stops[2],
    });

    // Silhouette quad under (z 0.0) the billboards (z 1.0) in Transparent2d's
    // z-sort.
    commands.spawn((
        RadianceRoot,
        bevy::mesh::Mesh2d(meshes.add(Mesh::from(Rectangle::new(w, h)))),
        bevy::sprite_render::MeshMaterial2d(silhouette_material),
        Transform::from_xyz(0.0, 0.0, 0.0),
        GlobalTransform::default(),
        Visibility::default(),
    ));
    commands.spawn((
        RadianceRoot,
        bevy::mesh::Mesh2d(billboard_mesh_handle),
        bevy::sprite_render::MeshMaterial2d(particle_material),
        Transform::from_xyz(0.0, 0.0, 1.0),
        GlobalTransform::default(),
        Visibility::default(),
    ));

    // Zeroed params (emission 0, no edges) until the first bake next Update.
    commands.insert_resource(RadianceSimParams {
        params: RadianceSimParamsGpu::zeroed(),
        particles: particles_handle,
        particle_count: count,
    });
    commands.insert_resource(RadianceState::default());
}

/// `OnEnter(AppState::Radiance)` (chained after `spawn_radiance`): start the
/// mic capture + body tracking via the Plan A/B activation contracts.
///
/// Skipped under the synthetic-body capture toggle (debug builds): capture
/// scenarios must not open a microphone or camera.
pub fn insert_tracking_requests(
    settings: Res<'_, RadianceSettings>,
    #[cfg(debug_assertions)] toggles: Option<Res<'_, wc_core::debug::DebugToggles>>,
    mut commands: Commands<'_, '_>,
) {
    #[cfg(debug_assertions)]
    if toggles.is_some_and(|t| t.force_radiance_synthetic_body) {
        tracing::info!("radiance: synthetic body forced; skipping mic + camera requests");
        return;
    }
    let device = settings.audio_input_device.trim();
    commands.insert_resource(AudioCaptureRequest {
        device_name: if device.is_empty() {
            None
        } else {
            Some(device.to_owned())
        },
        paused: false,
    });
    commands.insert_resource(BodyTrackingRequest {
        idle_throttle: false,
        mask_ema: settings.mask_ema,
        one_euro_min_cutoff: settings.one_euro_min_cutoff,
        one_euro_beta: settings.one_euro_beta,
    });
}

/// `OnExit(AppState::Radiance)`: drop the sim resources (releasing the
/// particle buffer's VRAM via its sole handle) and stop capture/tracking by
/// removing the activation requests (their contract: remove to stop).
pub fn remove_radiance_resources(mut commands: Commands<'_, '_>) {
    commands.remove_resource::<RadianceSimParams>();
    commands.remove_resource::<RadianceState>();
    commands.remove_resource::<AudioCaptureRequest>();
    commands.remove_resource::<BodyTrackingRequest>();
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use bevy::asset::AssetPlugin;
    use bevy::ecs::system::RunSystemOnce;

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        app.init_asset::<ShaderBuffer>();
        app.init_asset::<Mesh>();
        app.init_asset::<Image>();
        app.init_asset::<RadianceMaterial>();
        app.init_asset::<RadianceSilhouetteMaterial>();
        app.world_mut().spawn(Window::default());
        app.insert_resource(RadianceSettings::default());
        app
    }

    /// `ensure_body_surfaces` creates the mask + edges when absent and
    /// leaves an existing pair untouched.
    #[test]
    fn ensure_body_surfaces_is_init_if_absent() {
        let mut app = test_app();
        app.world_mut()
            .run_system_once(ensure_body_surfaces)
            .expect("runs");
        let first = app.world().resource::<MaskTexture>().0.clone();
        assert!(app.world().get_resource::<SilhouetteEdges>().is_some());
        app.world_mut()
            .run_system_once(ensure_body_surfaces)
            .expect("runs again");
        assert_eq!(
            app.world().resource::<MaskTexture>().0,
            first,
            "existing mask must not be replaced"
        );
    }

    /// Spawn sizes the buffer + mesh from the setting, inserts the sim
    /// resources zeroed, and teardown drops them plus both requests.
    #[test]
    fn spawn_sizes_buffers_and_teardown_drops_resources() {
        let mut app = test_app();
        app.world_mut()
            .resource_mut::<RadianceSettings>()
            .particle_count = 12_000.0;
        app.world_mut()
            .run_system_once(ensure_body_surfaces)
            .expect("surfaces");
        app.world_mut()
            .run_system_once(spawn_radiance)
            .expect("spawn runs");

        let sim = app.world().resource::<RadianceSimParams>();
        assert_eq!(sim.particle_count, 12_000);
        assert!(sim.params.emission_prob.abs() < f32::EPSILON, "zeroed until first bake");
        let handle = sim.particles.clone();
        let buffers = app.world().resource::<Assets<ShaderBuffer>>();
        let buffer = buffers.get(&handle).expect("particle buffer present");
        let data = buffer.data.as_ref().expect("cpu seed present");
        assert_eq!(data.len(), 12_000 * 32, "32-byte particles at full count");
        assert!(data.iter().all(|&b| b == 0), "zeroed = all dead");

        // Two draw entities (silhouette + billboards) under the marker.
        let mut roots = app
            .world_mut()
            .query_filtered::<Entity, With<RadianceRoot>>();
        assert_eq!(roots.iter(app.world()).count(), 2);

        app.world_mut()
            .run_system_once(insert_tracking_requests)
            .expect("requests");
        assert!(app.world().get_resource::<AudioCaptureRequest>().is_some());
        assert!(app.world().get_resource::<BodyTrackingRequest>().is_some());

        app.world_mut()
            .run_system_once(remove_radiance_resources)
            .expect("teardown");
        assert!(app.world().get_resource::<RadianceSimParams>().is_none());
        assert!(app.world().get_resource::<RadianceState>().is_none());
        assert!(app.world().get_resource::<AudioCaptureRequest>().is_none());
        assert!(app.world().get_resource::<BodyTrackingRequest>().is_none());
    }

    /// The device name maps empty → system default (None), trimmed → Some.
    #[test]
    fn request_maps_device_name() {
        let mut app = test_app();
        app.world_mut()
            .resource_mut::<RadianceSettings>()
            .audio_input_device = "  USB Interface  ".to_owned();
        app.world_mut()
            .run_system_once(insert_tracking_requests)
            .expect("requests");
        let req = app.world().resource::<AudioCaptureRequest>();
        assert_eq!(req.device_name.as_deref(), Some("USB Interface"));
        assert!(!req.paused);
    }
}
```

Note for the implementer: if the merged Plan B's `BodyTrackingRequest` does not yet carry the three tuning fields, Task 14 adds them — until Task 14 executes, construct the request with only `idle_throttle` and leave a `// TASK 14 wires tuning` marker is NOT acceptable; instead execute Task 14's struct change *first* if the literal above fails to compile (the two tasks are order-flexible by design).

- [ ] **Step 2: Write `systems/arbitration.rs` (complete)**

```rust
//! Camera arbitration: body replaces hands while Radiance is active.
//!
//! `OnEnter(AppState::Radiance)`: if a MediaPipe (webcam) hand provider is
//! registered, stop it synchronously — the worker joins and releases the
//! camera — so Plan B's body worker can open the same device. The provider
//! stays *registered* (its `NotStarted` status is the honest dev-panel
//! signal that it is suspended). Leap is untouched; Radiance never reads
//! hand data.
//!
//! `OnExit`: restart is *deferred* ~0.75 s via [`PendingHandCameraRestore`]
//! so the body worker (torn down by the request removal, observed by Plan
//! B's watcher on the following frame) has released the camera before the
//! hand provider re-opens it. The restore listener is registered always-on
//! and early-outs on a `None` resource in one branch — the same
//! cheap-no-op contract as the sanctioned reload listeners; it self-removes
//! after firing.

use std::time::Duration;

use bevy::prelude::*;
use wc_core::input::provider::{ProviderId, ProviderRegistry};

/// Marker: Radiance stopped the MediaPipe hand provider on entry and owes a
/// restart on exit.
#[derive(Resource)]
pub struct SuspendedHandCamera;

/// Deferred restore: restart the suspended MediaPipe hand provider once
/// `Time::elapsed` passes `at`.
#[derive(Resource)]
pub struct PendingHandCameraRestore {
    /// Instant (Bevy `Time::elapsed`) after which the restart runs.
    pub at: Duration,
}

/// How long after exit to wait before re-opening the hand camera, giving the
/// body worker's teardown time to release the device.
pub const RESTORE_DELAY: Duration = Duration::from_millis(750);

/// `OnEnter(AppState::Radiance)`: stop a registered MediaPipe hand provider
/// (releasing the webcam) and remember to restore it.
pub fn suspend_mediapipe_hand_camera(
    registry: Option<ResMut<'_, ProviderRegistry>>,
    mut commands: Commands<'_, '_>,
) {
    let Some(mut registry) = registry else {
        return; // headless / hand tracking not installed
    };
    let Some(slot) = registry.iter_mut().find(|p| p.id == ProviderId::MediaPipe) else {
        return; // Leap / mock / Off: nothing to arbitrate
    };
    slot.inner.stop();
    tracing::info!(
        "radiance: suspended the MediaPipe hand provider (webcam handed to body tracking)"
    );
    commands.insert_resource(SuspendedHandCamera);
}

/// `OnExit(AppState::Radiance)`: schedule the deferred restore (only if we
/// actually suspended on entry).
pub fn schedule_hand_camera_restore(
    suspended: Option<Res<'_, SuspendedHandCamera>>,
    time: Res<'_, Time>,
    mut commands: Commands<'_, '_>,
) {
    if suspended.is_none() {
        return;
    }
    commands.remove_resource::<SuspendedHandCamera>();
    commands.insert_resource(PendingHandCameraRestore {
        at: time.elapsed() + RESTORE_DELAY,
    });
}

/// Always-on `Update` listener: one `Option` branch in the steady state;
/// restarts the MediaPipe hand provider once the delay passes, then removes
/// itself. A start failure is logged and stays visible as the provider's
/// honest `Errored` status (the house failure philosophy); re-picking the
/// provider in the tracking dropdown re-probes.
pub fn resume_hand_camera_when_due(
    pending: Option<Res<'_, PendingHandCameraRestore>>,
    time: Res<'_, Time>,
    registry: Option<ResMut<'_, ProviderRegistry>>,
    mut commands: Commands<'_, '_>,
) {
    let Some(pending) = pending else {
        return; // steady-state no-op
    };
    if time.elapsed() < pending.at {
        return;
    }
    commands.remove_resource::<PendingHandCameraRestore>();
    let Some(mut registry) = registry else {
        return;
    };
    let Some(slot) = registry.iter_mut().find(|p| p.id == ProviderId::MediaPipe) else {
        // The operator switched providers while Radiance ran; nothing owed.
        return;
    };
    match slot.inner.start() {
        Ok(()) => tracing::info!("radiance: restored the MediaPipe hand provider"),
        Err(err) => tracing::error!(
            ?err,
            "radiance: failed to restore the MediaPipe hand provider; its status \
             stays visible in the dev panel"
        ),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use wc_core::input::provider::{HandTrackingProvider, ProviderRole};
    use wc_core::input::state::{
        HandTrackingError, HandTrackingFrame, ProviderDiagnostics, ProviderStatus,
    };

    /// Scripted provider counting start/stop calls (mirrors the binary's
    /// ServiceStub test pattern).
    struct CountingStub {
        starts: Arc<AtomicUsize>,
        stops: Arc<AtomicUsize>,
    }

    impl HandTrackingProvider for CountingStub {
        fn start(&mut self) -> Result<(), HandTrackingError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn stop(&mut self) {
            self.stops.fetch_add(1, Ordering::SeqCst);
        }
        fn poll(&mut self, _now: Duration, _out: &mut Messages<HandTrackingFrame>) {}
        fn status(&self) -> ProviderStatus {
            ProviderStatus::default()
        }
        fn diagnostics(&self) -> ProviderDiagnostics {
            ProviderDiagnostics::default()
        }
    }

    fn registry_with(id: ProviderId) -> (ProviderRegistry, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let starts = Arc::new(AtomicUsize::new(0));
        let stops = Arc::new(AtomicUsize::new(0));
        let mut registry = ProviderRegistry::default();
        registry.register(
            id,
            ProviderRole::Primary,
            Box::new(CountingStub {
                starts: Arc::clone(&starts),
                stops: Arc::clone(&stops),
            }),
        );
        (registry, starts, stops)
    }

    /// Suspend stops a registered MediaPipe provider and marks the debt;
    /// the deferred restore fires once the delay passes and self-removes.
    #[test]
    fn suspend_then_deferred_restore_round_trip() {
        use bevy::ecs::system::RunSystemOnce;
        let (registry, starts, stops) = registry_with(ProviderId::MediaPipe);
        // register() auto-starts once; ignore that baseline.
        let base_starts = starts.load(Ordering::SeqCst);

        let mut world = World::new();
        world.insert_resource(registry);
        world.insert_resource(Time::<()>::default());

        world
            .run_system_once(suspend_mediapipe_hand_camera)
            .expect("suspend");
        assert_eq!(stops.load(Ordering::SeqCst), 1, "provider stopped");
        assert!(world.get_resource::<SuspendedHandCamera>().is_some());

        world
            .run_system_once(schedule_hand_camera_restore)
            .expect("schedule");
        assert!(world.get_resource::<SuspendedHandCamera>().is_none());
        assert!(world.get_resource::<PendingHandCameraRestore>().is_some());

        // Before the delay: no restart.
        world
            .run_system_once(resume_hand_camera_when_due)
            .expect("early poll");
        assert_eq!(starts.load(Ordering::SeqCst), base_starts);

        // Advance past the delay: restart fires exactly once and clears.
        let mut time = Time::<()>::default();
        time.advance_by(RESTORE_DELAY + Duration::from_millis(10));
        world.insert_resource(time);
        world
            .run_system_once(resume_hand_camera_when_due)
            .expect("due poll");
        assert_eq!(starts.load(Ordering::SeqCst), base_starts + 1);
        assert!(world.get_resource::<PendingHandCameraRestore>().is_none());
    }

    /// A non-MediaPipe registry (Leap) is untouched: no suspend marker, no
    /// stop.
    #[test]
    fn leap_registry_is_untouched() {
        use bevy::ecs::system::RunSystemOnce;
        let (registry, _starts, stops) = registry_with(ProviderId::Leap);
        let mut world = World::new();
        world.insert_resource(registry);
        world
            .run_system_once(suspend_mediapipe_hand_camera)
            .expect("suspend");
        assert_eq!(stops.load(Ordering::SeqCst), 0);
        assert!(world.get_resource::<SuspendedHandCamera>().is_none());
    }

    /// No registry at all (headless): both systems are clean no-ops.
    #[test]
    fn missing_registry_is_a_no_op() {
        use bevy::ecs::system::RunSystemOnce;
        let mut world = World::new();
        world.insert_resource(Time::<()>::default());
        world
            .run_system_once(suspend_mediapipe_hand_camera)
            .expect("suspend");
        world
            .run_system_once(resume_hand_camera_when_due)
            .expect("resume");
        assert!(world.get_resource::<SuspendedHandCamera>().is_none());
    }
}
```

Add `pub mod arbitration;` to `systems/mod.rs`.

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p wc-sketches radiance::systems`
Expected: PASS (spawn + arbitration + sim_params suites). If `Messages` in the stub's `poll` signature does not resolve from the prelude, mirror the exact import used by the `ServiceStub` test in `crates/waveconductor/src/hand_providers.rs`.

- [ ] **Step 4: Commit**

```bash
git add crates/wc-sketches/src/radiance
git commit -m "feat(radiance): spawn and teardown, activation requests, hand-camera arbitration" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 10: Activity sync + full lifecycle wiring in `RadiancePlugin::build`

**Files:**
- Create: `crates/wc-sketches/src/radiance/systems/activity.rs`
- Modify: `crates/wc-sketches/src/radiance/systems/mod.rs` (add `pub mod activity;`)
- Modify: `crates/wc-sketches/src/radiance/mod.rs` (wire everything built so far)

**Interfaces:**
- Consumes: everything from Tasks 2–9; `wc_core::sketch::{sketch_active, despawn_with, reset_render_profile}`; `SketchActivity`.
- Produces: `pause_tracking_requests`, `resume_tracking_requests`; the assembled plugin (screensaver + debug arrive in Tasks 12–13).

- [ ] **Step 1: Write `systems/activity.rs` (complete)**

```rust
//! `SketchActivity` → activation-request sync.
//!
//! The Plan A/B request resources carry live pause knobs (`paused` on the
//! audio capture, `idle_throttle` on body tracking). Radiance flips both on
//! the activity seams: Idle and Screensaver pause the mic analysis (the
//! attract mode is not audio-reactive) and drop the body worker to its
//! detector-only idle rate (so a person walking up still re-activates the
//! sketch via the presence → InteractionTimer path Plan B owns); Active
//! restores both. Registered on `OnEnter` of each activity, gated
//! `in_state(AppState::Radiance)` — zero per-frame cost.

use bevy::prelude::*;
use wc_core::audio::input::AudioCaptureRequest;
use wc_core::input::body::BodyTrackingRequest;

/// `OnEnter(SketchActivity::Idle)` / `OnEnter(SketchActivity::Screensaver)`:
/// pause capture, throttle tracking. Both resources are optional — the
/// synthetic capture path never inserts them.
pub fn pause_tracking_requests(
    mut audio: Option<ResMut<'_, AudioCaptureRequest>>,
    mut body: Option<ResMut<'_, BodyTrackingRequest>>,
) {
    if let Some(audio) = audio.as_mut() {
        if !audio.paused {
            audio.paused = true;
        }
    }
    if let Some(body) = body.as_mut() {
        if !body.idle_throttle {
            body.idle_throttle = true;
        }
    }
}

/// `OnEnter(SketchActivity::Active)`: resume capture + full-rate tracking.
pub fn resume_tracking_requests(
    mut audio: Option<ResMut<'_, AudioCaptureRequest>>,
    mut body: Option<ResMut<'_, BodyTrackingRequest>>,
) {
    if let Some(audio) = audio.as_mut() {
        if audio.paused {
            audio.paused = false;
        }
    }
    if let Some(body) = body.as_mut() {
        if body.idle_throttle {
            body.idle_throttle = false;
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    #[test]
    fn pause_and_resume_flip_both_requests() {
        let mut world = World::new();
        world.insert_resource(AudioCaptureRequest {
            device_name: None,
            paused: false,
        });
        world.insert_resource(BodyTrackingRequest {
            idle_throttle: false,
            mask_ema: 0.6,
            one_euro_min_cutoff: 1.0,
            one_euro_beta: 0.05,
        });
        world
            .run_system_once(pause_tracking_requests)
            .expect("pause");
        assert!(world.resource::<AudioCaptureRequest>().paused);
        assert!(world.resource::<BodyTrackingRequest>().idle_throttle);
        world
            .run_system_once(resume_tracking_requests)
            .expect("resume");
        assert!(!world.resource::<AudioCaptureRequest>().paused);
        assert!(!world.resource::<BodyTrackingRequest>().idle_throttle);
    }

    #[test]
    fn absent_requests_are_a_no_op() {
        let mut world = World::new();
        world
            .run_system_once(pause_tracking_requests)
            .expect("pause");
        world
            .run_system_once(resume_tracking_requests)
            .expect("resume");
    }
}
```

- [ ] **Step 2: Assemble `RadiancePlugin::build` (replace the Task 2 body with the full wiring; complete)**

```rust
impl Plugin for RadiancePlugin {
    // The registration list is the single source of truth for wiring order
    // (flame's convention); splitting it would scatter it.
    #[allow(clippy::too_many_lines)]
    fn build(&self, app: &mut App) {
        // ── Signal / data flow ─────────────────────────────────────────
        // OnEnter: suspend the MediaPipe *hand* camera → ensure the mask +
        // edge surfaces exist → spawn buffers/quads + sim resources → insert
        // the mic + body activation requests. Per frame while Active:
        // update_radiance_sim bakes AudioAnalysis + BodyTrackingState +
        // SilhouetteEdges into RadianceSimParams; the render world extracts
        // it, uploads edges generation-gated, and dispatches the aura kernel
        // before the 2D pass draws the billboards over the silhouette quad.
        // drive_radiance_materials runs through Idle/Screensaver (in_state)
        // so the ember blend keeps rendering. Activity seams pause/resume
        // the requests; Idle zeroes emission; OnExit tears everything down
        // and schedules the deferred hand-camera restore.

        app.register_sketch_settings::<settings::RadianceSettings>();
        register_radiance_manifest(app);

        // OnEnter chain: arbitration first (release the webcam), then
        // surfaces, then spawn (reads MaskTexture), then requests.
        app.add_systems(
            OnEnter(AppState::Radiance),
            (
                systems::arbitration::suspend_mediapipe_hand_camera,
                systems::spawn::ensure_body_surfaces,
                systems::spawn::spawn_radiance,
                systems::spawn::insert_tracking_requests,
            )
                .chain(),
        );
        app.add_systems(
            OnExit(AppState::Radiance),
            (
                wc_core::sketch::despawn_with::<systems::spawn::RadianceRoot>,
                systems::spawn::remove_radiance_resources,
                systems::arbitration::schedule_hand_camera_restore,
                wc_core::sketch::reset_render_profile,
            ),
        );
        // Deferred hand-camera restore: always-on, one Option branch when
        // idle (see its module docs for the sanctioned-listener rationale).
        app.add_systems(Update, systems::arbitration::resume_hand_camera_when_due);

        // Live writer: the per-frame baker, Active only.
        app.add_systems(
            Update,
            systems::sim_params::update_radiance_sim
                .run_if(wc_core::sketch::sketch_active(AppState::Radiance)),
        );

        // Idle freeze: zero emission so the aura fades out and the throttled
        // last frames hold (flame's freeze idiom).
        app.add_systems(
            OnEnter(wc_core::lifecycle::state::SketchActivity::Idle),
            (
                systems::sim_params::freeze_radiance_emission,
                systems::activity::pause_tracking_requests,
            )
                .run_if(in_state(AppState::Radiance)),
        );
        app.add_systems(
            OnEnter(wc_core::lifecycle::state::SketchActivity::Screensaver),
            systems::activity::pause_tracking_requests.run_if(in_state(AppState::Radiance)),
        );
        app.add_systems(
            OnEnter(wc_core::lifecycle::state::SketchActivity::Active),
            systems::activity::resume_tracking_requests.run_if(in_state(AppState::Radiance)),
        );

        // Material driver: runs through Idle and the screensaver (in_state,
        // flame's drive_flame_material gating) so the ember blend and held
        // envelopes keep rendering.
        app.add_systems(
            Update,
            render::drive_radiance_materials.run_if(in_state(AppState::Radiance)),
        );

        // Shared lifecycle glue (sanctioned always-on listeners + profile).
        app.add_systems(
            Update,
            wc_core::sketch::restart_on_settings_change::<settings::RadianceSettings>,
        );
        app.add_message::<wc_core::lifecycle::window_resize::WindowResizeSettled>();
        app.add_systems(
            Update,
            wc_core::sketch::reload_on_resize_settled::<settings::RadianceSettings>,
        );
        app.add_systems(
            Update,
            wc_core::sketch::apply_render_profile::<settings::RadianceSettings>
                .run_if(in_state(AppState::Radiance)),
        );
    }
}
```

Also update the module doc at the top of `mod.rs`: replace the "later tasks add" list with the actual data-flow summary (mirror the build comment above in `//!` prose).

- [ ] **Step 3: Run tests + wiring check**

Run: `cargo nextest run -p wc-sketches radiance:: && cargo check -p wc-sketches`
Expected: PASS / clean.

- [ ] **Step 4: Commit**

```bash
git add crates/wc-sketches/src/radiance
git commit -m "feat(radiance): activity sync and full lifecycle wiring" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 11: Synthetic body performer (`synthetic.rs`)

**Files:**
- Create: `crates/wc-sketches/src/radiance/synthetic.rs`
- Modify: `crates/wc-sketches/src/radiance/mod.rs` (add `pub mod synthetic;`)

**Interfaces:**
- Consumes: pinned `EdgePoint`, `MAX_EDGE_POINTS`, `MASK_SIZE`, `AudioAnalysis`.
- Produces: `Ellipse`, `PhantomPose`, `phantom_pose(t)`, `dancing_pose(t)`, `rasterize_mask`, `extract_edges`, `synthetic_audio(t)`, `DANCER_LANDMARK_UV` — shared by unit tests, the Task 12 phantom, and the Task 13 capture driver. This module is deterministic in `t` and allocation-free after its buffers exist (callers own the scratch).

- [ ] **Step 1: Write the module (complete)**

```rust
//! Deterministic synthetic body performer.
//!
//! One generator, three consumers (the spec's testing keystone):
//!
//! - unit tests (mask/edge math, below);
//! - the attract-mode phantom (`screensaver.rs`) — a slow drifting ellipse
//!   cluster;
//! - the capture scenarios' dancer (`systems/debug.rs`) — the same cluster
//!   with larger, faster limb swings plus synthetic landmarks/audio.
//!
//! Everything is a pure function of `t` (virtual seconds), so fixed-dt
//! captures are reproducible frame-for-frame. Mask space is the pinned
//! contract's: 256×256 `R8Unorm`, UV origin top-left, y down.

use bevy::prelude::*;
use wc_core::audio::input::AudioAnalysis;
use wc_core::input::body::{EdgePoint, MASK_SIZE, MAX_EDGE_POINTS};

/// One soft ellipse blob in mask-UV space.
#[derive(Clone, Copy, Debug)]
pub struct Ellipse {
    /// Center in mask UV (0..1, y down).
    pub center: Vec2,
    /// Semi-axes in mask UV.
    pub radii: Vec2,
}

/// A phantom body: six blobs (head, torso, two arms, two legs).
#[derive(Clone, Copy, Debug)]
pub struct PhantomPose {
    /// Blob cluster; the union rasterizes into the silhouette.
    pub blobs: [Ellipse; 6],
}

/// Blob indices (documented so limb landmarks can anchor to them).
pub const BLOB_HEAD: usize = 0;
/// See [`BLOB_HEAD`].
pub const BLOB_TORSO: usize = 1;
/// See [`BLOB_HEAD`].
pub const BLOB_ARM_L: usize = 2;
/// See [`BLOB_HEAD`].
pub const BLOB_ARM_R: usize = 3;
/// See [`BLOB_HEAD`].
pub const BLOB_LEG_L: usize = 4;
/// See [`BLOB_HEAD`].
pub const BLOB_LEG_R: usize = 5;

/// Build the pose at time `t` with the given sway/limb amplitudes.
/// `sway_amp` ~0.05 reads as an idle drift; `limb_amp` ~0.09 as dancing.
#[must_use]
fn pose_at(t: f32, sway_amp: f32, limb_amp: f32) -> PhantomPose {
    let sway = (t * 0.35).sin() * sway_amp;
    let bob = (t * 0.9).sin() * 0.015;
    let cx = 0.5 + sway;
    let arm_l_swing = (t * 0.8).sin() * limb_amp;
    let arm_r_swing = (t * 0.8 + 2.1).sin() * limb_amp;
    let leg_shift = (t * 0.5).sin() * limb_amp * 0.4;
    PhantomPose {
        blobs: [
            // Head.
            Ellipse {
                center: Vec2::new(cx + sway * 0.4, 0.30 + bob),
                radii: Vec2::new(0.055, 0.065),
            },
            // Torso.
            Ellipse {
                center: Vec2::new(cx, 0.52 + bob),
                radii: Vec2::new(0.09, 0.16),
            },
            // Arms (vertical-ish blobs swinging outward from the shoulders).
            Ellipse {
                center: Vec2::new(cx - 0.13 - arm_l_swing.abs(), 0.46 + arm_l_swing),
                radii: Vec2::new(0.035, 0.11),
            },
            Ellipse {
                center: Vec2::new(cx + 0.13 + arm_r_swing.abs(), 0.46 + arm_r_swing),
                radii: Vec2::new(0.035, 0.11),
            },
            // Legs.
            Ellipse {
                center: Vec2::new(cx - 0.05 + leg_shift, 0.76 + bob),
                radii: Vec2::new(0.045, 0.14),
            },
            Ellipse {
                center: Vec2::new(cx + 0.05 - leg_shift, 0.76 + bob),
                radii: Vec2::new(0.045, 0.14),
            },
        ],
    }
}

/// The attract phantom: slow drift, small limb motion.
#[must_use]
pub fn phantom_pose(t: f32) -> PhantomPose {
    pose_at(t, 0.05, 0.03)
}

/// The capture dancer: bigger sway and limb swings (still deterministic).
#[must_use]
pub fn dancing_pose(t: f32) -> PhantomPose {
    pose_at(t * 1.6, 0.08, 0.09)
}

/// Approximate landmark UVs for the seven impulse landmarks (nose, wrists,
/// hips, ankles), anchored to the pose's blobs. Order matches
/// `systems::sim_params::IMPULSE_LANDMARKS`.
#[must_use]
pub fn dancer_landmark_uv(pose: &PhantomPose) -> [Vec2; 7] {
    let head = pose.blobs[BLOB_HEAD].center;
    let arm_l = pose.blobs[BLOB_ARM_L];
    let arm_r = pose.blobs[BLOB_ARM_R];
    let torso = pose.blobs[BLOB_TORSO];
    let leg_l = pose.blobs[BLOB_LEG_L];
    let leg_r = pose.blobs[BLOB_LEG_R];
    [
        head,                                             // nose
        arm_l.center + Vec2::new(0.0, arm_l.radii.y),     // left wrist (arm tip)
        arm_r.center + Vec2::new(0.0, arm_r.radii.y),     // right wrist
        torso.center + Vec2::new(-torso.radii.x, 0.10),   // left hip
        torso.center + Vec2::new(torso.radii.x, 0.10),    // right hip
        leg_l.center + Vec2::new(0.0, leg_l.radii.y),     // left ankle
        leg_r.center + Vec2::new(0.0, leg_r.radii.y),     // right ankle
    ]
}

/// Rasterize the pose's smooth-union coverage into a `MASK_SIZE²` byte
/// buffer (255 inside, 0 outside, a soft band at the boundary — matching the
/// EMA-softened real mask). `out.len()` must be `MASK_SIZE * MASK_SIZE`.
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "pixel-loop index/value conversions on bounded 0..256 / 0..1 values"
)]
pub fn rasterize_mask(pose: &PhantomPose, out: &mut [u8]) {
    debug_assert_eq!(out.len(), MASK_SIZE * MASK_SIZE);
    let inv = 1.0 / MASK_SIZE as f32;
    for y in 0..MASK_SIZE {
        let v = (y as f32 + 0.5) * inv;
        for x in 0..MASK_SIZE {
            let u = (x as f32 + 0.5) * inv;
            let p = Vec2::new(u, v);
            // Max coverage over blobs; each blob's normalized squared field
            // f = |(p-c)/r|² crosses 1 at the boundary; a smoothstep band
            // (0.85..1.15) softens it.
            let mut cov = 0.0_f32;
            for blob in &pose.blobs {
                let q = (p - blob.center) / blob.radii;
                let f = q.length_squared();
                let c = 1.0 - smoothstep(0.85, 1.15, f);
                cov = cov.max(c);
            }
            out[y * MASK_SIZE + x] = (cov * 255.0) as u8;
        }
    }
}

/// Scalar smoothstep (WGSL semantics).
#[must_use]
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Threshold used by [`extract_edges`] — the byte form of the contract's 0.5
/// mask crossing.
pub const EDGE_THRESHOLD: u8 = 128;

/// Extract up to [`MAX_EDGE_POINTS`] `(position, outward normal)` pairs where
/// the mask crosses [`EDGE_THRESHOLD`], into `out` (cleared first; capacity
/// is reused, never grown past the cap). Same single-pass scan shape as Plan
/// B's worker-side extractor: an inside pixel with any 4-neighbor outside is
/// a boundary pixel; the outward normal is the negated central-difference
/// gradient (the mask is high inside, so the gradient points inward).
#[allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "pixel index -> UV conversion on bounded 0..256 values"
)]
pub fn extract_edges(mask: &[u8], out: &mut Vec<EdgePoint>) {
    debug_assert_eq!(mask.len(), MASK_SIZE * MASK_SIZE);
    out.clear();
    let inv = 1.0 / MASK_SIZE as f32;
    let at = |x: usize, y: usize| mask[y * MASK_SIZE + x];
    for y in 1..MASK_SIZE - 1 {
        for x in 1..MASK_SIZE - 1 {
            if at(x, y) < EDGE_THRESHOLD {
                continue;
            }
            let inside = |v: u8| v >= EDGE_THRESHOLD;
            let boundary = !inside(at(x - 1, y))
                || !inside(at(x + 1, y))
                || !inside(at(x, y - 1))
                || !inside(at(x, y + 1));
            if !boundary {
                continue;
            }
            // Central-difference gradient (points toward higher = inward).
            let gx = f32::from(at(x + 1, y)) - f32::from(at(x - 1, y));
            let gy = f32::from(at(x, y + 1)) - f32::from(at(x, y - 1));
            let g = Vec2::new(gx, gy);
            let len = g.length();
            if len < 1e-3 {
                continue; // flat plateau artifact; no meaningful normal
            }
            let normal = -g / len;
            out.push(EdgePoint {
                pos: [(x as f32 + 0.5) * inv, (y as f32 + 0.5) * inv],
                normal: [normal.x, normal.y],
            });
            if out.len() >= MAX_EDGE_POINTS {
                return;
            }
        }
    }
}

/// Deterministic synthetic analysis frame for the capture dancer: a slow
/// bass swell, a high-band shimmer, and a 2 Hz onset "beat".
#[must_use]
pub fn synthetic_audio(t: f32) -> AudioAnalysis {
    let bass = 0.5 + 0.4 * (t * 1.1).sin();
    let high = 0.35 + 0.3 * (t * 3.7).sin();
    // A short raised-cosine click twice a second.
    let beat_phase = (t * 2.0).fract();
    let onset = if beat_phase < 0.08 {
        1.2 * (1.0 - beat_phase / 0.08)
    } else {
        0.0
    };
    AudioAnalysis {
        rms: 0.35 + 0.25 * bass,
        gain: 1.0,
        bands: [
            bass, bass * 0.8, bass * 0.6, 0.3, 0.3, high * 0.8, high, high * 0.9,
        ],
        onset,
        beat_confidence: 0.8,
        active: true,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;

    /// Same `t` → bit-identical mask (the determinism captures depend on).
    #[test]
    fn rasterize_is_deterministic() {
        let pose = dancing_pose(3.25);
        let mut a = vec![0u8; MASK_SIZE * MASK_SIZE];
        let mut b = vec![0u8; MASK_SIZE * MASK_SIZE];
        rasterize_mask(&pose, &mut a);
        rasterize_mask(&pose, &mut b);
        assert_eq!(a, b);
        assert!(a.iter().any(|&v| v >= EDGE_THRESHOLD), "body present");
        assert!(a.iter().any(|&v| v == 0), "background present");
    }

    /// A single centered circle: edge count ≈ its pixel circumference, every
    /// normal points away from the center, all positions in the edge band.
    #[test]
    fn circle_edges_point_outward() {
        let pose = PhantomPose {
            blobs: [Ellipse {
                center: Vec2::new(0.5, 0.5),
                radii: Vec2::new(0.2, 0.2),
            }; 6],
        };
        let mut mask = vec![0u8; MASK_SIZE * MASK_SIZE];
        rasterize_mask(&pose, &mut mask);
        let mut edges = Vec::with_capacity(MAX_EDGE_POINTS);
        extract_edges(&mask, &mut edges);
        // r = 0.2 * 256 ≈ 51 px → circumference ≈ 322 px of boundary.
        assert!(
            edges.len() > 200 && edges.len() < 800,
            "got {}",
            edges.len()
        );
        for e in &edges {
            let pos = Vec2::new(e.pos[0], e.pos[1]);
            let n = Vec2::new(e.normal[0], e.normal[1]);
            assert!((n.length() - 1.0).abs() < 1e-3, "unit normal");
            assert!(
                (pos - Vec2::new(0.5, 0.5)).dot(n) > 0.0,
                "outward at {pos:?} n {n:?}"
            );
            let r = (pos - Vec2::new(0.5, 0.5)).length();
            assert!((r - 0.2).abs() < 0.03, "on the rim: r = {r}");
        }
    }

    /// A stripe pattern with far more boundary pixels than the cap clamps to
    /// exactly `MAX_EDGE_POINTS`.
    #[test]
    fn extraction_clamps_to_capacity() {
        let mut mask = vec![0u8; MASK_SIZE * MASK_SIZE];
        for y in 0..MASK_SIZE {
            for x in 0..MASK_SIZE {
                // 4-px stripes with soft 1-px ramps so gradients are nonzero.
                let phase = x % 8;
                mask[y * MASK_SIZE + x] = match phase {
                    0 => 64,
                    1..=3 => 255,
                    4 => 64,
                    _ => 0,
                };
            }
        }
        let mut edges = Vec::with_capacity(MAX_EDGE_POINTS);
        extract_edges(&mask, &mut edges);
        assert_eq!(edges.len(), MAX_EDGE_POINTS);
    }

    /// The dancer's landmarks stay inside the mask frame and move over time
    /// (finite differences are nonzero → real impulse velocities).
    #[test]
    fn dancer_landmarks_move_in_bounds() {
        let a = dancer_landmark_uv(&dancing_pose(1.0));
        let b = dancer_landmark_uv(&dancing_pose(1.1));
        let mut moved = 0;
        for (pa, pb) in a.iter().zip(&b) {
            assert!(pa.x > 0.0 && pa.x < 1.0 && pa.y > 0.0 && pa.y < 1.0);
            if pa.distance(*pb) > 1e-4 {
                moved += 1;
            }
        }
        assert!(moved >= 4, "limbs must actually dance ({moved} moved)");
    }

    /// Synthetic audio is deterministic and periodically produces onsets.
    #[test]
    fn synthetic_audio_is_deterministic_with_beats() {
        assert_eq!(synthetic_audio(2.0), synthetic_audio(2.0));
        let on_beat = synthetic_audio(1.0); // beat_phase 0 → onset peak
        assert!(on_beat.onset > 1.0);
        let off_beat = synthetic_audio(1.25);
        assert!(off_beat.onset.abs() < f32::EPSILON);
        assert!(on_beat.active);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo nextest run -p wc-sketches radiance::synthetic`
Expected: PASS (5 tests). The circle-count bounds and rim tolerance are the only heuristic assertions — if one trips, print the actual value and adjust the *band*, not the extractor, unless normals/rim are actually wrong.

- [ ] **Step 3: Commit**

```bash
git add crates/wc-sketches/src/radiance
git commit -m "feat(radiance): deterministic synthetic body performer" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 12: Screensaver — phantom performer + attract sim writer

**Files:**
- Create: `crates/wc-sketches/src/radiance/screensaver.rs`
- Modify: `crates/wc-sketches/src/radiance/mod.rs` (add `pub mod screensaver;` + `app.add_plugins(screensaver::RadianceScreensaverPlugin);` at the end of `build`)

**Interfaces:**
- Consumes: Task 11 generator; Task 5 baker (`bake_radiance_sim`, `neutral_audio`) — one baker, two writers; `in_screensaver(AppState::Radiance)`, `ScreensaverFade`; pinned `MaskTexture`/`SilhouetteEdges`.
- Produces: `RadianceScreensaverPlugin`, `PhantomClock`, `drive_phantom`, `drive_radiance_attract_sim`.

- [ ] **Step 1: Write `screensaver.rs` (complete)**

```rust
//! Radiance attract-mode performer.
//!
//! Two drivers, both gated `in_screensaver(AppState::Radiance)` (zero systems
//! otherwise — AGENTS.md "zero systems when idle"), running under the
//! established thermal present-rate throttle:
//!
//! - [`drive_phantom`]: an analytic SDF silhouette (the synthetic module's
//!   drifting ellipse cluster) writes a synthetic mask + edge list through
//!   the SAME `MaskTexture` / `SilhouetteEdges` resources the real tracker
//!   uses, so the particle kernel and silhouette material are unchanged.
//!   Rasterization is rate-limited to [`PHANTOM_REGEN_HZ`]; between regens
//!   the phantom costs one accumulator add.
//! - [`drive_radiance_attract_sim`]: the screensaver's [`bake_radiance_sim`]
//!   writer (one baker, two writers — flame's Condition A1). No audio (the
//!   attract mode is not audio-reactive: it bakes the neutral frame), no
//!   impulses, and ember overrides scale emission/flow/buoyancy down on the
//!   `ScreensaverFade` envelope so sleep and wake ease symmetrically. The
//!   ember *palette* blend lives in `render::drive_radiance_materials`
//!   (already gated `in_state`, so it runs through the screensaver).
//!
//! During the screensaver the camera stays at Plan B's detector-only idle
//! rate (the activity sync set `idle_throttle`), so a person walking up
//! resets the `InteractionTimer` and wakes the sketch; the worker's mask
//! writes resume only once a person is actually present, which is also the
//! moment the phantom stops running.

use bevy::prelude::*;
use wc_core::input::body::{MaskTexture, SilhouetteEdges};
use wc_core::lifecycle::screensaver::fade::ScreensaverFade;
use wc_core::lifecycle::screensaver::in_screensaver;
use wc_core::lifecycle::state::AppState;

use crate::radiance::compute::sim_params::RadianceSimParams;
use crate::radiance::settings::RadianceSettings;
use crate::radiance::synthetic::{extract_edges, phantom_pose, rasterize_mask};
use crate::radiance::systems::sim_params::{bake_radiance_sim, neutral_audio, RadianceState};

/// Phantom mask regeneration rate. 12 Hz reads as continuous drift at the
/// screensaver's throttled present rates while keeping the 256² rasterize
/// well under the thermal budget.
pub const PHANTOM_REGEN_HZ: f32 = 12.0;
/// Fraction of live emission at full fade (the "low particle count" of the
/// spec's compute-lite attract mode — fewer births, thinner aura).
pub const EMBER_EMISSION_FRACTION: f32 = 0.25;
/// Flow-strength multiplier at full fade (slow drift).
pub const EMBER_FLOW_FRACTION: f32 = 0.4;
/// Buoyancy multiplier at full fade.
pub const EMBER_BUOYANCY_FRACTION: f32 = 0.6;
/// Phantom time scale: the pose clock runs slower than wall time.
pub const PHANTOM_TIME_SCALE: f32 = 0.6;

/// Phantom driver state: pose clock + regen accumulator.
#[derive(Resource, Default)]
pub struct PhantomClock {
    /// Seconds of screensaver time (drives the pose).
    pub elapsed: f32,
    /// Seconds since the last mask regen.
    pub since_regen: f32,
}

/// Plugin wiring the Radiance attract performer.
pub struct RadianceScreensaverPlugin;

impl Plugin for RadianceScreensaverPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PhantomClock>();
        app.add_systems(
            Update,
            (drive_phantom, drive_radiance_attract_sim)
                .chain()
                .run_if(in_screensaver(AppState::Radiance)),
        );
    }
}

/// `Update` (`in_screensaver(AppState::Radiance)`): advance the phantom and,
/// at [`PHANTOM_REGEN_HZ`], rewrite the shared mask + edge list in place
/// (no allocation: the image bytes and the edge Vec are reused).
pub fn drive_phantom(
    time: Res<'_, Time>,
    mut clock: ResMut<'_, PhantomClock>,
    mask: Option<Res<'_, MaskTexture>>,
    mut images: ResMut<'_, Assets<Image>>,
    edges: Option<ResMut<'_, SilhouetteEdges>>,
) {
    clock.elapsed += time.delta_secs();
    clock.since_regen += time.delta_secs();
    if clock.since_regen < 1.0 / PHANTOM_REGEN_HZ {
        return;
    }
    clock.since_regen = 0.0;

    let (Some(mask), Some(mut edges)) = (mask, edges) else {
        return; // surfaces absent (headless harness): nothing to draw into
    };
    let pose = phantom_pose(clock.elapsed * PHANTOM_TIME_SCALE);
    if let Some(image) = images.get_mut(&mask.0) {
        if let Some(data) = image.data.as_mut() {
            rasterize_mask(&pose, data);
            extract_edges(data, &mut edges.points);
            edges.generation = edges.generation.wrapping_add(1);
        }
    }
}

/// `Update` (`in_screensaver(AppState::Radiance)`, after [`drive_phantom`]):
/// bake the neutral-audio, no-body frame, then apply the ember overrides on
/// the fade envelope.
pub fn drive_radiance_attract_sim(
    time: Res<'_, Time>,
    window: Single<'_, '_, &Window>,
    settings: Res<'_, RadianceSettings>,
    fade: Res<'_, ScreensaverFade>,
    edges: Option<Res<'_, SilhouetteEdges>>,
    mut state: ResMut<'_, RadianceState>,
    mut sim: ResMut<'_, RadianceSimParams>,
) {
    let edge_count = edges.map_or(0, |e| e.points.len());
    let window_size = Vec2::new(window.width(), window.height());
    let quiet = neutral_audio();
    bake_radiance_sim(
        &settings,
        &quiet,
        None,
        edge_count,
        window_size,
        time.delta_secs(),
        time.elapsed_secs(),
        &mut state,
        &mut sim.params,
    );
    // Ember overrides ride the fade in both directions, so the decay into
    // the ember and the roar-back on wake are symmetric.
    let a = fade.alpha().clamp(0.0, 1.0);
    sim.params.emission_prob *= 1.0 - a * (1.0 - EMBER_EMISSION_FRACTION);
    sim.params.flow_strength *= 1.0 - a * (1.0 - EMBER_FLOW_FRACTION);
    sim.params.buoyancy *= 1.0 - a * (1.0 - EMBER_BUOYANCY_FRACTION);
    sim.params.burst_speed = 0.0;
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use bevy::asset::AssetPlugin;
    use bevy::ecs::system::RunSystemOnce;
    use bytemuck::Zeroable;
    use std::time::Duration;

    use crate::radiance::compute::sim_params::RadianceSimParamsGpu;
    use crate::radiance::systems::spawn::ensure_body_surfaces;

    fn phantom_app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        app.init_asset::<Image>();
        app.insert_resource(RadianceSettings::default());
        app.init_resource::<PhantomClock>();
        let mut time = Time::<()>::default();
        time.advance_by(Duration::from_millis(200)); // past the regen period
        app.insert_resource(time);
        app.world_mut()
            .run_system_once(ensure_body_surfaces)
            .expect("surfaces");
        app
    }

    /// The phantom writes a real mask and a fresh edge list, bumping the
    /// generation so the GPU upload path sees it.
    #[test]
    fn phantom_writes_mask_and_edges() {
        let mut app = phantom_app();
        let gen_before = app.world().resource::<SilhouetteEdges>().generation;
        app.world_mut()
            .run_system_once(drive_phantom)
            .expect("phantom runs");
        let edges = app.world().resource::<SilhouetteEdges>();
        assert!(edges.generation != gen_before, "generation bumped");
        assert!(!edges.points.is_empty(), "phantom has a rim");
        let mask = app.world().resource::<MaskTexture>().0.clone();
        let images = app.world().resource::<Assets<Image>>();
        let data = images
            .get(&mask)
            .and_then(|i| i.data.as_ref())
            .expect("mask bytes");
        assert!(data.iter().any(|&v| v > 128), "phantom body rasterized");
    }

    /// At full fade the attract writer emits less, flows slower, and rises
    /// less than the live bake; burst is zeroed.
    #[test]
    fn attract_writer_applies_ember_overrides() {
        let mut world = World::new();
        world.insert_resource(RadianceSettings::default());
        world.insert_resource(RadianceState::default());
        world.insert_resource(RadianceSimParams {
            params: RadianceSimParamsGpu::zeroed(),
            particles: Handle::default(),
            particle_count: 1_000,
        });
        world.insert_resource(SilhouetteEdges {
            points: Vec::with_capacity(8),
            generation: 1,
        });
        let mut fade = ScreensaverFade::default();
        fade.set_target(1.0);
        let fade = fade.advanced(Duration::from_secs(10));
        world.insert_resource(fade);
        world.insert_resource(Time::<()>::default());
        world.spawn(Window::default());

        world
            .run_system_once(drive_radiance_attract_sim)
            .expect("attract writer runs");
        let sim = world.resource::<RadianceSimParams>();
        // Live neutral value: rate(0.5) * EMISSION_BASE_HZ * dt; ember cuts
        // it to the fraction.
        let live = 0.5 * crate::radiance::systems::sim_params::EMISSION_BASE_HZ
            * sim.params.dt;
        assert!(
            (sim.params.emission_prob - live * EMBER_EMISSION_FRACTION).abs() < 1e-6,
            "ember emission: {} vs live {live}",
            sim.params.emission_prob
        );
        assert!(sim.params.burst_speed.abs() < f32::EPSILON);
        assert!(sim.params.impulse_count == 0, "no body in attract mode");
    }
}
```

- [ ] **Step 2: Wire into `RadiancePlugin::build`** (append at the end):

```rust
        // Attract performer: phantom silhouette + ember sim writer, both
        // gated in_screensaver (zero systems otherwise).
        app.add_plugins(screensaver::RadianceScreensaverPlugin);
```

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p wc-sketches radiance::screensaver`
Expected: PASS (2 tests). Note: `drive_radiance_attract_sim` uses `Time::default()` (dt 0) in the second test — `bake_radiance_sim` caps dt but never divides by it, and the emission expectation multiplies by the same `sim.params.dt`, so the assertion holds at dt 0.

- [ ] **Step 4: Commit**

```bash
git add crates/wc-sketches/src/radiance
git commit -m "feat(radiance): screensaver phantom performer and ember attract writer" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 13: Capture/debug: `WC_DEBUG_FORCE_RADIANCE_SYNTHETIC_BODY` + dev overlays

**Files:**
- Modify: `crates/wc-core/src/debug/mod.rs` (new field ~line 78, `from_env_vars` ~line 108, docs)
- Modify: `crates/wc-core/src/capture/system.rs` (`toggles_json` ~line 236 + its exhaustive test literal ~line 379)
- Modify: exhaustive `DebugToggles` test literals: `crates/wc-core/src/lifecycle/screensaver/mod.rs` ~line 594, `crates/wc-sketches/src/dots/mod.rs` ~lines 440/458, `crates/wc-sketches/src/line/mod.rs` ~lines 548/564 (add `force_radiance_synthetic_body: false,` to each — the compiler enumerates them; the DebugToggles new-field checklist)
- Create: `crates/wc-sketches/src/radiance/systems/debug.rs`
- Modify: `crates/wc-sketches/src/radiance/systems/mod.rs`, `crates/wc-sketches/src/radiance/mod.rs` (wiring)
- Modify: `tests/visual/CLAUDE.md` (toggle table row — done here so the doc and code land together)

**Interfaces:**
- Consumes: `DebugToggles` (debug-builds-only resource, absent = all off); Task 11 generator; pinned `BodyTrackingState`/`BodyLandmark`/`BODY_LANDMARK_COUNT`, `AudioAnalysis`; `bevy_egui::EguiContexts` + `EguiPrimaryContextPass` (flame's ui.rs idiom); `Gizmos`.
- Produces: `DebugToggles::force_radiance_synthetic_body`; `drive_synthetic_body` (capture dancer), `synthetic_body_forced` run condition; `draw_edge_debug`; `radiance_inference_readout`.

- [ ] **Step 1: Add the toggle to wc-core**

In `crates/wc-core/src/debug/mod.rs`, append to the struct:

```rust
    /// `WC_DEBUG_FORCE_RADIANCE_SYNTHETIC_BODY`: drive Radiance from the
    /// deterministic synthetic dancer (mask + edges + landmarks + audio)
    /// instead of the mic/camera pipelines, and suppress the
    /// `AudioCaptureRequest`/`BodyTrackingRequest` inserts so a capture run
    /// never opens hardware. Presence = on.
    pub force_radiance_synthetic_body: bool,
```

in `from_env_vars`:

```rust
            force_radiance_synthetic_body: present("WC_DEBUG_FORCE_RADIANCE_SYNTHETIC_BODY"),
```

In `crates/wc-core/src/capture/system.rs`, `toggles_json` gains (alongside the other flag branches):

```rust
    if t.force_radiance_synthetic_body {
        parts.push("\"force_radiance_synthetic_body\":true".to_string());
    }
```

Then run `cargo check -p wc-core -p wc-sketches` and add `force_radiance_synthetic_body: false,` to every exhaustive `DebugToggles { .. }` literal the compiler reports (the five test sites listed above, plus any added since). Extend the `from_env_vars` test in `debug/mod.rs`:

```rust
    #[test]
    fn radiance_synthetic_body_flag_parses_by_presence() {
        let vars = vec![(
            "WC_DEBUG_FORCE_RADIANCE_SYNTHETIC_BODY".to_string(),
            String::new(),
        )];
        let t = DebugToggles::from_env_vars(&vars);
        assert!(t.force_radiance_synthetic_body);
        assert!(!DebugToggles::from_env_vars(&[]).force_radiance_synthetic_body);
    }
```

- [ ] **Step 2: Write `systems/debug.rs` (complete)**

```rust
//! Radiance dev/debug drivers: the synthetic capture dancer (debug builds),
//! the edge-point gizmo overlay, and the inference readout.
//!
//! The egui readout is registered in `EguiPrimaryContextPass` and self-gates
//! (flame's ui.rs idiom); the gizmo overlay runs `sketch_active` and
//! early-outs on the settings bool. The synthetic dancer runs only under
//! `WC_DEBUG_FORCE_RADIANCE_SYNTHETIC_BODY` in debug builds — it overwrites
//! the mask/edges/body-state/audio resources with deterministic
//! virtual-time data so `cargo xtask capture radiance-*` needs no hardware.

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};
use wc_core::input::body::{
    BodyLandmark, BodyTrackingState, MaskTexture, SilhouetteEdges, BODY_LANDMARK_COUNT,
};
use wc_core::lifecycle::state::AppState;

use crate::radiance::settings::RadianceSettings;
use crate::radiance::systems::sim_params::IMPULSE_LANDMARKS;

/// Run condition: the synthetic-body capture toggle is set (debug builds).
#[cfg(debug_assertions)]
pub fn synthetic_body_forced(
    toggles: Option<Res<'_, wc_core::debug::DebugToggles>>,
) -> bool {
    toggles.is_some_and(|t| t.force_radiance_synthetic_body)
}

/// `Update` (debug builds, `sketch_active(Radiance)` + the toggle, ordered
/// before the live baker): drive the deterministic dancer. Writes the mask +
/// edge list every frame (fixed-dt capture wants per-frame freshness, and
/// thermal budget is irrelevant under capture), synthesizes the seven
/// impulse landmarks with finite-difference velocities, and overwrites
/// `AudioAnalysis` with the synthetic beat (running in `Update` after Plan
/// A's `PreUpdate` publisher means this write wins for the baker).
#[cfg(debug_assertions)]
#[allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "virtual-time trig on bounded values"
)]
pub fn drive_synthetic_body(
    time: Res<'_, Time>,
    mask: Option<Res<'_, MaskTexture>>,
    mut images: ResMut<'_, Assets<Image>>,
    edges: Option<ResMut<'_, SilhouetteEdges>>,
    body: Option<ResMut<'_, BodyTrackingState>>,
    audio: Option<ResMut<'_, wc_core::audio::input::AudioAnalysis>>,
    mut commands: Commands<'_, '_>,
) {
    use crate::radiance::synthetic::{
        dancer_landmark_uv, dancing_pose, extract_edges, rasterize_mask, synthetic_audio,
    };

    let t = time.elapsed_secs();
    let pose = dancing_pose(t);

    // Mask + edges through the same shared surfaces the real tracker uses.
    if let (Some(mask), Some(mut edges)) = (mask, edges) {
        if let Some(image) = images.get_mut(&mask.0) {
            if let Some(data) = image.data.as_mut() {
                rasterize_mask(&pose, data);
                extract_edges(data, &mut edges.points);
                edges.generation = edges.generation.wrapping_add(1);
            }
        }
    }

    // Landmarks + finite-difference velocities for the impulse slots.
    let uv_now = dancer_landmark_uv(&pose);
    let h = 1.0 / 60.0;
    let uv_prev = dancer_landmark_uv(&dancing_pose(t - h));
    let mut landmarks = [BodyLandmark::default(); BODY_LANDMARK_COUNT];
    let mut velocities = [Vec3::ZERO; BODY_LANDMARK_COUNT];
    for (slot, &lm_index) in IMPULSE_LANDMARKS.iter().enumerate() {
        landmarks[lm_index] = BodyLandmark {
            pos: Vec3::new(uv_now[slot].x, uv_now[slot].y, 0.0),
            visibility: 1.0,
        };
        let v = (uv_now[slot] - uv_prev[slot]) / h;
        velocities[lm_index] = Vec3::new(v.x, v.y, 0.0);
    }
    let state = BodyTrackingState {
        present: true,
        confidence: 1.0,
        landmarks,
        world_landmarks: [Vec3::ZERO; BODY_LANDMARK_COUNT],
        velocities,
        timestamp: time.elapsed(),
    };
    match body {
        Some(mut existing) => *existing = state,
        None => commands.insert_resource(state),
    }

    let frame = synthetic_audio(t);
    match audio {
        Some(mut existing) => *existing = frame,
        None => commands.insert_resource(frame),
    }
}

/// `Update` (`sketch_active(Radiance)`): gizmo tick + outward normal at each
/// edge point (the `edge_debug` Dev toggle). Early-outs on the bool.
pub fn draw_edge_debug(
    settings: Res<'_, RadianceSettings>,
    edges: Option<Res<'_, SilhouetteEdges>>,
    window: Single<'_, '_, &Window>,
    mut gizmos: Gizmos<'_, '_>,
) {
    if !settings.edge_debug {
        return;
    }
    let Some(edges) = edges else {
        return;
    };
    let scale = Vec2::new(window.width().max(1.0), window.height().max(1.0));
    for e in &edges.points {
        let pos = crate::radiance::systems::sim_params::mask_uv_to_world(
            Vec2::new(e.pos[0], e.pos[1]),
            scale,
            settings.mirror,
        );
        let dir = crate::radiance::systems::sim_params::mask_dir_to_world(
            Vec2::new(e.normal[0], e.normal[1]),
            scale,
            settings.mirror,
        )
        .normalize_or_zero();
        gizmos.line_2d(pos, pos + dir * 12.0, Color::srgb(0.2, 1.0, 0.6));
    }
}

/// `EguiPrimaryContextPass` (self-gated on state + the Dev bool): tracking +
/// audio readouts. Body frame rate is derived from `timestamp` deltas via
/// `Local`s — everything shown is computable from the pinned contract
/// surface alone.
pub fn radiance_inference_readout(
    app_state: Res<'_, State<AppState>>,
    settings: Res<'_, RadianceSettings>,
    body: Option<Res<'_, BodyTrackingState>>,
    audio: Option<Res<'_, wc_core::audio::input::AudioAnalysis>>,
    edges: Option<Res<'_, SilhouetteEdges>>,
    mut last_ts: Local<'_, f64>,
    mut fps: Local<'_, f32>,
    mut contexts: EguiContexts<'_, '_>,
) {
    if **app_state != AppState::Radiance || !settings.inference_readouts {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    if let Some(body) = body.as_ref() {
        let ts = body.timestamp.as_secs_f64();
        let dt = ts - *last_ts;
        if dt > 1e-6 {
            // One-pole smoothed body-frame rate from timestamp deltas.
            #[allow(
                clippy::as_conversions,
                clippy::cast_possible_truncation,
                reason = "display-only smoothing of a bounded dt"
            )]
            {
                *fps = *fps * 0.9 + (1.0 / dt as f32) * 0.1;
            }
            *last_ts = ts;
        }
    }
    egui::Window::new("Radiance readouts")
        .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(12.0, -12.0))
        .resizable(false)
        .show(ctx, |ui| {
            match body.as_ref() {
                Some(b) => {
                    ui.label(format!(
                        "body: present={} conf={:.2} ~{:.1} fps",
                        b.present, b.confidence, *fps
                    ));
                }
                None => {
                    ui.label("body: (no tracking resource)");
                }
            }
            ui.label(format!(
                "edges: {}",
                edges.as_ref().map_or(0, |e| e.points.len())
            ));
            match audio.as_ref() {
                Some(a) => {
                    ui.label(format!(
                        "audio: active={} rms={:.3} gain={:.2} onset={:.2}",
                        a.active, a.rms, a.gain, a.onset
                    ));
                }
                None => {
                    ui.label("audio: (no analysis resource)");
                }
            }
        });
}
```

Add `pub mod debug;` to `systems/mod.rs`.

- [ ] **Step 3: Wire into `RadiancePlugin::build`** (append):

```rust
        // Dev overlays: edge gizmos (settings-gated internally) + readouts
        // (self-gated egui pass system, flame's overlay idiom).
        app.add_systems(
            Update,
            systems::debug::draw_edge_debug
                .run_if(wc_core::sketch::sketch_active(AppState::Radiance)),
        );
        app.add_systems(
            bevy_egui::EguiPrimaryContextPass,
            systems::debug::radiance_inference_readout,
        );

        // Capture dancer: debug builds, only under the synthetic-body toggle,
        // ordered before the live baker so its resources win this frame.
        #[cfg(debug_assertions)]
        app.add_systems(
            Update,
            systems::debug::drive_synthetic_body
                .before(systems::sim_params::update_radiance_sim)
                .run_if(wc_core::sketch::sketch_active(AppState::Radiance))
                .run_if(systems::debug::synthetic_body_forced),
        );
```

- [ ] **Step 4: Document the toggle**

Add a row to the `WC_DEBUG_*` table in `tests/visual/CLAUDE.md`:

```markdown
| `WC_DEBUG_FORCE_RADIANCE_SYNTHETIC_BODY` | Drive Radiance from the deterministic synthetic dancer (mask/edges/landmarks/audio) and suppress the mic + camera activation requests. Used by both radiance capture scenarios. Presence = on. |
```

- [ ] **Step 5: Run tests + checks**

Run: `cargo nextest run -p wc-core debug:: capture:: && cargo nextest run -p wc-sketches && cargo check -p waveconductor`
Expected: PASS — including every pre-existing exhaustive-literal test now carrying the new field.

- [ ] **Step 6: Commit**

```bash
git add crates/wc-core/src/debug/mod.rs crates/wc-core/src/capture/system.rs crates/wc-core/src/lifecycle/screensaver/mod.rs crates/wc-sketches tests/visual/CLAUDE.md
git commit -m "feat(radiance): synthetic-body capture toggle and dev overlays" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 14: Route the body-tuning Dev knobs through `BodyTrackingRequest`

**Files:**
- Modify: `crates/wc-core/src/input/body/mod.rs` (Plan B's module — additive change only)
- Possibly modify: Plan B test files constructing `BodyTrackingRequest` literals (compiler-enumerated)

**Interfaces:**
- Consumes: the pinned `BodyTrackingRequest` (the contract explicitly permits adding fields; renaming/retyping is forbidden).
- Produces: `BodyTrackingRequest { idle_throttle, mask_ema, one_euro_min_cutoff, one_euro_beta }` — already constructed by Task 9's `insert_tracking_requests`. The three Radiance Dev fields are `requires_restart`, so a change re-runs `OnEnter` and re-inserts the request with fresh values; Plan B observes the fresh request at worker (re)start.

**Honesty note (the one deliberately discovery-bounded step in this plan):** Plan B's *internal* consumption point could not be read at planning time (only the pinned contract existed). The struct change below is fully specified; plumbing the three values from the request into the worker config is a small, bounded edit at the single site where Plan B's activation watcher reads the request to spawn/configure the worker. If Plan B already exposes its own live-tuning resource for these exact values, wire to that instead and note it in the commit message. Defaults here (0.6 / 1.0 / 0.05) must be reconciled with Plan B's actual defaults — if they differ, change the `RadianceSettings` defaults (both the attribute and the serde fn and the tests) to match Plan B, not vice versa.

- [ ] **Step 1: Extend the request struct**

In `crates/wc-core/src/input/body/mod.rs`, add to `BodyTrackingRequest`:

```rust
    /// Worker-side temporal EMA factor on the segmentation mask (0 = raw,
    /// higher = steadier/laggier). Read at worker (re)start; Radiance's Dev
    /// knob routes here via its requires_restart reload.
    pub mask_ema: f32,
    /// One-Euro landmark filter min-cutoff, Hz. Same routing.
    pub one_euro_min_cutoff: f32,
    /// One-Euro landmark filter beta (speed coefficient). Same routing.
    pub one_euro_beta: f32,
```

- [ ] **Step 2: Compiler-driven fixups + plumbing**

Run: `cargo check -p wc-core -p wc-sketches -p waveconductor`
Fix every `BodyTrackingRequest { .. }` literal the compiler reports (Plan B tests): fill the three fields with Plan B's defaults. Then plumb the three values at Plan B's request-consumption site into the worker/smoothing config (bounded edit; see the honesty note). Acceptance: `cargo nextest run -p wc-core input::body` passes, and the values demonstrably reach the worker config (extend one existing Plan B config-construction test with a non-default request if such a test exists; otherwise assert the mapping in a new small test at the consumption site).

- [ ] **Step 3: Run the full body suite**

Run: `cargo nextest run -p wc-core input::body && cargo nextest run -p wc-sketches radiance::`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/wc-core/src/input/body crates/wc-sketches
git commit -m "feat(radiance): route mask-EMA and One-Euro dev knobs through the body request" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 15: Capture scenarios + baselines (operator-assisted)

**Files:**
- Modify: `tests/visual/scenarios.toml` (append two scenarios)
- Modify: `tests/visual/CLAUDE.md` (scenario table + review guidance)
- Create (generated): `tests/visual/baselines/radiance-synthetic/*.png`, `tests/visual/baselines/radiance-screensaver/*.png`
- Replace: `assets/sketches/radiance/screenshot.png` (real tile image)

**Interfaces:**
- Consumes: Task 13's toggle; `WAVECONDUCTOR_START_SKETCH=radiance` (Task 1's `from_name`); the capture harness (`cargo xtask capture`).

- [ ] **Step 1: Append to `tests/visual/scenarios.toml`**

```toml
# Radiance, active, driven by the deterministic synthetic dancer (no mic, no
# camera — the toggle suppresses both activation requests). Frames sample the
# dancer's limb-swing cycle and two synthetic beats.
[scenarios.radiance-synthetic]
sketch   = "radiance"
provider = "off"
config   = "clean"
frames   = [60, 120, 240, 480]

[scenarios.radiance-synthetic.debug]
FORCE_RADIANCE_SYNTHETIC_BODY = "1"

# Radiance attract mode: the phantom performer + ember overrides. The
# synthetic-body toggle is set ONLY to suppress the hardware requests (the
# dancer itself is gated to Active and stays inert here); the phantom owns
# the mask during the screensaver.
[scenarios.radiance-screensaver]
sketch   = "radiance"
provider = "off"
config   = "clean"
frames   = [120, 360, 720, 1200]

[scenarios.radiance-screensaver.debug]
FORCE_RADIANCE_SYNTHETIC_BODY = "1"
FORCE_SCREENSAVER = "1"
```

- [ ] **Step 2: Document in `tests/visual/CLAUDE.md`**

Add both rows to the scenario table, and a review-guidance paragraph:

```markdown
Radiance review guidance (`radiance-synthetic`): (a) a dark glassy humanoid
silhouette with a thin bright rim, mirrored, centered, limbs visibly swinging
across frames; (b) particles emanate outward from the silhouette edge — never
from empty space — rising with a flame-like drift; (c) frames after a
synthetic beat (frame 120 lands just after one) show an outward burst;
(d) `delta_prev` stays well above ~5 (continuous motion). For
`radiance-screensaver`: a slower, thinner ember-toned aura around a gently
drifting phantom; whites/hot tones read ember-orange, and the field is
visibly sparser than the active scenario.
```

- [ ] **Step 3: Build + capture + review (OPERATOR-ASSISTED — window must be foregrounded)**

```bash
cargo build -p waveconductor
cargo xtask capture line-synthetic --json   # known-good canary FIRST
cargo xtask capture radiance-synthetic --json
cargo xtask capture radiance-screensaver --json
```

Known environment trap: captures come back all-black `[0,0,0]` when the app window is not foregrounded. If the radiance frames are black, check the `line-synthetic` canary — if it is also black, the run was unfocused (ask Madison to keep the capture window foregrounded, or run while she is at the machine); only a black radiance against a healthy canary is a real bug. The agent Reads the PNGs itself and judges against the review guidance (no LLM API spend).

- [ ] **Step 4: Seed baselines (only after visually confirming the frames)**

```bash
cargo xtask capture radiance-synthetic --update-baselines
cargo xtask capture radiance-screensaver --update-baselines
```

- [ ] **Step 5: Real tile screenshot**

From the best-looking `radiance-synthetic` frame (or a dedicated `--watch` run), produce the tile: copy the chosen frame over the placeholder and downscale is unnecessary (the tile renderer scales):

```bash
cp target/capture/radiance-synthetic/frame_0240.png assets/sketches/radiance/screenshot.png
```

(Pick the frame that best sells the sketch; confirm visually.)

- [ ] **Step 6: Commit**

```bash
git add tests/visual/scenarios.toml tests/visual/CLAUDE.md tests/visual/baselines/radiance-synthetic tests/visual/baselines/radiance-screensaver assets/sketches/radiance/screenshot.png
git commit -m "test(radiance): capture scenarios, baselines, and the real picker tile" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 16: Full gates + live smoke test (operator-assisted)

**Files:** none new — verification only (fix-forward anything the gates surface).

- [ ] **Step 1: Run every CI gate**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --document-private-items
cargo deny check
cargo xtask check-secrets
cargo xtask validate-shaders
```

Expected: all clean. Watch specifically for: intra-doc links into wc-core body/audio items (doc gate builds default features only — demote to code spans), and clippy's `too_many_arguments` on the baker/writer (add the flame-style scoped allow with a data-dependency rationale if it trips).

- [ ] **Step 2: Live smoke test (prompt Madison)**

Prompt her with exactly this checklist:

> `cargo rund`, then click the Radiance tile (or press N/P to cycle to it — it sits after Cymatics). Mic + webcam live. Please verify: (1) your silhouette appears as a dark glassy form with a glowing rim, mirrored like a mirror; (2) particles stream off your outline and rise; (3) music (or clapping) visibly pumps the aura — bass swells emission, hits make bursts; (4) fast arm sweeps shed particle trails; (5) hand-tracked sketches still work after leaving Radiance (the webcam is handed back ~1 s after exit); (6) idle for the timeout — the aura should die out, then the phantom screensaver should appear in embers, and stepping in front of the camera should wake it. The Dev knobs (mask threshold, One-Euro, debug overlays) sit behind the ADVANCED toggle in the Shift+D panel — it resets every launch, so flip it on first. If the aura reads mushy or laggy, the ear/eye-tune knobs are Emission, Flow strength, Buoyancy, and Audio sensitivity.

- [ ] **Step 3: Fix-forward and finish**

Address anything the gates or the smoke test surfaced (as new commits on `radiance`), then hand off per the team's merge flow (superpowers:finishing-a-development-branch).

---

## Self-review (performed while writing this plan)

**Spec coverage for Unit C** — checked item-by-item against the design doc:

| Spec item | Where |
|---|---|
| `AppState::Radiance` + activity `#[source]` + order/cycle/from_name + ui_picker guard | Task 1 |
| Canonical module shape (mod/settings/systems/compute/render/screensaver/edge upload) | File Structure; Tasks 2–13 |
| Own kernel: edge-list respawn, curl 2–3 octaves, buoyancy, 8 impulse slots | Task 4 (full WGSL), Task 3 (PODs) |
| Audio drive CPU-baked (bass→emission+buoyancy, highs→turbulence+sparkle, onset→burst, RMS→intensity) | Task 5 (`audio_drive`, baker, tests) |
| Motion drive: wrists/ankles/hips/head impulses, contract indices | Task 5 (`IMPULSE_LANDMARKS`, impulse baking, tests) |
| Additive HDR billboards, no new post-process; bloom/tonemap supply radiance | Task 7 (specialize `(One, One)`) |
| Silhouette fill quad sampling `MaskTexture` (glassy fill + rim, smoothstep edge, shimmer) | Task 8 (full WGSL) |
| Mirror default on, toggle | Tasks 2/4/5/8 (one mapping, three consumers) |
| Palette idiom, audio-shifted | Tasks 2 (stops), 5 (shift), 7/8 (uniforms) |
| Lifecycle: OnEnter chain, OnExit despawn/remove/reset, `remove_*_if_absent`, `sketch_active` gating, activity-driven `paused`/`idle_throttle`, Idle freeze | Tasks 6, 9, 10 |
| Camera arbitration via existing registry machinery, restore on exit, Leap untouched | Task 9 (`arbitration.rs`) |
| Screensaver: phantom SDF cluster through the SAME resources, ember, no audio, compute-lite | Task 12 |
| Synthetic performer shared by tests/phantom/captures | Task 11 (+13, 15) |
| Settings per spec incl. RuntimeEnum device + Dev knobs; house serde pattern + two tests | Task 2 (+14 routing) |
| `register_sketch_settings` / `register_sketch_tile` + screenshot | Task 2 (+15 real image) |
| Dispatch scales with count; cached bind groups keyed on BufferId, bounded; no per-frame GPU alloc | Task 6 |
| Restart/resize/render-profile shared generics | Tasks 2/10 |
| ≥2 capture scenarios + baselines + black-frame caveat | Task 15 |
| `cargo rund` smoke prompt incl. ADVANCED-toggle note | Task 16 |

**Known deliberate deviations / resolutions** (also in the final summary): own 32-byte particle + own render shader instead of literal `ParticleMaterial` reuse (additive blend + gradient/lifetime semantics force it; the *patterns* are reused); the arbitration suspends via provider `stop()`/deferred `start()` rather than registry removal (no access to the binary's constructors from wc-sketches — and it keeps the suspended provider honestly visible); mask→window mapping is full-window stretch in v1 (documented as the follow-up tune); Task 14 contains the one discovery-bounded plumbing step, with the struct change fully specified.

**Placeholder scan:** no TBDs; every code step is complete. The only intentionally deferred edits are compiler-enumerated literal fixups (Tasks 13/14) and Task 14's bounded plumbing, both with explicit acceptance criteria.

**Type consistency:** all Plan A/B types match the pinned contracts file verbatim (`AudioAnalysis` fields/Copy, request activation semantics, `BodyLandmark`/`BodyTrackingState` fields, `EdgePoint` 16-byte Pod, `MaskTexture` newtype, `SilhouetteEdges { points, generation }`, `MASK_SIZE`/`MAX_EDGE_POINTS`, landmark indices). Bevy API usage mirrors the exact idioms read from flame/particles/cymatics sources (BindGroupLayoutDescriptor::new, RenderStartup init, `queue.write_buffer` via `render_queue.0`, `Single<&Window>`, `MeshMaterial2d`, `EguiContexts`, `Time::advance_by`, `RunSystemOnce`).
