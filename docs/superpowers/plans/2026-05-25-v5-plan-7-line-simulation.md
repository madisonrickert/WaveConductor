# Plan 7: Line Simulation Parity + Idle Veto Hook Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring v5 Line's particle physics to functional parity with v4. After this plan, particles flow like v4 (dual drag, size-scaled gravity, multi-attractor list with float power, geometric mouse-power decay, originalX/Y + constrainToBox reset, per-particle fade-in, horizontal-line spawn with sawtooth jitter, `particleDensity` setting), and the idle system can be vetoed by a sketch so Line stays `Active` while attractors are still decaying. Visual character still flat-shaded (gravity-smear shader and star sprite are Plan 8). Audio still silent (Plan 9). Heatmap-image spawn still deferred (Plan 10).

**Architecture:** The GPU compute kernel remains authoritative for rendering (per spec §5.6 WebGPU-only). A new CPU mirror (`Vec<Particle>` resource) runs the same math in Rust each frame so Plan 9 has a readable source for `ParticleStats` without a GPU readback stall. The two integrators are independent; they will drift slightly over long timescales due to floating-point order-of-operations differences, but `groupedUpness` and friends are smooth scalars where 1% drift does not matter. Multi-attractor support adds a fixed-size attractor list (`MAX_ATTRACTORS = 8`) in `SimParams` — mouse is index 0, future Leap hands populate the rest. The idle veto is a thin Bevy-idiomatic hook: a `Resource<IdleVetoes>` holding `Vec<fn(&World) -> bool>` that `advance_activity` consults before transitioning out of `Active`.

**Tech Stack:** Rust 1.89, Bevy 0.18.1, existing `bytemuck` / `leafwing-input-manager` / `bevy_egui` / `wc-core-macros` from prior plans. No new workspace deps.

**Reference spec:** `docs/superpowers/specs/2026-05-22-bevy-rewrite-design.md` §5.2 (lifecycle), §5.5 (settings), §8 (parity decisions).

**Reference v4 sources** (read-only, on `git show main:<path>`):

- `src/sketches/line/index.ts` — `LineSketch` class, mouse attractor lifecycle, `step()` orchestration
- `src/particles/particleSystem.ts` — `ParticleSystem.stepParticles()`, the authoritative physics
- `src/particles/attractor.ts` — `Attractor` (power, position; visual mesh is Plan 8)
- `src/sketch/BaseSketch.ts` — idle system, `isReadyToSleep()` pattern

**Branch:** All work on `rewrite/bevy`.

**Pre-flight check:** verify HEAD is at or after the `v5-line` tag (commit `fe040d1`).

---

## Scope check

Plan 7 is the first plan in the 4-plan Line parity stack (see `docs/superpowers/roadmap.md`). It is intentionally simulation-only: no visual character changes, no audio. The deliverable is "v5 Line particles move like v4 Line particles when you compare the trajectories side-by-side, even though v5 still looks like generic warm-colored quads."

Five phases, each one commit. Phase F is push + tag.

## File structure

**Modified files** (Phase 0 carry-forwards):

- `crates/wc-core/src/settings/autosave.rs` — add `AppExit` flush handler.
- `crates/wc-core/src/settings/panel_user.rs` — extend `render_number` to dispatch on `i32` and `i64`; extend `render_widget` with `Vec2`/`Vec3`/`Color` branches.
- `crates/wc-core/src/settings/mod.rs` — drop `pub mod test_settings;`.
- `crates/wc-core/src/settings/test_settings.rs` — **deleted** (moved to `tests/common/`).
- `crates/wc-core/tests/common/mod.rs` — **new** — shared `TestSketchSettings` fixture for integration tests.
- `crates/wc-core/tests/settings_persistence.rs` — import the fixture from `mod common`.
- `crates/wc-core/tests/settings_plugin.rs` — import the fixture from `mod common`.
- `crates/wc-sketches/src/line/mod.rs` — replace the punt-to-Home restart with a same-frame OnExit→OnEnter cycle; update doc framing for Plan 7+ split.
- `crates/wc-sketches/src/line/compute.rs` — `min_binding_size: NonZeroU64` on bind-group-layout entries; `tracing::trace!` on `LineComputeNode::run` early returns.
- `crates/wc-sketches/src/line/systems.rs` — `Single<&Window>` instead of `Query<&Window>::iter().next()`; remove the 1 Hz diagnostic timer; *(gravity-formula tuning is folded into Phase C, not Phase 0)*.
- `crates/waveconductor/src/main.rs` — `cfg(debug_assertions)` switch on `AssetPlugin.file_path` so release bundles use the default `"assets"`.
- `crates/wc-sketches/src/line/PARITY.md` — re-target the "deferred to Plan 7" notes to Plans 8/9/10.

**Modified files** (Phase A — idle veto):

- `crates/wc-core/src/lifecycle/idle.rs` — `IdleVetoes` resource + `register_idle_veto` helper; `advance_activity` consults it.
- `crates/wc-core/src/lifecycle/mod.rs` — `init_resource::<idle::IdleVetoes>`.
- `crates/wc-core/tests/lifecycle_idle_veto.rs` — *new* — covers register, fires, clears.

**Modified files** (Phase B — multi-attractor):

- `crates/wc-sketches/src/line/particle.rs` — new `Attractor` struct (`Pod + Zeroable`); replace `SimParams.attractor_pos / radius / enabled` with `attractors: [Attractor; MAX_ATTRACTORS]` + `attractor_count: u32`.
- `assets/shaders/line/simulate.wgsl` — iterate the attractor array; accumulate force from each `power > 0` entry.
- `crates/wc-sketches/src/line/systems.rs` — `MouseAttractorState` resource (power, decay logic); `update_sim_params` writes the attractor array.
- `crates/wc-sketches/src/line/mod.rs` — register `MouseAttractorState`; wire the decay system.

**Modified files** (Phase C — physics parity + CPU mirror):

- `crates/wc-sketches/src/line/particle.rs` — `Particle` gains `original_xy: [f32; 2]` + `alpha: f32` (with one trailing `_pad: f32` to stay 32-byte / 16-multiple aligned — see task body); `SimParams` gains `pulling_drag_baked`, `inertial_drag_baked`, `size_scale`, `fade_duration`, `constrain_min`, `constrain_max`.
- `assets/shaders/line/simulate.wgsl` — port v4 physics: pick drag by `attractor_count`, apply size-scaled gravity, fade-in α, constrain-to-box-with-reset.
- `assets/shaders/line/render.wgsl` — use particle `alpha` as fragment α (and base color on alpha-weighted brightness so faded-in particles smoothly appear).
- `crates/wc-sketches/src/line/sim_cpu.rs` — *new* — pure-Rust port of the same step function. Unit-tested in isolation.
- `crates/wc-sketches/src/line/mod.rs` — install `LineCpuMirror` resource (initialized with the same grid as the GPU buffer); step it each `Update`.

**Modified files** (Phase D — spawn shape + density setting):

- `crates/wc-sketches/src/line/settings.rs` — `particle_count: u32` → `particle_density: f32` (with `min/max/step/default` matching v4's `particleDensity: 10`); `requires_restart` semantics retained for now.
- `crates/wc-sketches/src/line/systems.rs` — `spawn_line` switches to horizontal-line layout at mid-Y with `((i % 5) - 2) * 2` sawtooth jitter; derives `particle_count` from `density * window.width`.
- `crates/wc-sketches/src/line/mod.rs` — restart wiring already updated in Phase 0; verify it still fires when `particle_density` changes.

**Modified files** (Phase E — Line idle veto + tests):

- `crates/wc-sketches/src/line/mod.rs` — call `register_idle_veto` from `LinePlugin::build` with a closure that queries `MouseAttractorState`.
- `crates/wc-sketches/tests/line_lifecycle.rs` — add `idle_veto_keeps_line_active_during_attractor_decay`.

**Phase F:** push, verify CI green across the matrix, tag `v5-line-sim`.

---

## Conventions used in this plan

- All file paths are absolute from the repo root unless otherwise noted.
- Code blocks show the full file (or the full added section) so the implementer never has to merge by hand.
- Each `cargo` step lists the exact command + expected outcome.
- "Commit" steps stage files explicitly with the message specified.
- When the Bevy 0.18 API differs from what's shown, the implementer adapts to the installed version and notes deviations in the phase report.
- The v4 reference is **read-only** — `git show main:<path>` only, never check out.

---

# Phase 0 — Plan 6 carry-forwards

Ten items from `docs/superpowers/next-plan-carry-forwards.md` (the nine bullets) plus a PARITY.md update. Ships as one commit before any new functionality.

### Task 1: Save-on-exit flush in `autosave`

**File:** `crates/wc-core/src/settings/autosave.rs`

When the app receives `AppExit`, drain `AutosaveState.pending` and call every queued `save_fn` so edits made in the last <0.5 s are not lost.

- [ ] **Step 1: Add the flush system**

At the end of `crates/wc-core/src/settings/autosave.rs`, before any existing `#[cfg(test)]` block, append:

```rust
/// Drains any pending debounce timers on `AppExit` and writes every queued
/// settings type to disk. Without this, edits made in the <0.5 s window before
/// shutdown are lost because [`tick`] never fires their saves.
///
/// Reads `MessageReader<AppExit>` and runs in `Update` (not `Last`) because
/// Bevy's exit handling consumes `Update`'s schedule cycle.
pub fn flush_on_exit(world: &mut World) {
    let mut state = bevy::ecs::system::SystemState::<
        bevy::prelude::MessageReader<'_, '_, bevy::app::AppExit>,
    >::new(world);
    let mut reader = state.get_mut(world);
    let exiting = reader.read().next().is_some();
    state.apply(world);
    if !exiting {
        return;
    }
    let keys: smallvec::SmallVec<[&'static str; 8]> = {
        let mut s = world.resource_mut::<AutosaveState>();
        let collected = s.pending.keys().copied().collect();
        s.pending.clear();
        collected
    };
    if keys.is_empty() {
        return;
    }
    let snapshot: SaveSnapshot = world
        .get_resource::<SettingsRegistry>()
        .map(|r| r.entries.iter().map(|e| (e.save_fn, e.storage_key)).collect())
        .unwrap_or_default();
    for key in keys {
        if let Some((save_fn, _)) = snapshot.iter().find(|(_, k)| *k == key) {
            save_fn(world);
            tracing::info!(%key, "settings saved (flush on AppExit)");
        }
    }
}
```

- [ ] **Step 2: Register the flush system**

In `crates/wc-core/src/settings/mod.rs`, extend the `Update` system tuple inside `SettingsPlugin::build` to include `autosave::flush_on_exit` after `autosave::tick`:

```rust
.add_systems(
    Update,
    (
        panel_dev::handle_dev_panel_toggle,
        registry::emit_restart_events,
        autosave::detect_changes,
        autosave::tick,
        autosave::flush_on_exit,
    )
        .chain(),
);
```

- [ ] **Step 3: Add a unit test**

At the end of `crates/wc-core/src/settings/autosave.rs`, in the existing `#[cfg(test)]` block (or add one if absent), add:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bevy::app::AppExit;

    #[test]
    fn flush_on_exit_drains_pending_when_exit_emitted() {
        let mut app = bevy::prelude::App::new();
        app.add_plugins(bevy::MinimalPlugins);
        app.init_resource::<AutosaveState>();
        app.init_resource::<crate::settings::registry::SettingsRegistry>();

        // Seed one pending key. We don't need a real save_fn here — with no
        // matching registry entry, flush_on_exit logs "no save fn" and moves
        // on. The key behavior under test is that `pending` is drained.
        app.world_mut()
            .resource_mut::<AutosaveState>()
            .pending
            .insert("synthetic-key", DEBOUNCE_SECS);

        // Emit AppExit and run one update.
        app.world_mut().write_message(AppExit::Success);
        app.add_systems(bevy::prelude::Update, flush_on_exit);
        app.update();

        let state = app.world().resource::<AutosaveState>();
        assert!(state.pending.is_empty(), "flush_on_exit must drain pending");
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p wc-core --lib settings::autosave`
Expected: pass (existing tests plus the new one).

### Task 2: Reflection panel type coverage

**File:** `crates/wc-core/src/settings/panel_user.rs`

Extend `render_number` and `render_widget` so future sketches don't silently fall through to `(unsupported number type)`.

- [ ] **Step 1: Add i32 / i64 dispatch to `render_number`**

In `render_number` (the function starting at the existing `if let Some(v) = field.try_downcast_mut::<u32>()` block), insert before the final `ui.label(...)`:

```rust
    if let Some(v) = field.try_downcast_mut::<i32>() {
        let mut tmp = *v as i64;
        let mut slider = egui::Slider::new(&mut tmp, (lo as i64)..=(hi as i64)).text(label);
        if let Some(s) = step {
            slider = slider.step_by(s);
        }
        if ui.add(slider).changed() {
            *v = tmp.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        }
        return;
    }
    if let Some(v) = field.try_downcast_mut::<i64>() {
        let mut slider = egui::Slider::new(v, (lo as i64)..=(hi as i64)).text(label);
        if let Some(s) = step {
            slider = slider.step_by(s);
        }
        ui.add(slider);
        return;
    }
```

- [ ] **Step 2: Add Vec2 / Vec3 branches**

After the existing `render_text` function, add:

```rust
fn render_vec2(field: &mut dyn bevy::reflect::PartialReflect, label: &str, ui: &mut egui::Ui) {
    if let Some(v) = field.try_downcast_mut::<bevy::math::Vec2>() {
        ui.horizontal(|ui| {
            ui.label(label);
            ui.add(egui::DragValue::new(&mut v.x).prefix("x: "));
            ui.add(egui::DragValue::new(&mut v.y).prefix("y: "));
        });
    } else {
        ui.label(format!("(expected Vec2 for {label})"));
    }
}

fn render_vec3(field: &mut dyn bevy::reflect::PartialReflect, label: &str, ui: &mut egui::Ui) {
    if let Some(v) = field.try_downcast_mut::<bevy::math::Vec3>() {
        ui.horizontal(|ui| {
            ui.label(label);
            ui.add(egui::DragValue::new(&mut v.x).prefix("x: "));
            ui.add(egui::DragValue::new(&mut v.y).prefix("y: "));
            ui.add(egui::DragValue::new(&mut v.z).prefix("z: "));
        });
    } else {
        ui.label(format!("(expected Vec3 for {label})"));
    }
}
```

`render_color` already handles `[f32; 4]`. If a future sketch uses `bevy::color::Color`, extend `render_color` to also dispatch on it:

```rust
fn render_color(field: &mut dyn bevy::reflect::PartialReflect, label: &str, ui: &mut egui::Ui) {
    if let Some(v) = field.try_downcast_mut::<[f32; 4]>() {
        ui.horizontal(|ui| {
            ui.label(label);
            ui.color_edit_button_rgba_unmultiplied(v);
        });
        return;
    }
    if let Some(v) = field.try_downcast_mut::<bevy::color::Color>() {
        let mut rgba = v.to_srgba().to_f32_array();
        ui.horizontal(|ui| {
            ui.label(label);
            if ui.color_edit_button_rgba_unmultiplied(&mut rgba).changed() {
                *v = bevy::color::Color::srgba(rgba[0], rgba[1], rgba[2], rgba[3]);
            }
        });
        return;
    }
    ui.label(format!("(expected [f32; 4] or Color for {label})"));
}
```

- [ ] **Step 3: Wire the new branches into `render_widget`**

There is no existing `SettingKind` variant for `Vec2`/`Vec3` — they are not yet expressible in a `#[setting(...)]` attribute. Document this in a `///` comment above `render_vec2` and `render_vec3`:

```rust
/// Reflection branch for `Vec2` fields. Not yet reachable through the
/// `#[setting(...)]` attribute (no `SettingKind` variant); added eagerly so
/// the panel is ready when the next sketch needs it. The derive macro will
/// gain a `kind = Vec2` parser when that sketch lands.
```

(Same boilerplate for `render_vec3`.)

This keeps the dead code marked but linkable; the next sketch enabling these in the macro will not have to touch the panel.

- [ ] **Step 4: Run the build**

Run: `cargo build -p wc-core`
Expected: builds without warnings. (Add `#[allow(dead_code)]` on `render_vec2`/`render_vec3` if necessary; the `Vec2/Vec3` branches are not yet called.)

### Task 3: Auto-reenter on `requires_restart`

**File:** `crates/wc-sketches/src/line/mod.rs`

The current `restart_on_settings_change` punts to `AppState::Home` and forces the user to re-click the sketch tile. Replace with a same-frame `OnExit → OnEnter` cycle.

- [ ] **Step 1: Replace the restart handler**

In `crates/wc-sketches/src/line/mod.rs`, replace the entire `restart_on_settings_change` function with:

```rust
/// Listens for `SketchRestart { storage_key == LineSettings::STORAGE_KEY }`
/// and forces a same-frame `Line → Home → Line` cycle so the `OnExit`/`OnEnter`
/// systems rebuild the sketch with the new settings.
///
/// Uses a one-frame `LineRestartPending` resource as a self-clearing trampoline:
/// on the frame the restart message arrives, we set `NextState::Home` *and*
/// insert `LineRestartPending`. On the following frame's update, the resource is
/// observed → `NextState::Line`, then the resource is removed.
fn restart_on_settings_change(
    mut events: MessageReader<'_, '_, wc_core::settings::SketchRestart>,
    current: Res<'_, State<AppState>>,
    mut next: ResMut<'_, NextState<AppState>>,
    mut commands: Commands<'_, '_>,
    pending: Option<Res<'_, LineRestartPending>>,
) {
    if pending.is_some() {
        // Second frame: complete the cycle by re-entering Line.
        next.set(AppState::Line);
        commands.remove_resource::<LineRestartPending>();
        tracing::info!("LineSettings restart cycle: re-entering Line");
        return;
    }
    let want_restart = events
        .read()
        .any(|e| e.storage_key == settings::LineSettings::STORAGE_KEY);
    if want_restart && **current == AppState::Line {
        next.set(AppState::Home);
        commands.insert_resource(LineRestartPending);
        tracing::info!("LineSettings changed — cycling Line via Home for one frame");
    }
}

/// Trampoline marker for the same-frame Line→Home→Line cycle. Inserted on the
/// frame a restart is detected; the next frame's `restart_on_settings_change`
/// observes it, transitions back to `Line`, and removes the resource.
#[derive(Resource)]
struct LineRestartPending;
```

- [ ] **Step 2: Test the cycle**

In `crates/wc-sketches/tests/line_lifecycle.rs`, add at the bottom of the file (after the existing tests):

```rust
#[test]
fn settings_restart_cycles_back_to_line() {
    use wc_core::settings::SketchRestart;
    use wc_sketches::line::settings::LineSettings;
    use wc_core::settings::SketchSettings;

    let mut app = build_app();
    app.update();

    // Enter Line and let OnEnter run.
    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Line);
    app.update();
    app.update();
    assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Line);

    // Emit a SketchRestart for LineSettings.
    app.world_mut().write_message(SketchRestart {
        storage_key: LineSettings::STORAGE_KEY,
    });
    app.update(); // restart handler sets NextState::Home + inserts pending
    app.update(); // state transition processed → AppState::Home
    assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Home);
    app.update(); // restart handler sees pending → sets NextState::Line
    app.update(); // state transition → AppState::Line
    assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Line);
}
```

- [ ] **Step 3: Run the test**

Run: `cargo test -p wc-sketches --test line_lifecycle settings_restart_cycles_back_to_line`
Expected: pass.

### Task 4: Render-graph trace logs

**File:** `crates/wc-sketches/src/line/compute.rs`

Add `tracing::trace!` calls on each early-return branch of `LineComputeNode::run` so "why aren't particles dispatching?" is observable.

- [ ] **Step 1: Add trace logs**

In `crates/wc-sketches/src/line/compute.rs`, modify the body of `impl render_graph::Node for LineComputeNode` so each early return logs:

```rust
fn run(
    &self,
    _graph: &mut render_graph::RenderGraphContext<'_>,
    render_context: &mut RenderContext<'_>,
    world: &World,
) -> Result<(), render_graph::NodeRunError> {
    let Some(bg) = world.get_resource::<LineComputeBindGroup>() else {
        tracing::trace!("LineComputeNode: no bind group — sketch inactive or buffer not ready");
        return Ok(());
    };
    let Some(pipeline_res) = world.get_resource::<LinePipeline>() else {
        tracing::trace!("LineComputeNode: no LinePipeline resource");
        return Ok(());
    };
    let pipeline_cache = world.resource::<PipelineCache>();
    let Some(compute_pipeline) = pipeline_cache.get_compute_pipeline(pipeline_res.pipeline_id)
    else {
        tracing::trace!("LineComputeNode: pipeline still compiling");
        return Ok(());
    };

    let mut pass =
        render_context
            .command_encoder()
            .begin_compute_pass(&ComputePassDescriptor {
                label: Some("line_compute_pass"),
                timestamp_writes: None,
            });
    pass.set_pipeline(compute_pipeline);
    pass.set_bind_group(0, &bg.bind_group, &[]);
    pass.dispatch_workgroups(bg.dispatch_size, 1, 1);
    Ok(())
}
```

- [ ] **Step 2: Build**

Run: `cargo check -p wc-sketches`
Expected: clean.

### Task 5: `min_binding_size: NonZeroU64` on compute bind-group layout

**File:** `crates/wc-sketches/src/line/compute.rs`

Tighten validation so a `SimParams` struct-size drift trips at pipeline creation rather than at runtime binding.

- [ ] **Step 1: Compute the binding size**

In `crates/wc-sketches/src/line/compute.rs`, replace the existing `BindGroupLayoutDescriptor::new` call body inside `init_line_pipeline` with the version below. The change is `min_binding_size: Some(NonZeroU64::new(...).expect("..."))` on the uniform entry. The storage entry stays `None` (storage buffers are runtime-sized).

```rust
use std::num::NonZeroU64;

let sim_params_size = NonZeroU64::new(std::mem::size_of::<super::particle::SimParams>() as u64)
    .expect("SimParams must be non-zero-sized");

let bind_group_layout_descriptor = BindGroupLayoutDescriptor::new(
    "line_compute_bgl",
    &[
        BindGroupLayoutEntry {
            binding: 0,
            visibility: ShaderStages::COMPUTE,
            ty: BindingType::Buffer {
                ty: BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: Some(sim_params_size),
            },
            count: None,
        },
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
    ],
);
```

- [ ] **Step 2: Build**

Run: `cargo check -p wc-sketches`
Expected: clean.

### Task 6: Move `test_settings` to `tests/common/`

**Files:** `crates/wc-core/src/settings/mod.rs`, `crates/wc-core/src/settings/test_settings.rs` (deleted), `crates/wc-core/tests/common/mod.rs` (new), `crates/wc-core/tests/settings_persistence.rs`, `crates/wc-core/tests/settings_plugin.rs`

`#[cfg(test)]` would hide the module from integration tests in `tests/` (those build as separate crates against the library's public API). The canonical fix is to move `TestSketchSettings` into a `tests/common/` shared helper that integration tests import via `mod common;`.

- [ ] **Step 1: Create the shared helper**

Create `crates/wc-core/tests/common/mod.rs` with:

```rust
//! Shared fixtures for `wc-core` integration tests.
//!
//! `TestSketchSettings` is a small, varied settings struct that touches every
//! `SettingKind` so panel renderers and persistence can be tested in isolation
//! against a stable target. Lives in `tests/common/` (not `src/`) so it does
//! not ship in the release binary.

#![allow(
    dead_code,
    reason = "test fixtures may be referenced from only some integration tests"
)]

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "test")]
pub struct TestSketchSettings {
    #[setting(default = 42_u32, min = 0_u32, max = 1000_u32, category = User, requires_restart)]
    pub widget_count: u32,
    #[setting(default = 0.5_f32, min = 0.0_f32, max = 4.0_f32, step = 0.05_f32, category = User)]
    pub tempo_hz: f32,
    #[setting(default = true, ty = Boolean, category = User)]
    pub enable_tint: bool,
    #[setting(default = [1.0_f32, 1.0, 1.0, 1.0], ty = Color, category = User)]
    pub tint_color: [f32; 4],
    #[setting(default = String::from("default"), ty = Text, category = Dev)]
    pub dev_label: String,
}
```

- [ ] **Step 2: Wire the helper into the existing integration tests**

In `crates/wc-core/tests/settings_persistence.rs`, replace the line:

```rust
use wc_core::settings::test_settings::TestSketchSettings;
```

with:

```rust
mod common;
use common::TestSketchSettings;
```

(If the test file doesn't declare a `mod common;` near the top of its module imports, add one.)

Do the same in `crates/wc-core/tests/settings_plugin.rs`.

> **Cargo quirk:** Cargo treats every `.rs` file directly under `tests/` as its own integration-test binary. `tests/common/mod.rs` (a directory module, not a top-level file) is **not** itself promoted to a binary — it's only compiled when referenced via `mod common;`. This is the standard Rust pattern; no Cargo.toml change required.

- [ ] **Step 3: Remove the in-crate module**

Delete `crates/wc-core/src/settings/test_settings.rs` and remove `pub mod test_settings;` from `crates/wc-core/src/settings/mod.rs`.

- [ ] **Step 4: Verify**

Run: `cargo build -p wc-core` — expected: clean.
Run: `grep -rn 'test_settings\|TestSketchSettings' crates/wc-core/src/ crates/wc-sketches/src/ crates/waveconductor/src/ 2>/dev/null` — expected: no hits.
Run: `cargo test -p wc-core` — expected: pass (integration tests pick up the fixture via the new path).

### Task 7: `Single<&Window>` in `update_sim_params`

**File:** `crates/wc-sketches/src/line/systems.rs`

Replace `Query<&Window>::iter().next()` with `Single<&Window>` so a missing window fails loudly rather than silently using fallback coordinates.

- [ ] **Step 1: Update the system signature**

In `crates/wc-sketches/src/line/systems.rs`, change `update_sim_params`'s `windows: Query<'_, '_, &Window>,` parameter to:

```rust
    window: Single<'_, &Window>,
```

And inside the function, replace the `match pointer.primary { Some(...) => { let (cx, cy) = if let Some(window) = windows.iter().next() { ... } else { (cursor_window.x, cursor_window.y) }; ... } }` block with:

```rust
    let (attractor_pos, attractor_enabled) = match pointer.primary {
        Some(cursor_window) => {
            let w = window.width();
            let h = window.height();
            let wx = cursor_window.x - w * 0.5;
            let wy = -(cursor_window.y - h * 0.5);
            ([wx, wy], 1.0_f32)
        }
        None => ([0.0_f32, 0.0_f32], 0.0_f32),
    };
```

- [ ] **Step 2: Add a synthetic `Window` entity to the lifecycle test harness**

`MinimalPlugins` does not run `WindowPlugin`, so no `Window` component exists by default. `Single<&Window>` panics without one. In `crates/wc-sketches/tests/line_lifecycle.rs`, inside `build_app()`, after the existing `app.init_resource::<PointerState>();` line, add:

```rust
    // Single<&Window> needs an entity with a Window component. WindowPlugin
    // creates one in production; tests use MinimalPlugins, so spawn one
    // manually with a fixed resolution that matches the production default.
    app.world_mut().spawn(Window {
        resolution: (1280_u32, 720_u32).into(),
        ..Default::default()
    });
```

- [ ] **Step 3: Build and run lifecycle test**

Run: `cargo test -p wc-sketches --test line_lifecycle`
Expected: pass.

### Task 8: Remove the 1 Hz diagnostic log

**File:** `crates/wc-sketches/src/line/systems.rs`

The diagnostic is no longer needed; pointer state was confirmed correct in manual testing.

- [ ] **Step 1: Remove the timer and log**

In `crates/wc-sketches/src/line/systems.rs`, remove from `update_sim_params`:

1. The `mut diag_timer: Local<'_, f32>,` parameter.
2. The entire `*diag_timer += time.delta_secs();` block and its `tracing::info!(...)` body.

- [ ] **Step 2: Build**

Run: `cargo build -p wc-sketches`
Expected: clean. If `time` becomes unused after removing the timer, leave it in place — Phase C will use it for fixed-timestep accumulation.

### Task 9: Asset-path config for release bundles

**File:** `crates/waveconductor/src/main.rs`

Use `cfg(debug_assertions)` to pick the right path. Dev builds use the workspace-relative `"../../assets"`; release builds use the default `"assets"` so bundlers (DMG / portable exe / AppImage) find the asset tree they copy next to the binary.

- [ ] **Step 1: Update `AssetPlugin` config**

In `crates/waveconductor/src/main.rs`, replace the `.set(AssetPlugin { ... })` block with:

```rust
                .set(AssetPlugin {
                    // Dev builds: shaders live at the workspace root, two levels
                    // above the binary crate. Release bundles: the bundler
                    // copies `assets/` next to the binary, so the default
                    // `"assets"` is correct.
                    #[cfg(debug_assertions)]
                    file_path: "../../assets".into(),
                    ..default()
                })
```

- [ ] **Step 2: Verify dev build still finds shaders**

Run: `cargo build -p waveconductor`
Expected: clean.

(Release bundle path is exercised in the eventual distribution plan; here we only confirm the dev path still works.)

### Task 10: Update Line `PARITY.md` to reflect the new plan split

**File:** `crates/wc-sketches/src/line/PARITY.md`

The current `PARITY.md` says "audio coupling and heatmap spawn deferred to Plan 7." That's now wrong — Plan 7 is simulation parity; audio is Plan 9; heatmap is Plan 10.

- [ ] **Step 1: Replace the file contents**

Overwrite `crates/wc-sketches/src/line/PARITY.md` with:

```markdown
# Line — Parity Record

**Parity target:** Perceptual

**Reference media:** v4 main branch, `src/sketches/line/screenshots/gravity4_cropped.png` and the festival-loop recording from `scenarios/festival-loop.toml` at Pi Party 2026-03 timestamp.

**Plan progression toward parity:**

- **Plan 6 (shipped, tag `v5-line`)** — sketch scaffolding, single-attractor inverse-linear gravity, flat-color quads.
- **Plan 7 (this plan, tag `v5-line-sim`)** — multi-attractor physics with dual drag, size-scaled gravity, mouse-power decay, `original_xy` + constrain-to-box, fade-in α, horizontal-line spawn with sawtooth jitter, `particle_density` setting.
- **Plan 8 (tag `v5-line-render`)** — gravity-smear post-process shader, star sprite, attractor ring meshes.
- **Plan 9 (tag `v5-line-audio`)** — fundsp-based synthesis, particle-stats coupling driving synth params and shader uniforms.
- **Plan 10 (tag `v5-line-parity`)** — heatmap-image spawn, `gamma` setting, signed verdict.

**Approved deviations from v4** (carried forward; verdict deferred until Plan 10):

- Render uses vertex-index-driven quads (6 vertices per particle, triangle list mesh) rather than instanced quads. Visually identical; chosen because Bevy 0.18's `Material2d` path does not support N-instance single-entity draws without a custom render phase.
- WGSL compute kernel replaces CPU-side `particleSystem.ts` for rendering; a parallel Rust CPU mirror runs the same math on the host (introduced in Plan 7) to feed `ParticleStats` in Plan 9 without a GPU readback stall. The two integrators may drift by ≤1% over long timescales due to floating-point order-of-operations differences; acceptable for groupedUpness and friends.

**Verdict:** pending (Plan 10).
```

### Task 11: Commit Phase 0

- [ ] **Step 1: Stage the modified files**

```bash
git add \
    crates/wc-core/src/settings/autosave.rs \
    crates/wc-core/src/settings/panel_user.rs \
    crates/wc-core/src/settings/mod.rs \
    crates/wc-core/tests/common/mod.rs \
    crates/wc-core/tests/settings_persistence.rs \
    crates/wc-core/tests/settings_plugin.rs \
    crates/wc-sketches/src/line/mod.rs \
    crates/wc-sketches/src/line/compute.rs \
    crates/wc-sketches/src/line/systems.rs \
    crates/wc-sketches/src/line/PARITY.md \
    crates/wc-sketches/tests/line_lifecycle.rs \
    crates/waveconductor/src/main.rs
git rm crates/wc-core/src/settings/test_settings.rs
```

- [ ] **Step 2: Verify nothing else is staged**

Run: `git status --short`
Expected: only the files above; no stray test fixtures or `Cargo.lock` changes from cargo invocations.

- [ ] **Step 3: Commit**

```bash
git commit -m "$(cat <<'EOF'
Plan 7 Phase 0: absorb Plan 6 carry-forwards

Save-on-exit flush, reflection panel coverage (i32/i64/Vec2/Vec3/Color),
same-frame Line restart cycle, render-graph trace logs, min_binding_size
on the SimParams uniform, drop test_settings.rs from production, switch
to Single<&Window>, remove 1Hz diagnostic, cfg(debug_assertions) asset
path, and rewrite PARITY.md against the new Plan 7-10 split.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 4: Run the full test suite**

Run: `cargo test --workspace`
Expected: pass.

---

# Phase A — Idle veto hook in lifecycle

Sketches need to keep themselves `Active` while attractors decay. A new `IdleVetoes` resource holds `Vec<fn(&World) -> bool>`; `advance_activity` consults the list and stays `Active` if any veto returns `true`. Ships as one commit.

### Task 12: `IdleVetoes` resource + registration helper

**File:** `crates/wc-core/src/lifecycle/idle.rs`

- [ ] **Step 1: Define the resource and trait**

After the existing `impl InteractionTimer { ... }` block in `crates/wc-core/src/lifecycle/idle.rs`, insert:

```rust
/// Function pointer type for idle vetos. Receives a read-only `World` reference;
/// returning `true` keeps the sketch in `SketchActivity::Active` regardless of
/// elapsed idle time.
pub type IdleVetoFn = fn(&World) -> bool;

/// List of registered veto callbacks. [`advance_activity`] consults this list
/// before transitioning out of `Active`; any veto returning `true` overrides
/// the timeout-based decision.
///
/// Sketches register their veto in `Plugin::build` via
/// [`RegisterIdleVetoExt::register_idle_veto`].
#[derive(Resource, Default)]
pub struct IdleVetoes {
    /// Registered veto callbacks.
    pub vetos: Vec<IdleVetoFn>,
}

/// Returns `true` if any registered veto fires for the current world state.
fn any_veto_active(world: &World) -> bool {
    let Some(vetos) = world.get_resource::<IdleVetoes>() else {
        return false;
    };
    vetos.vetos.iter().any(|f| f(world))
}

/// Extension trait that adds `register_idle_veto` to Bevy's [`App`].
pub trait RegisterIdleVetoExt {
    /// Register a closure that returns `true` while the sketch should stay
    /// `Active` regardless of the idle timer.
    ///
    /// Registrations accumulate; multiple sketches can each contribute a veto.
    /// Vetoes are not auto-removed when a sketch exits — they read `World`
    /// and gracefully return `false` if their resources are absent.
    fn register_idle_veto(&mut self, veto: IdleVetoFn) -> &mut Self;
}

impl RegisterIdleVetoExt for App {
    fn register_idle_veto(&mut self, veto: IdleVetoFn) -> &mut Self {
        let mut vetos = self
            .world_mut()
            .get_resource_or_insert_with(IdleVetoes::default);
        vetos.vetos.push(veto);
        self
    }
}
```

- [ ] **Step 2: Convert `advance_activity` to an exclusive system**

Bevy 0.18 does not accept `&World` as a regular system parameter alongside `Res<...>`. To read arbitrary world state for the vetos, `advance_activity` becomes an exclusive system (`world: &mut World`). The chain ordering with `reset_on_interaction` is preserved.

Replace the existing `pub fn advance_activity(...)` function in `crates/wc-core/src/lifecycle/idle.rs` with:

```rust
pub fn advance_activity(world: &mut World) {
    let now = world.resource::<Time>().elapsed();
    let timer = world.resource::<InteractionTimer>().clone();
    let idle = timer.idle_for(now);
    let timeout_target = if idle >= timer.screensaver_threshold + timer.idle_threshold {
        SketchActivity::Screensaver
    } else if idle >= timer.idle_threshold {
        SketchActivity::Idle
    } else {
        SketchActivity::Active
    };
    let target = if timeout_target != SketchActivity::Active && any_veto_active(world) {
        SketchActivity::Active
    } else {
        timeout_target
    };
    let Some(current) = world.get_resource::<State<SketchActivity>>() else {
        return; // Not in a sketch state; nothing to do.
    };
    if *current.get() == target {
        return;
    }
    if let Some(mut next) = world.get_resource_mut::<NextState<SketchActivity>>() {
        next.set(target);
    }
}
```

The function pointer type and registration trait stay the same. `any_veto_active` already takes `&World`, so it works either way.

- [ ] **Step 3: Register the resource in the plugin**

In `crates/wc-core/src/lifecycle/mod.rs`, extend `LifecyclePlugin::build` to initialize the resource alongside `InteractionTimer`:

```rust
.init_resource::<idle::InteractionTimer>()
.init_resource::<idle::IdleVetoes>()
```

- [ ] **Step 4: Re-export the extension trait**

In `crates/wc-core/src/lifecycle/idle.rs`, the trait is already public. In `crates/wc-core/src/lifecycle/mod.rs`, re-export it at the module level (top of file, near other `pub use`s — if there are none yet, add a fresh export):

```rust
pub use idle::RegisterIdleVetoExt;
```

- [ ] **Step 5: Update the existing idle docs**

The module-level `//!` block in `crates/wc-core/src/lifecycle/idle.rs` currently describes a strictly time-based system. Update the third bullet to:

```text
//! - [`advance_activity`] reads the timer each frame and transitions
//!   [`crate::lifecycle::state::SketchActivity`] through
//!   `Active → Idle → Screensaver` as the elapsed time crosses thresholds,
//!   unless a sketch-registered [`IdleVetoFn`] (via [`IdleVetoes`]) overrides
//!   the decision to keep the sketch `Active`.
```

### Task 13: Idle veto integration test

**File:** `crates/wc-core/tests/lifecycle_idle_veto.rs` (new)

- [ ] **Step 1: Create the test file**

```rust
//! Integration tests for the idle-veto hook.

#![allow(
    clippy::expect_used,
    reason = "expect with a clear message is appropriate in test code"
)]

use std::time::Duration;

use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use wc_core::lifecycle::idle::{IdleVetoes, InteractionTimer, RegisterIdleVetoExt};
use wc_core::lifecycle::state::{AppState, SketchActivity};

#[derive(Resource, Default)]
struct VetoFlag(bool);

fn flag_veto(world: &World) -> bool {
    world.get_resource::<VetoFlag>().map_or(false, |f| f.0)
}

fn build_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_plugins(bevy::input::InputPlugin);
    app.add_plugins(StatesPlugin);
    app.init_state::<AppState>();
    app.add_sub_state::<SketchActivity>();
    app.add_plugins(leafwing_input_manager::plugin::InputManagerPlugin::<
        wc_core::lifecycle::actions::WaveConductorAction,
    >::default());
    app.insert_resource(wc_core::lifecycle::actions::default_input_map());
    app.init_resource::<leafwing_input_manager::prelude::ActionState<
        wc_core::lifecycle::actions::WaveConductorAction,
    >>();
    app.add_plugins(wc_core::lifecycle::LifecyclePlugin);
    app
}

#[test]
fn no_veto_means_idle_after_threshold() {
    let mut app = build_app();
    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Line);
    app.update();

    // Mark interaction at t=0, then advance time past idle_threshold.
    let threshold = app.world().resource::<InteractionTimer>().idle_threshold;
    app.world_mut()
        .resource_mut::<Time>()
        .advance_by(threshold + Duration::from_secs(1));
    app.update();
    app.update();

    let activity = app.world().resource::<State<SketchActivity>>();
    assert_eq!(
        *activity.get(),
        SketchActivity::Idle,
        "no veto registered → idle transition fires"
    );
}

#[test]
fn active_veto_keeps_sketch_active() {
    let mut app = build_app();
    app.init_resource::<VetoFlag>();
    app.register_idle_veto(flag_veto);
    app.world_mut().resource_mut::<VetoFlag>().0 = true;

    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Line);
    app.update();

    let threshold = app.world().resource::<InteractionTimer>().idle_threshold;
    app.world_mut()
        .resource_mut::<Time>()
        .advance_by(threshold + Duration::from_secs(1));
    app.update();
    app.update();

    let activity = app.world().resource::<State<SketchActivity>>();
    assert_eq!(
        *activity.get(),
        SketchActivity::Active,
        "veto override → sketch stays Active despite elapsed idle time"
    );
}

#[test]
fn veto_clearing_lets_sketch_idle() {
    let mut app = build_app();
    app.init_resource::<VetoFlag>();
    app.register_idle_veto(flag_veto);

    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Line);
    app.update();

    let threshold = app.world().resource::<InteractionTimer>().idle_threshold;

    // Veto active → stays Active across idle threshold.
    app.world_mut().resource_mut::<VetoFlag>().0 = true;
    app.world_mut()
        .resource_mut::<Time>()
        .advance_by(threshold + Duration::from_secs(1));
    app.update();
    app.update();
    assert_eq!(
        *app.world().resource::<State<SketchActivity>>().get(),
        SketchActivity::Active
    );

    // Veto cleared → next frame transitions to Idle.
    app.world_mut().resource_mut::<VetoFlag>().0 = false;
    app.update();
    app.update();
    assert_eq!(
        *app.world().resource::<State<SketchActivity>>().get(),
        SketchActivity::Idle
    );
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p wc-core --test lifecycle_idle_veto`
Expected: 3 tests pass.

### Task 14: Commit Phase A

- [ ] **Step 1: Stage**

```bash
git add \
    crates/wc-core/src/lifecycle/idle.rs \
    crates/wc-core/src/lifecycle/mod.rs \
    crates/wc-core/tests/lifecycle_idle_veto.rs
```

- [ ] **Step 2: Commit**

```bash
git commit -m "$(cat <<'EOF'
Plan 7 Phase A: idle veto hook

A new IdleVetoes resource lets sketches keep themselves Active past the
elapsed-time threshold while attractors decay. Sketches register a
fn(&World) -> bool via App::register_idle_veto; advance_activity
consults the list and stays Active if any veto fires.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

# Phase B — Multi-attractor data + sim

Replace the single-attractor `SimParams` with an attractor array (up to 8 entries; mouse is index 0). Mouse attractor lifecycle: power=10 on press, geometric decay (×0.9/frame on idle), zeroed below floor=2. WGSL iterates the array and accumulates force from every active entry.

### Task 15: Attractor struct + revised `SimParams`

**File:** `crates/wc-sketches/src/line/particle.rs`

- [ ] **Step 1: Add the `Attractor` struct**

In `crates/wc-sketches/src/line/particle.rs`, append after the existing `Particle` struct:

```rust
/// One gravitational attractor — position in world space + power (force scale).
///
/// `power == 0.0` means inactive; the simulate kernel skips zero-power entries.
/// 16-byte aligned (4 × f32) matching the WGSL `struct Attractor` layout.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct Attractor {
    /// World-space X/Y position.
    pub position: [f32; 2],
    /// Force scale. Mouse attractor uses power=10 at press, decays geometrically.
    pub power: f32,
    /// Padding to keep the struct 16-byte aligned (WGSL std140/storage rules).
    #[allow(
        clippy::pub_underscore_fields,
        reason = "GPU struct layout padding must be pub for bytemuck"
    )]
    pub _pad: f32,
}

/// Maximum simultaneous attractors. Index 0 is the mouse; indices 1..=N are
/// reserved for future Leap-tracked hands (Plan 11+).
pub const MAX_ATTRACTORS: usize = 8;
```

- [ ] **Step 2: Rewrite `SimParams`**

Replace the entire existing `SimParams` struct in the same file with:

```rust
/// Compute kernel uniforms pushed every frame.
///
/// Field order matches the WGSL `struct SimParams` in `simulate.wgsl` exactly;
/// the Rust layout is `#[repr(C)]` so `bytemuck::bytes_of` produces the
/// correct byte sequence. WGSL alignment for arrays-of-structs requires the
/// header fields ahead of the array to total a multiple of 16 bytes.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct SimParams {
    /// Frame time in seconds (capped to 50 ms to avoid blow-up on pauses).
    pub dt: f32,
    /// Number of attractors with `power > 0` to process. Capped at
    /// [`MAX_ATTRACTORS`]; bytes beyond `attractor_count` in `attractors` are
    /// ignored by the kernel.
    pub attractor_count: u32,
    /// Pulling drag baked via `pow(PULLING_DRAG_CONSTANT, fixed_dt)`. Active
    /// when at least one attractor has `power > 0`.
    pub pulling_drag_baked: f32,
    /// Inertial drag baked via `pow(INERTIAL_DRAG_CONSTANT, fixed_dt)`. Active
    /// when no attractors are active.
    pub inertial_drag_baked: f32,
    /// Multiplier on `gravity_constant` derived from canvas width. v4 uses
    /// `min(2^(width/836 - 1), 1)`; identical here.
    pub size_scale: f32,
    /// Per-particle fade-in duration in seconds.
    pub fade_duration: f32,
    /// Lower world-space bounds (x_min, y_min) for the constrain-to-box reset.
    pub constrain_min: [f32; 2],
    /// Upper world-space bounds (x_max, y_max).
    pub constrain_max: [f32; 2],
    /// Padding to bring the header to a 16-byte boundary before the array.
    /// The header above totals 40 bytes (six 4-byte scalars plus two 8-byte
    /// `vec2`s); we need 8 more to reach 48 (a multiple of 16) so the
    /// `attractors` array begins aligned.
    #[allow(
        clippy::pub_underscore_fields,
        reason = "GPU struct layout padding must be pub for bytemuck"
    )]
    pub _pad: [f32; 2],
    /// Attractor list. Entries `[0..attractor_count]` are live; the rest are
    /// zero-power and ignored.
    pub attractors: [Attractor; MAX_ATTRACTORS],
}
```

- [ ] **Step 3: Static-assert the size is a multiple of 16**

At the end of `crates/wc-sketches/src/line/particle.rs`, add:

```rust
const _: () = {
    assert!(std::mem::size_of::<SimParams>() % 16 == 0);
    assert!(std::mem::size_of::<Attractor>() % 16 == 0);
    assert!(std::mem::size_of::<Particle>() % 16 == 0);
};
```

> **If the compile-time assertion fails**, the implementer adjusts `_pad` until the size is a multiple of 16. Bevy's WGSL uniform binding requires 16-byte multiples; failure here means the WGSL pipeline will refuse to bind at runtime.

### Task 16: Update `simulate.wgsl` to iterate attractors

**File:** `assets/shaders/line/simulate.wgsl`

- [ ] **Step 1: Replace the file contents**

```wgsl
// Line particle simulation — one workgroup per 64 particles.
//
// Reads SimParams from a uniform buffer at @group(0) @binding(0).
// Reads + writes Particles in a storage buffer at @group(0) @binding(1).
//
// Each frame, every particle accumulates force from each attractor with
// `power > 0`, applies dual drag (pulling when any attractor is active,
// otherwise inertial), and integrates position. New particles fade in over
// `fade_duration` seconds; out-of-bounds particles teleport home.

struct Particle {
    position: vec2<f32>,
    velocity: vec2<f32>,
    original_xy: vec2<f32>,
    alpha: f32,
    _pad: f32,
};

struct Attractor {
    position: vec2<f32>,
    power: f32,
    _pad: f32,
};

const MAX_ATTRACTORS: u32 = 8u;

struct SimParams {
    dt: f32,
    attractor_count: u32,
    pulling_drag_baked: f32,
    inertial_drag_baked: f32,
    size_scale: f32,
    fade_duration: f32,
    constrain_min: vec2<f32>,
    constrain_max: vec2<f32>,
    _pad: vec2<f32>,
    attractors: array<Attractor, MAX_ATTRACTORS>,
};

@group(0) @binding(0) var<uniform> params: SimParams;
@group(0) @binding(1) var<storage, read_write> particles: array<Particle>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let idx = id.x;
    let count = arrayLength(&particles);
    if (idx >= count) {
        return;
    }
    var p = particles[idx];

    // --- Accumulate force from active attractors -------------------------
    // v4's particleSystem.ts: forceX = power * G * size_scale * dx / distance.
    // That's a CONSTANT-MAGNITUDE force in the unit direction toward the
    // attractor — distance-independent magnitude, only direction varies.
    // (Not inverse-square or inverse-linear; see v4 reference.)
    //
    // `mouse.power * gravity_constant` is already baked into `attractor.power`
    // host-side. Distance uses a small epsilon to avoid division by zero.
    var accel = vec2<f32>(0.0);
    let active_count = min(params.attractor_count, MAX_ATTRACTORS);
    for (var i: u32 = 0u; i < active_count; i = i + 1u) {
        let a = params.attractors[i];
        if (a.power <= 0.0) {
            continue;
        }
        let delta = a.position - p.position;
        let dist = max(length(delta), 1e-6);
        let dir = delta / dist;
        let force_mag = a.power * params.size_scale;
        accel = accel + dir * force_mag;
    }
    p.velocity = p.velocity + accel * params.dt;

    // --- Drag selection (pulling when any attractor active) --------------
    let drag = select(params.inertial_drag_baked,
                      params.pulling_drag_baked,
                      params.attractor_count > 0u);
    p.velocity = p.velocity * drag;

    // --- Euler integration -----------------------------------------------
    p.position = p.position + p.velocity * params.dt;

    // --- Constrain to box; reset to original on OOB ----------------------
    let oob = (p.position.x < params.constrain_min.x ||
               p.position.x > params.constrain_max.x ||
               p.position.y < params.constrain_min.y ||
               p.position.y > params.constrain_max.y);
    if (oob) {
        p.position = p.original_xy;
        p.velocity = vec2<f32>(0.0);
        p.alpha = 0.0; // re-fade-in
    }

    // --- Fade-in alpha ---------------------------------------------------
    if (p.alpha < 1.0 && params.fade_duration > 0.0) {
        p.alpha = min(1.0, p.alpha + params.dt / params.fade_duration);
    }

    particles[idx] = p;
}
```

- [ ] **Step 2: Build & launch**

Run: `cargo run -p waveconductor`
Expected: window opens, particles still visible. They will not yet flow correctly because Phase B does not yet wire the mouse attractor into the new array — that's Task 17. The point of this step is to confirm WGSL compiles and the pipeline binds with the new layout.

### Task 17: Mouse attractor state + per-frame array write

**File:** `crates/wc-sketches/src/line/systems.rs`

- [ ] **Step 1: Add `MouseAttractorState` resource**

At the top of `crates/wc-sketches/src/line/systems.rs`, after the existing `use` lines, add:

```rust
use super::particle::{Attractor, MAX_ATTRACTORS};

/// Lifecycle state for the mouse attractor — power that activates on click and
/// decays geometrically while held or after release. Matches v4's behavior:
/// `power=10` on press; each frame `power = floor + (power - floor) * 0.9`
/// down to `power < floor + epsilon`, then zero.
#[derive(Resource, Debug, Clone, Copy)]
pub struct MouseAttractorState {
    /// Current power. `0.0` = inactive.
    pub power: f32,
    /// World-space position (followed every frame the cursor moves).
    pub position: [f32; 2],
}

impl Default for MouseAttractorState {
    fn default() -> Self {
        Self {
            power: 0.0,
            position: [0.0, 0.0],
        }
    }
}

/// v4 `MOUSE_ATTRACTOR_POWER_DECAY_SPEED = 0.9`.
pub const MOUSE_POWER_DECAY: f32 = 0.9;
/// v4 `MOUSE_ATTRACTOR_POWER_DECAY_FLOOR = 2.0`. Power below `floor + ε` zeros.
pub const MOUSE_POWER_FLOOR: f32 = 2.0;
/// v4 `enableMouseAttractor`: `power = 10` on click.
pub const MOUSE_POWER_PRESS: f32 = 10.0;
```

- [ ] **Step 2: Add the press/move/decay system**

Append to the same file:

```rust
/// Tracks pointer button transitions and updates [`MouseAttractorState`].
///
/// - Just-pressed → set `power = MOUSE_POWER_PRESS`, position = cursor.
/// - Held / moving → update position only.
/// - Released → start decay (handled in [`decay_mouse_attractor`]).
pub fn update_mouse_attractor(
    pointer: Res<'_, PointerState>,
    mouse_buttons: Res<'_, bevy::input::ButtonInput<bevy::input::mouse::MouseButton>>,
    touches: Res<'_, bevy::input::touch::Touches>,
    window: Single<'_, &Window>,
    mut state: ResMut<'_, MouseAttractorState>,
) {
    let just_pressed = mouse_buttons.just_pressed(bevy::input::mouse::MouseButton::Left)
        || touches.iter_just_pressed().next().is_some();
    let held = mouse_buttons.pressed(bevy::input::mouse::MouseButton::Left)
        || touches.iter().next().is_some();

    if let Some(cursor_window) = pointer.primary {
        let w = window.width();
        let h = window.height();
        let wx = cursor_window.x - w * 0.5;
        let wy = -(cursor_window.y - h * 0.5);
        state.position = [wx, wy];

        if just_pressed {
            state.power = MOUSE_POWER_PRESS;
        } else if held && state.power < MOUSE_POWER_PRESS {
            // Keep power topped up while holding (matches v4's setGravityFocalPoint
            // running every mousemove that re-asserts the attractor).
            state.power = MOUSE_POWER_PRESS;
        }
    }
}

/// Decays the mouse attractor power each frame regardless of input state.
///
/// v4 runs this in the sketch's `animate()` regardless of idle state, so the
/// attractor's visual decay completes even after the user has stopped
/// interacting. Plan 8 will add the visual mesh; here only the physical power
/// matters.
pub fn decay_mouse_attractor(mut state: ResMut<'_, MouseAttractorState>) {
    if state.power <= 0.0 {
        return;
    }
    state.power = MOUSE_POWER_FLOOR + (state.power - MOUSE_POWER_FLOOR) * MOUSE_POWER_DECAY;
    if state.power < MOUSE_POWER_FLOOR + 1e-2 {
        state.power = 0.0;
    }
}
```

- [ ] **Step 3: Rewrite `update_sim_params` to populate the attractor array**

Replace the body of `update_sim_params` with:

```rust
pub fn update_sim_params(
    time: Res<'_, Time>,
    settings: Res<'_, LineSettings>,
    window: Single<'_, &Window>,
    mouse: Res<'_, MouseAttractorState>,
    mut sim: ResMut<'_, LineSimParams>,
) {
    use super::particle::{SimParams, MAX_ATTRACTORS};

    // --- Attractor list -------------------------------------------------
    let mut attractors = [Attractor::default(); MAX_ATTRACTORS];
    let mut attractor_count = 0_u32;
    if mouse.power > 0.0 {
        attractors[0] = Attractor {
            position: mouse.position,
            // Bake `gravity_constant` into the attractor's `power` so the
            // WGSL kernel can treat power uniformly across attractor sources.
            power: mouse.power * settings.gravity_constant,
            _pad: 0.0,
        };
        attractor_count = 1;
    }

    // --- Drag baking ----------------------------------------------------
    // v4 uses fixed_dt = 0.016 * 2 = 0.032. We bake against the same constant
    // so the per-frame drag matches v4 regardless of the actual render dt.
    let fixed_dt = 0.032_f32;
    let pulling_drag_baked = 0.93075095702_f32.powf(fixed_dt);
    let inertial_drag_baked = 0.53913643334_f32.powf(fixed_dt);

    // --- Size scaling (matches v4 sizeScaledGravityConstant) ------------
    let w = window.width();
    let size_scale = (2.0_f32.powf(w / 836.0 - 1.0)).min(1.0);

    // --- Constrain-to-box bounds (centered on origin, matching spawn) ---
    let h = window.height();
    let half_w = w * 0.5;
    let half_h = h * 0.5;
    let constrain_min = [-half_w, -half_h];
    let constrain_max = [half_w, half_h];

    sim.params = SimParams {
        dt: time.delta_secs().min(0.05),
        attractor_count,
        pulling_drag_baked,
        inertial_drag_baked,
        size_scale,
        fade_duration: 3.0, // v4 PARTICLE_SYSTEM_PARAMS.FADE_DURATION
        constrain_min,
        constrain_max,
        _pad: [0.0; 3],
        attractors,
    };
}
```

- [ ] **Step 4: Wire the new systems in `LinePlugin`**

In `crates/wc-sketches/src/line/mod.rs`, extend `LinePlugin::build` to:

```rust
        // Mouse attractor state (independent of sketch active/idle so the
        // attractor's decay continues during the screensaver-fade window).
        app.init_resource::<systems::MouseAttractorState>();
        app.add_systems(
            Update,
            (
                systems::update_mouse_attractor,
                systems::decay_mouse_attractor,
                systems::update_sim_params,
            )
                .chain()
                .run_if(sketch_active(AppState::Line)),
        );
```

Replace the existing `app.add_systems(Update, systems::update_sim_params.run_if(...))` block. Note: `decay_mouse_attractor` only fires while the sketch is `Active` — the idle veto (Phase E) keeps it `Active` so the decay completes.

- [ ] **Step 5: Run cargo check**

Run: `cargo check -p wc-sketches`
Expected: clean.

### Task 18: Commit Phase B

- [ ] **Step 1: Stage**

```bash
git add \
    crates/wc-sketches/src/line/particle.rs \
    crates/wc-sketches/src/line/systems.rs \
    crates/wc-sketches/src/line/mod.rs \
    assets/shaders/line/simulate.wgsl
```

- [ ] **Step 2: Commit**

```bash
git commit -m "$(cat <<'EOF'
Plan 7 Phase B: multi-attractor sim with mouse-power decay

SimParams replaces single-attractor fields with an Attractor[MAX_ATTRACTORS]
array; the WGSL kernel iterates it and accumulates force from each entry.
A new MouseAttractorState resource tracks power and position; it ramps to
10 on click and decays geometrically (0.9/frame down to floor=2) so the
attractor's pull fades smoothly after release.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

# Phase C — Physics parity + CPU mirror

Particle struct gains `original_xy` and `alpha`. The WGSL kernel already accounts for these (Phase B). Now: spawn populates the new fields, the render shader uses `alpha`, and a pure-Rust CPU mirror runs alongside the GPU sim so Plan 9 has a readable source for `ParticleStats`.

### Task 19: Extend `Particle` struct

**File:** `crates/wc-sketches/src/line/particle.rs`

- [ ] **Step 1: Replace the `Particle` struct**

Replace the existing `Particle` definition with:

```rust
/// Per-particle state. Position + velocity in 2D world-space (centered on
/// origin), plus the original spawn position (for constrain-to-box reset) and
/// the fade-in α.
///
/// 32-byte aligned (8 × f32, the trailing `_pad` brings the struct to a
/// 16-byte multiple) — see the WGSL `struct Particle` in `simulate.wgsl` and
/// `render.wgsl`.
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct Particle {
    /// World-space X/Y position (current).
    pub position: [f32; 2],
    /// X/Y velocity in world units per second.
    pub velocity: [f32; 2],
    /// Spawn position; OOB particles teleport here.
    pub original_xy: [f32; 2],
    /// Fade-in alpha, ramps 0 → 1 over `SimParams.fade_duration` seconds.
    pub alpha: f32,
    /// Padding to keep the struct multiple-of-16 aligned for WGSL storage rules.
    #[allow(
        clippy::pub_underscore_fields,
        reason = "GPU struct layout padding must be pub for bytemuck"
    )]
    pub _pad: f32,
}
```

The existing `const _: () = { assert!(...) };` block at the file footer keeps the alignment check.

### Task 20: Update `render.wgsl` to use the new layout

**File:** `assets/shaders/line/render.wgsl`

- [ ] **Step 1: Replace the file**

```wgsl
// Line particle render — one quad per particle, driven by vertex_index.
//
// Particle storage buffer at @group(2) @binding(0) (Bevy Material2d convention).

#import bevy_sprite::mesh2d_view_bindings::view

struct Particle {
    position: vec2<f32>,
    velocity: vec2<f32>,
    original_xy: vec2<f32>,
    alpha: f32,
    _pad: f32,
};

@group(2) @binding(0) var<storage, read> particles: array<Particle>;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) brightness: f32,
    @location(1) alpha: f32,
};

// Half-size of each quad in world units.
const QUAD_HALF: f32 = 1.5;

fn quad_corner(corner: u32) -> vec2<f32> {
    switch corner {
        case 0u: { return vec2<f32>(-QUAD_HALF, -QUAD_HALF); }
        case 1u: { return vec2<f32>( QUAD_HALF, -QUAD_HALF); }
        case 2u: { return vec2<f32>( QUAD_HALF,  QUAD_HALF); }
        case 3u: { return vec2<f32>(-QUAD_HALF, -QUAD_HALF); }
        case 4u: { return vec2<f32>( QUAD_HALF,  QUAD_HALF); }
        default: { return vec2<f32>(-QUAD_HALF,  QUAD_HALF); }
    }
}

@vertex
fn vertex(
    @builtin(vertex_index) vertex_index: u32,
    @location(0) local_pos: vec3<f32>,
) -> VertexOutput {
    let particle_index = vertex_index / 6u;
    let corner_index   = vertex_index % 6u;

    let p = particles[particle_index];
    let corner = quad_corner(corner_index);
    let world_pos = vec4<f32>(p.position + corner, 0.0, 1.0);

    var out: VertexOutput;
    out.clip_position = view.clip_from_world * world_pos;
    out.brightness = clamp(length(p.velocity) * 0.005, 0.05, 1.0);
    out.alpha = p.alpha;
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let b = in.brightness;
    return vec4<f32>(b, b * 0.85, b * 0.6, in.alpha);
}
```

### Task 21: Update `spawn_line` to populate the new fields

**File:** `crates/wc-sketches/src/line/systems.rs`

- [ ] **Step 1: Update the spawn body**

In `spawn_line`, replace the existing `initial.push(...)` block with:

```rust
        initial.push(Particle {
            position: [x * spacing, y * spacing],
            velocity: [0.0, 0.0],
            original_xy: [x * spacing, y * spacing],
            alpha: 0.0,
            _pad: 0.0,
        });
```

(Phase D will replace the grid layout with a horizontal line. Don't refactor the layout yet — keep the grid one phase longer so Phase B/C changes can be verified in isolation against the existing visual.)

- [ ] **Step 2: Build + launch**

Run: `cargo run -p waveconductor`
Expected: particles still visible, now fading in from α=0 over 3 seconds. Pressing the mouse should pull them in; releasing should let them drift with `INERTIAL_DRAG`.

### Task 22: CPU mirror

**Files:** `crates/wc-sketches/src/line/sim_cpu.rs` (new), `crates/wc-sketches/src/line/mod.rs`, `crates/wc-sketches/src/line/systems.rs`

The CPU mirror is a parallel implementation of the same physics. It is *not* consulted for rendering; it exists so Plan 9 has a deterministic CPU-side particle buffer to feed `ParticleStats` without a GPU readback.

- [ ] **Step 1: Create the mirror module**

Create `crates/wc-sketches/src/line/sim_cpu.rs`:

```rust
//! CPU-side particle integrator — a parallel implementation of the WGSL
//! kernel in `assets/shaders/line/simulate.wgsl`.
//!
//! Used by Plan 9's [`ParticleStats`] computation as a readable source for
//! per-particle velocities (avoiding a GPU readback stall). The GPU sim
//! remains authoritative for rendering; the two integrators run independently
//! and may drift by ≤1% due to floating-point order-of-operations, which is
//! acceptable for `groupedUpness` and other smooth scalars.

use bevy::prelude::*;

use super::particle::{Attractor, Particle, SimParams, MAX_ATTRACTORS};

/// CPU mirror of the particle storage buffer.
///
/// Populated by [`crate::line::systems::spawn_line`] with the same grid the
/// GPU buffer starts from, then stepped each `Update` by [`step_cpu_mirror`].
#[derive(Resource, Default)]
pub struct LineCpuMirror {
    /// Particle state in the same layout as the GPU buffer.
    pub particles: Vec<Particle>,
}

/// Step the CPU mirror by one frame. The math mirrors the WGSL kernel
/// exactly; if you change one, change both, and re-check the parity test in
/// `crates/wc-sketches/tests/line_lifecycle.rs`.
pub fn step_cpu_mirror(mut mirror: ResMut<'_, LineCpuMirror>, sim: Res<'_, super::compute::LineSimParams>) {
    let params = sim.params;
    for p in mirror.particles.iter_mut() {
        step_one(p, &params);
    }
}

/// Pure function: step a single particle. Extracted for unit testing.
pub fn step_one(p: &mut Particle, params: &SimParams) {
    // Accumulate force. v4: constant-magnitude in unit direction toward attractor.
    let mut accel = [0.0_f32, 0.0];
    let active_count = (params.attractor_count as usize).min(MAX_ATTRACTORS);
    for a in &params.attractors[..active_count] {
        if a.power <= 0.0 {
            continue;
        }
        let dx = a.position[0] - p.position[0];
        let dy = a.position[1] - p.position[1];
        let dist = (dx * dx + dy * dy).sqrt().max(1e-6);
        let inv_dist = 1.0 / dist;
        let force_mag = a.power * params.size_scale;
        accel[0] += dx * inv_dist * force_mag;
        accel[1] += dy * inv_dist * force_mag;
    }
    p.velocity[0] += accel[0] * params.dt;
    p.velocity[1] += accel[1] * params.dt;

    // Drag.
    let drag = if params.attractor_count > 0 {
        params.pulling_drag_baked
    } else {
        params.inertial_drag_baked
    };
    p.velocity[0] *= drag;
    p.velocity[1] *= drag;

    // Integrate.
    p.position[0] += p.velocity[0] * params.dt;
    p.position[1] += p.velocity[1] * params.dt;

    // Constrain.
    let oob = p.position[0] < params.constrain_min[0]
        || p.position[0] > params.constrain_max[0]
        || p.position[1] < params.constrain_min[1]
        || p.position[1] > params.constrain_max[1];
    if oob {
        p.position = p.original_xy;
        p.velocity = [0.0, 0.0];
        p.alpha = 0.0;
    }

    // Fade.
    if p.alpha < 1.0 && params.fade_duration > 0.0 {
        p.alpha = (p.alpha + params.dt / params.fade_duration).min(1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zero_attractor_params() -> SimParams {
        SimParams {
            dt: 0.016,
            attractor_count: 0,
            pulling_drag_baked: 0.9,
            inertial_drag_baked: 0.5,
            size_scale: 1.0,
            fade_duration: 3.0,
            constrain_min: [-100.0, -100.0],
            constrain_max: [100.0, 100.0],
            _pad: [0.0; 3],
            attractors: [Attractor::default(); MAX_ATTRACTORS],
        }
    }

    #[test]
    fn no_attractors_uses_inertial_drag() {
        let params = zero_attractor_params();
        let mut p = Particle {
            position: [0.0, 0.0],
            velocity: [10.0, 0.0],
            original_xy: [0.0, 0.0],
            alpha: 1.0,
            _pad: 0.0,
        };
        step_one(&mut p, &params);
        // Inertial drag = 0.5, applied to velocity before integration.
        assert!((p.velocity[0] - 5.0).abs() < 1e-5, "got {}", p.velocity[0]);
    }

    #[test]
    fn one_attractor_pulls_particle() {
        let mut params = zero_attractor_params();
        params.attractor_count = 1;
        params.attractors[0] = Attractor {
            position: [100.0, 0.0],
            power: 1000.0,
            _pad: 0.0,
        };
        let mut p = Particle {
            position: [0.0, 0.0],
            velocity: [0.0, 0.0],
            original_xy: [0.0, 0.0],
            alpha: 1.0,
            _pad: 0.0,
        };
        step_one(&mut p, &params);
        assert!(p.velocity[0] > 0.0, "should accelerate toward attractor");
    }

    #[test]
    fn oob_resets_to_original() {
        let mut params = zero_attractor_params();
        params.constrain_min = [-10.0, -10.0];
        params.constrain_max = [10.0, 10.0];
        let mut p = Particle {
            position: [50.0, 0.0],
            velocity: [10.0, 0.0],
            original_xy: [-5.0, 2.5],
            alpha: 1.0,
            _pad: 0.0,
        };
        step_one(&mut p, &params);
        assert_eq!(p.position, [-5.0, 2.5]);
        assert_eq!(p.velocity, [0.0, 0.0]);
        assert_eq!(p.alpha, 0.0);
    }

    #[test]
    fn alpha_fades_in() {
        let params = zero_attractor_params();
        let mut p = Particle {
            position: [0.0, 0.0],
            velocity: [0.0, 0.0],
            original_xy: [0.0, 0.0],
            alpha: 0.0,
            _pad: 0.0,
        };
        step_one(&mut p, &params);
        let expected = params.dt / params.fade_duration;
        assert!((p.alpha - expected).abs() < 1e-6, "got {}", p.alpha);
    }
}
```

- [ ] **Step 2: Add the module + initialize the resource**

In `crates/wc-sketches/src/line/mod.rs`, add `pub mod sim_cpu;` to the module list. In `LinePlugin::build`, after `app.init_resource::<systems::MouseAttractorState>();`, add:

```rust
        app.init_resource::<sim_cpu::LineCpuMirror>();
```

In the same plugin build, append `sim_cpu::step_cpu_mirror` to the gated `Update` system tuple:

```rust
        app.add_systems(
            Update,
            (
                systems::update_mouse_attractor,
                systems::decay_mouse_attractor,
                systems::update_sim_params,
                sim_cpu::step_cpu_mirror,
            )
                .chain()
                .run_if(sketch_active(AppState::Line)),
        );
```

- [ ] **Step 3: Populate the mirror in `spawn_line`**

In `crates/wc-sketches/src/line/systems.rs`, in `spawn_line`, before the existing `commands.insert_resource(LineSimParams { ... });` line, add:

```rust
    commands.insert_resource(super::sim_cpu::LineCpuMirror {
        particles: initial.clone(),
    });
```

(The `clone()` is a one-shot allocation at sketch entry, not per-frame — acceptable.)

- [ ] **Step 4: Clear the mirror in `remove_sim_params`**

In `crates/wc-sketches/src/line/mod.rs`, the existing `remove_sim_params` system runs on `OnExit(AppState::Line)`. Extend it to clear the mirror:

```rust
fn remove_sim_params(mut commands: Commands<'_, '_>) {
    commands.remove_resource::<compute::LineSimParams>();
    commands.remove_resource::<sim_cpu::LineCpuMirror>();
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p wc-sketches`
Expected: pass, including 4 new `sim_cpu` tests.

### Task 23: Commit Phase C

- [ ] **Step 1: Stage**

```bash
git add \
    crates/wc-sketches/src/line/particle.rs \
    crates/wc-sketches/src/line/sim_cpu.rs \
    crates/wc-sketches/src/line/systems.rs \
    crates/wc-sketches/src/line/mod.rs \
    assets/shaders/line/render.wgsl
```

- [ ] **Step 2: Commit**

```bash
git commit -m "$(cat <<'EOF'
Plan 7 Phase C: physics parity + CPU mirror

Particle gains original_xy and alpha (with WGSL kernel already wired in
Phase B). render.wgsl forwards alpha to fragment for fade-in visibility.
A new sim_cpu module is a pure-Rust port of the same physics, run as a
LineCpuMirror resource alongside the GPU sim — Plan 9 will read it for
ParticleStats without a GPU readback stall.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

# Phase D — Spawn shape + density setting

Replace the grid spawn with v4's horizontal-line layout. Replace `particle_count: u32` with `particle_density: f32`. Window-width drives the actual count.

### Task 24: Settings: `particle_density` replaces `particle_count`

**File:** `crates/wc-sketches/src/line/settings.rs`

- [ ] **Step 1: Replace the struct**

```rust
//! Line sketch settings.
//!
//! Curated knobs that show up in the user panel. v4 exposes two: particle
//! density and the gravity constant. Plan 7 mirrors that. Drag and attractor
//! radius existed as v5-only knobs during Plan 6 (the inverse-linear gravity
//! era); Plan 7 baked drag into [`crate::line::compute::SimParams`] from
//! fixed v4 constants and made the force constant-magnitude (no radius
//! needed), so both fields are dropped.
//!
//! - **`particle_density`** — particles per canvas-pixel of width. v4 uses 10
//!   (so a 1280px window has ~12,800 particles). Restart on change (the
//!   compute pipeline rebuilds its storage buffer).
//! - **`gravity_constant`** — strength of the pull toward attractors (v4
//!   `GRAVITY_CONSTANT`, default 280).

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "line")]
pub struct LineSettings {
    /// Particles per canvas-pixel of width. Restart on change.
    #[setting(
        default = 10.0_f32,
        min = 0.1_f32,
        max = 30.0_f32,
        step = 0.5_f32,
        category = User,
        requires_restart
    )]
    pub particle_density: f32,

    /// Strength of the pull toward the pointer attractor. v4 default = 280.
    #[setting(default = 280.0_f32, min = 0.0_f32, max = 1000.0_f32, step = 10.0_f32, category = User)]
    pub gravity_constant: f32,
}
```

Removing fields is forward-compatible: `serde` ignores unknown keys when loading persisted TOML, so users upgrading from v5-line keep their `gravity_constant` and pick up the new default for `particle_density`.

### Task 25: Horizontal-line spawn with sawtooth jitter

**File:** `crates/wc-sketches/src/line/systems.rs`

- [ ] **Step 1: Add `Single<&Window>` to the spawn signature**

In `spawn_line`, add `window: Single<'_, &Window>,` as a parameter.

- [ ] **Step 2: Replace the initial-positions loop**

Replace the existing `let side = ...; let spacing = ...; for i in 0..count { ... }` block with:

```rust
    let w = window.width();
    let h = window.height();
    let half_w = w * 0.5;
    let mid_y = 0.0_f32; // window-centered world

    // v4 particleDensity = 10 per canvas-pixel of width. Derive count from
    // density × width, clamping to a sane range (avoids massive resize spikes).
    #[allow(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "density × width is positive and bounded by clamp"
    )]
    let count = ((settings.particle_density * w).round() as u32).clamp(100, 100_000);

    let mut initial: Vec<Particle> = Vec::with_capacity(count as usize);
    for i in 0..count {
        // Evenly space across the window width, centered on origin.
        let x = (i as f32 / count as f32) * w - half_w;
        // v4: subtle sawtooth Y-jitter `((i % 5) - 2) * 2` so particles sit on
        // five stacked horizontal strands rather than a single line.
        let jitter_strand = (i % 5) as f32 - 2.0;
        let y = mid_y + jitter_strand * 2.0;
        initial.push(Particle {
            position: [x, y],
            velocity: [0.0, 0.0],
            original_xy: [x, y],
            alpha: 0.0,
            _pad: 0.0,
        });
    }
```

The rest of `spawn_line` (mesh allocation, buffer upload, entity spawn) is unchanged, except update the `particle_count` reference inside the `LineSimParams` and `LineCpuMirror` inserts to use `count`.

- [ ] **Step 3: Update references to `particle_count` throughout the crate**

Across `crates/wc-sketches/src/line/`, replace `settings.particle_count` references with the derived `count` computed at spawn time. The compute pipeline already takes its count from `LineSimParams.particle_count`, which `spawn_line` populates.

In `crates/wc-sketches/tests/line_lifecycle.rs`, update `line_settings_resource_inserted` to check `particle_density` instead:

```rust
    assert!(
        settings.particle_density > 0.0,
        "particle_density should default > 0, got {}",
        settings.particle_density
    );
```

- [ ] **Step 4: Build + run**

Run: `cargo run -p waveconductor`
Expected: particles spawn in a horizontal line across the middle of the window with subtle vertical strands. Density visibly higher than the prior grid (~12k particles vs 5k).

### Task 26: Commit Phase D

- [ ] **Step 1: Stage**

```bash
git add \
    crates/wc-sketches/src/line/settings.rs \
    crates/wc-sketches/src/line/systems.rs \
    crates/wc-sketches/tests/line_lifecycle.rs
```

- [ ] **Step 2: Commit**

```bash
git commit -m "$(cat <<'EOF'
Plan 7 Phase D: particle_density setting + horizontal-line spawn

particle_count (absolute) becomes particle_density (per canvas-px),
matching v4's `particleDensity = 10`. Spawn switches from a centered
grid to a horizontal line at mid-Y with subtle 5-strand sawtooth jitter.
The `drag` field was removed entirely from `LineSettings` (not moved to
Dev) because v4's drag constants are baked into `SimParams` at fixed values
and are not user-tunable. The plan doc originally said "moves to Dev" but
the implementation dropped the field outright; this message has been
corrected to reflect reality.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

# Phase E — Line idle veto + lifecycle test

The mouse attractor decays over multiple frames after release. Without a veto, the sketch transitions to `Idle` at the 30s mark while particles are still in motion. Register a veto that keeps Line `Active` while `MouseAttractorState.power > 0`.

### Task 27: Register Line's idle veto

**File:** `crates/wc-sketches/src/line/mod.rs`

- [ ] **Step 1: Define the veto**

In `crates/wc-sketches/src/line/mod.rs`, add at module scope:

```rust
/// Idle veto for the Line sketch. Returns `true` while the mouse attractor's
/// power is non-zero (i.e., still decaying) — keeps the sketch in
/// `SketchActivity::Active` so [`systems::decay_mouse_attractor`] continues to
/// fire until the attractor is fully released.
fn line_idle_veto(world: &World) -> bool {
    world
        .get_resource::<systems::MouseAttractorState>()
        .is_some_and(|s| s.power > 0.0)
}
```

- [ ] **Step 2: Register the veto in `LinePlugin::build`**

Insert into `LinePlugin::build`, after `app.init_resource::<systems::MouseAttractorState>();`:

```rust
        use wc_core::lifecycle::RegisterIdleVetoExt;
        app.register_idle_veto(line_idle_veto);
```

### Task 28: Lifecycle test

**File:** `crates/wc-sketches/tests/line_lifecycle.rs`

- [ ] **Step 1: Add the veto test**

Append to the file:

```rust
#[test]
fn idle_veto_keeps_line_active_during_attractor_decay() {
    use wc_core::lifecycle::idle::InteractionTimer;
    use wc_sketches::line::systems::MouseAttractorState;

    let mut app = build_app();
    app.update();

    // Enter Line. LinePlugin registers the veto in build().
    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Line);
    app.update();
    app.update();

    // Simulate a click that left the attractor in mid-decay (power > 0 < press).
    app.world_mut().resource_mut::<MouseAttractorState>().power = 5.0;

    // Advance time past idle_threshold.
    let threshold = app.world().resource::<InteractionTimer>().idle_threshold;
    app.world_mut()
        .resource_mut::<Time>()
        .advance_by(threshold + std::time::Duration::from_secs(1));
    app.update();
    app.update();

    let activity = app.world().resource::<State<SketchActivity>>();
    assert_eq!(
        *activity.get(),
        SketchActivity::Active,
        "Line should stay Active while mouse attractor is still decaying"
    );
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p wc-sketches --test line_lifecycle idle_veto_keeps_line_active_during_attractor_decay`
Expected: pass.

### Task 29: Commit Phase E

- [ ] **Step 1: Stage**

```bash
git add \
    crates/wc-sketches/src/line/mod.rs \
    crates/wc-sketches/tests/line_lifecycle.rs
```

- [ ] **Step 2: Commit**

```bash
git commit -m "$(cat <<'EOF'
Plan 7 Phase E: Line idle veto

LinePlugin registers a fn(&World) -> bool veto that returns true while
MouseAttractorState.power > 0. The lifecycle's advance_activity now
respects the veto, keeping Line in SketchActivity::Active across the
30-second idle threshold while the mouse attractor finishes decaying.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

# Phase F — Push and tag

### Task 30: Verify CI green, push, tag

- [ ] **Step 1: Run the full workspace test suite locally**

Run: `cargo test --workspace`
Expected: pass.

- [ ] **Step 2: Run fmt + clippy**

Run: `cargo fmt --all -- --check`
Expected: clean.

Run: `cargo clippy --workspace -- -D warnings`
Expected: clean.

- [ ] **Step 3: Push**

```bash
git push origin rewrite/bevy
```

- [ ] **Step 4: Wait for CI green across the matrix**

The Plan 1 CI workflow runs: fmt, clippy, check-secrets, deny, audit, test, doc on Ubuntu and Windows. Wait for all 10 jobs to succeed. If any job fails, fix the root cause on a follow-up commit before tagging.

- [ ] **Step 5: Tag**

```bash
git tag v5-line-sim
git push origin v5-line-sim
```

- [ ] **Step 6: Update the roadmap status row**

In `docs/superpowers/roadmap.md`, change the Plan 7 row from `⏳ next` to `✅ shipped` and set the tag column to `` `v5-line-sim` ``. Bump Plan 8 to `⏳ next`.

```bash
git add docs/superpowers/roadmap.md
git commit -m "$(cat <<'EOF'
roadmap: Plan 7 shipped, Plan 8 is next

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
git push origin rewrite/bevy
```

---

## Self-review checklist

After completing all phases:

- [ ] Every `requires_restart` field change still produces a visible reload of the sketch (Phase 0 Task 3 + Phase D Task 24 both touch this).
- [ ] `cargo test --workspace` passes.
- [ ] `cargo clippy --workspace -- -D warnings` passes.
- [ ] `cargo fmt --all -- --check` passes.
- [ ] No allocations in the per-frame `update_sim_params` (all stack-allocated arrays + writes into the resource).
- [ ] No `unwrap()` / `expect()` introduced outside test code.
- [ ] No real personal information, email addresses, or `/Users/...` paths in code or comments.
- [ ] WGSL `SimParams` struct size is a multiple of 16 (the `const _: () = { assert!(...) }` block at the file footer verifies).
- [ ] CPU mirror is updated in lockstep with WGSL via shared `super::particle` types.
- [ ] `PARITY.md` reflects the new Plan 7–10 split.
- [ ] Roadmap status row updated.

## Carry-forwards for Plan 8

Items surfaced during Plan 7 review or testing that don't fit this plan's scope. Append to `docs/superpowers/next-plan-carry-forwards.md` as they appear:

- *(populated during review)*

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-25-v5-plan-7-line-simulation.md`. Two execution options:

**1. Subagent-Driven (recommended)** — dispatch a fresh subagent per phase, two-stage review (spec then code quality) between phases, fast iteration.

**2. Inline Execution** — execute tasks in this session using `superpowers:executing-plans`, batch execution with checkpoints for human review.

Which approach?
