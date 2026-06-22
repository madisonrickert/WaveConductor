# Dots D2 — Scaffold + grid spawn + sim + mouse attractor — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Stand up the Dots ("Fabric") sketch on the shared `particles/` foundation: a full-screen grid of star particles that fades in, runs the gravity sim with Dots' parameters (including the stationary spring), and is pulled by the mouse/touch pointer.

**Architecture:** Dots is a new self-contained sketch module `crates/wc-sketches/src/dots/` that consumes the shared `crate::particles` engine (compute harness, `Particle`/`SimParams`, `ParticleMaterial`, `simulate.wgsl`/`render.wgsl`). It supplies only what diverged from Line in v4: the grid spawn layout, Dots' `SimParams` values, and its own mouse attractor (different press power). The shared `ParticleComputePlugin` + `Material2dPlugin::<ParticleMaterial>` are hoisted from `LinePlugin` to the `SketchesPlugin` umbrella so both sketches share one registration. `AppState::Dots` already exists in `SKETCH_ORDER`.

**Tech Stack:** Rust, Bevy 0.19, the shared `crate::particles` module (built in D1), `wc_core_macros::SketchSettings`, `cargo nextest`.

## Global Constraints

- **Bevy 0.19.** Dots consumes the shared `crate::particles::{compute::{ParticleComputePlugin, ParticleSimParams}, particle::{Particle, Attractor, SimParams, MAX_ATTRACTORS}, material::ParticleMaterial, sim_cpu::CpuMirror}` — do NOT duplicate those; import them.
- **v4 Dots parameter values (from `.worktrees/v4/src/sketches/dots/index.ts`), use verbatim:** `GRAVITY_CONSTANT = 100`, `PULLING_DRAG_CONSTANT = 0.96075095702`, `INERTIAL_DRAG_CONSTANT = 0.23913643334`, `STATIONARY_CONSTANT = 0.01`, `FADE_DURATION = 3.0`, `timeStep = 0.016 * 3 = 0.048`, `constrainToBox = false`. Grid: `EXTENT = 10` cells of bleed, `dotSpacing` default `20`. Mouse attractor: press `power = 1`, decay floor `2.0`, decay speed `0.9`, release `power = 0`.
- **Self-contained sketch:** Dots' input/spawn/params systems live under `crates/wc-sketches/src/dots/`. Mirror the *structure* of the Line equivalents (read them) but with Dots values; do not import Line's `systems`/`settings` modules. (Sharing Line's mouse-attractor/param-baker is explicitly out of scope — the spec scoped the shared foundation to the engine only.)
- **`#[serde(default = "default_<name>")]` on every settings field** (carry-forward #57) so legacy TOML deserializes.
- **No `unwrap()`/`expect()`** in non-test code unless a documented invariant. **No `as` casts** where `TryFrom` works (the spawn/window-sizing casts may use the same `#[allow(...)]` pattern Line's `spawn.rs` uses, with the same `reason`).
- **`///`/`//!` docs** on every public item / module root.
- **Never allocate in a hot path** (the per-frame sim_params writer, the mouse systems). The one-shot spawn `clone()` for the CpuMirror snapshot is fine (mirrors Line's `spawn.rs`).
- **Verification gates:** `cargo fmt --all -- --check`; `cargo clippy --all-targets --all-features --workspace -- -D warnings`; `cargo nextest run --workspace --all-features` + `cargo test --doc --workspace`; `cargo doc --no-deps --workspace --document-private-items`; `cargo xtask check-secrets`. Dev iteration is `cargo rund` (interactive — the controller defers it to the operator; do NOT run it in a subagent).
- **Commit messages:** backticks shell-substitute — use `git commit -F` or avoid backticks.

## Reference material (read these)

- v4 source: `/Users/madison/Developer/WaveConductor/.worktrees/v4/src/sketches/dots/index.ts` (params, grid spawn loop, createAttractor/moveAttractor/removeAttractor, the step() decay).
- Line patterns to mirror (read for structure, swap values): `crates/wc-sketches/src/line/mod.rs` (plugin wiring, lifecycle, idle veto, manifest), `crates/wc-sketches/src/line/settings.rs` (SketchSettings derive + per-field serde defaults), `crates/wc-sketches/src/line/systems/spawn.rs` (buffer alloc, mesh, `ParticleMaterial` spawn, `CpuMirror` + `ParticleSimParams` insert), `crates/wc-sketches/src/line/systems/sim_params.rs` (the `bake_sim_params` writer — drag baking against fixed dt, size_scale, attractor array, constrain bounds), `crates/wc-sketches/src/line/systems/mouse.rs` (`MouseAttractorState` + update/decay).
- `crates/wc-sketches/src/lib.rs` (`SketchesPlugin` umbrella), `crates/wc-core/src/sketch/manifest.rs` (`register_sketch_manifest`), `crates/wc-core/src/lifecycle/state.rs` (`AppState::Dots`).

---

### Task 1: Dots module scaffold — plugin, settings, manifest; hoist shared plugins to the umbrella

**Files:**
- Create: `crates/wc-sketches/src/dots/mod.rs` (`DotsPlugin`)
- Create: `crates/wc-sketches/src/dots/settings.rs` (`DotsSettings`)
- Modify: `crates/wc-sketches/src/lib.rs` (`pub mod dots;`; register `DotsPlugin`; hoist `ParticleComputePlugin` + `Material2dPlugin::<ParticleMaterial>` into `SketchesPlugin`)
- Modify: `crates/wc-sketches/src/line/mod.rs` (REMOVE the `app.add_plugins(ParticleComputePlugin)` and `Material2dPlugin::<…ParticleMaterial>` registrations — now owned by the umbrella)
- Asset: ensure `assets/sketches/dots/screenshot.png` exists for the manifest tile (copy the v4 `dots2.png`/`dots1.png` to a scrubbed PNG if not present; PNG only — no `jpeg` feature). If the asset can't be created, register the manifest without erroring (the tile shows the placeholder until the image loads, exactly like Line).

**Interfaces:**
- Produces: `crate::dots::DotsPlugin` (registered by `SketchesPlugin`); `crate::dots::settings::DotsSettings { dot_spacing: f32, gamma: f32 }` with `STORAGE_KEY` and `Default`. Manifest entry for `AppState::Dots`, display name `"Fabric"`.
- Consumes: `crate::particles::compute::ParticleComputePlugin`, `crate::particles::material::ParticleMaterial` (now registered by the umbrella).

- [ ] **Step 1: Write `DotsSettings` (mirror `LineSettings`'s derive)**

Read `crates/wc-sketches/src/line/settings.rs` for the exact `#[derive(SketchSettings)]` usage, the `#[setting(...)]` attribute syntax (category, label, requires_restart, step), the per-field `#[serde(default = "default_<name>")]` pattern, the `STORAGE_KEY`, and the `Default` impl. Create `crates/wc-sketches/src/dots/settings.rs` with:

- `dot_spacing: f32` — Dev category, label "Dot spacing (px)", `requires_restart`, default `20.0`, a sensible min (e.g. `4.0`) so a tiny value can't allocate a runaway particle count.
- `gamma: f32` — Dev category, label "Gamma", `requires_restart`, `step = 0.1`, default `1.0`.

Both with `default_dot_spacing()` / `default_gamma()` free functions and `#[serde(default = "...")]`. Add a module `//!` doc citing v4 `dots/index.ts` `static settings`.

- [ ] **Step 2: Write `DotsPlugin` scaffold + manifest**

Create `crates/wc-sketches/src/dots/mod.rs` mirroring `LinePlugin::build`'s *structure* (read `line/mod.rs`) but minimal for D2: register `DotsSettings` via `register_sketch_settings`, register the manifest (`register_sketch_manifest` with `state: AppState::Dots`, `display_name: "Fabric"`, screenshot `assets/sketches/dots/screenshot.png` via `AssetServer`). Factor the manifest registration into a `register_dots_manifest(app: &mut App)` free function so it's unit-testable under `MinimalPlugins` (mirror `register_line_manifest`). Leave the spawn/sim/mouse wiring as a TODO comment for Tasks 2–3 (or stub `OnEnter`/`OnExit` empty for now). Add the `//!` module doc describing the data flow (mirror Line's, trimmed).

- [ ] **Step 3: Hoist shared plugins + register `DotsPlugin`**

In `crates/wc-sketches/src/lib.rs`: add `pub mod dots;`. In `SketchesPlugin::build`, add (once, before the sketch plugins) `app.add_plugins(crate::particles::compute::ParticleComputePlugin)` and `app.add_plugins(Material2dPlugin::<crate::particles::material::ParticleMaterial>::default())`, then `app.add_plugins(line::LinePlugin)` and `app.add_plugins(dots::DotsPlugin)`. In `crates/wc-sketches/src/line/mod.rs`, REMOVE the two corresponding `add_plugins` lines (the umbrella now owns them) and update the surrounding comments. (`Material2dPlugin` import may need to move to `lib.rs`.)

- [ ] **Step 4: Unit test the manifest registration**

Mirror `register_line_manifest_appends_entry` (in `line/mod.rs` tests): a test that builds a `MinimalPlugins` app + `AssetPlugin` + `ImagePlugin`, calls `register_dots_manifest`, and asserts the `SketchManifest` entry for `AppState::Dots` has `display_name == "Fabric"`. Put it in `dots/mod.rs`'s `#[cfg(test)] mod tests`.

- [ ] **Step 5: Build, test, gates**

```bash
cargo build -p wc-sketches --all-features
cargo nextest run -p wc-sketches --all-features
cargo clippy -p wc-sketches --all-targets --all-features -- -D warnings
```

Expected: PASS, including the new manifest test and ALL existing Line tests (the plugin hoist must not break Line — Line's compute/material still register, just from the umbrella).

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -F <(printf '%s\n' "feat(dots): scaffold DotsPlugin + settings + Fabric manifest; hoist shared particle plugins" "" "ParticleComputePlugin + Material2dPlugin::<ParticleMaterial> hoisted from LinePlugin to the SketchesPlugin umbrella so Line and Dots share one registration." "" "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>")
```

---

### Task 2: Grid spawn + Dots sim params + render (static fading-in grid)

**Files:**
- Create: `crates/wc-sketches/src/dots/systems/mod.rs` (module root re-exporting the submodules + `DotsRoot` marker)
- Create: `crates/wc-sketches/src/dots/systems/spawn.rs` (grid spawn)
- Create: `crates/wc-sketches/src/dots/systems/sim_params.rs` (per-frame `ParticleSimParams` writer with Dots constants)
- Modify: `crates/wc-sketches/src/dots/mod.rs` (wire `OnEnter`/`OnExit` + the `Update` sim-params system; declare `pub mod systems;`)

**Interfaces:**
- Produces: `crate::dots::systems::DotsRoot` (marker component); `spawn_dots` (`OnEnter(AppState::Dots)`); `update_dots_sim_params` (`Update`, gated `sketch_active(AppState::Dots)`); a `remove_dots_sim_params` (`OnExit`) that drops `ParticleSimParams` + `CpuMirror`.
- Consumes: `DotsSettings`, the shared `ParticleSimParams`/`SimParams`/`Particle`/`ParticleMaterial`/`CpuMirror`.

- [ ] **Step 1: Grid spawn (`spawn.rs`)**

Mirror `line/systems/spawn.rs`'s structure (buffer alloc via `ShaderBuffer`, the `count*6` flat mesh, the `ParticleMaterial` spawn with the four feature uniforms at their `*_off()` sentinels, `CpuMirror` snapshot insert, `ParticleSimParams` insert, `DotsRoot` marker). REPLACE the layout: build a full-screen grid in centered world space. Read `dot_spacing` from `DotsSettings`. v4 (`index.ts`): for `x` from `-EXTENT*spacing` to `width + EXTENT*spacing` step `spacing`, and `y` likewise over height, with `EXTENT = 10`; each particle's `original_xy` is its grid home, world-centered (subtract `half_w`/`half_h`, flip y to +up). Clamp the resulting `count` to `[100, 200_000]` (a dense grid is larger than Line's line). Use `make_particle`-style construction: `position == original_xy`, `velocity = [0,0]`, `alpha = 0`, and the attract-mode fields (`age=0`, `lifespan`, `spawn_hash`, `spawn_color = white`) seeded the same way Line does (reuse `crate::line::hash`? NO — Line's `hash` module is Line-private; either (a) move the tiny `hash` helper into `crate::particles` if trivially shareable, or (b) seed `lifespan/age/spawn_hash` to inert defaults `0.0` for D2 since Dots has no screensaver yet — D6 adds them). Choose (b) for D2: set `age=0, lifespan=0, spawn_hash=0, spawn_color=white`; note in a comment that D6 seeds the attract fields. The render shader treats `spawn_color = white (0x00FFFFFF)` as no tint.

- [ ] **Step 2: Dots sim-params writer (`sim_params.rs`)**

Mirror `line/systems/sim_params.rs`'s `bake_sim_params` structure (read it). Produce a `update_dots_sim_params` system that each frame writes `ParticleSimParams.params` with Dots values:
- `dt` = follow Line's convention (read what Line passes — match it so integration behaves consistently).
- Drag baked against `const V4_FIXED_DT_DOTS: f32 = 0.048;` — `pulling_drag_baked = V4_DOTS_PULLING_DRAG.powf(0.048)`, `inertial_drag_baked = V4_DOTS_INERTIAL_DRAG.powf(0.048)` with the Dots drag constants (`0.96075095702` / `0.23913643334`) as named `#[allow(clippy::excessive_precision, clippy::unreadable_literal, reason="v4 parity")]` consts.
- `size_scale` = `min(2f32.powf(width/836.0 - 1.0), 1.0)` — the canvas-width multiplier ONLY (NOT gravity). Match Line exactly: the kernel computes `force_mag = a.power * size_scale`, and `gravity_constant` is baked into the *attractor power* host-side (Task 3 Step 2), NOT into `size_scale`. So `size_scale` carries only the v4 `min(2^(w/836-1), 1)` width term. With no attractor active in D2 this is unused, but write it the same way Line does so Task 3 plugs in cleanly.
- `fade_duration = 3.0`, `stationary_constant = 0.01`.
- `constrain_min`/`constrain_max` = effectively infinite (e.g. `[-1e9, -1e9]` / `[1e9, 1e9]`) so the OOB→home reset NEVER fires (v4 `constrainToBox = false`).
- `attract_gate = 0`, `attract_fraction`, `turbulence_*` = off-sentinels (D6 wires the screensaver). `attractor_count = 0` for now (Task 3 adds the mouse attractor at index 0).
- The attractor array stays zeroed in D2 (no mouse yet) — so the grid sits at home, held by the stationary spring, fading in.

- [ ] **Step 3: Lifecycle wiring (`dots/mod.rs`)**

Add `OnEnter(AppState::Dots) -> spawn_dots`, `OnExit(AppState::Dots) -> (despawn_with::<DotsRoot>, remove_dots_sim_params)`, and `Update -> update_dots_sim_params.run_if(sketch_active(AppState::Dots))`. `remove_dots_sim_params` drops `ParticleSimParams` + `CpuMirror` (mirror Line's `remove_sim_params`, minus the Line-only `LineSmearFocal`/post params).

- [ ] **Step 4: Tests**

In `spawn.rs` `#[cfg(test)] mod tests`: assert the grid spawn produces a particle count > 0 for a known window size + spacing, that `original_xy == position` for every particle, and that the grid extent covers `[-EXTENT*spacing, width+EXTENT*spacing]` (e.g. min x ≤ `-EXTENT*spacing - half_w` bound, max x ≥ the right bound). In `sim_params.rs` tests: insert `DotsSettings::default()` + a known window, run `update_dots_sim_params`, assert `params.stationary_constant == 0.01`, `params.fade_duration == 3.0`, `params.attractor_count == 0`, and that `constrain_max` is huge (no OOB reset). Follow Line's lifecycle-test harness pattern if a full app is needed (read `crates/wc-sketches/tests/` for the helper); prefer `RunSystemOnce` for the param writer.

- [ ] **Step 5: Build, test, gates**

```bash
cargo build -p wc-sketches --all-features
cargo nextest run -p wc-sketches --all-features
cargo clippy -p wc-sketches --all-targets --all-features -- -D warnings
```

Expected: PASS. (Interactive `cargo rund` to actually SEE the grid is deferred to the operator — do not run it.)

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -F <(printf '%s\n' "feat(dots): grid spawn + Dots sim params + render (static fading grid)" "" "Full-screen grid on the shared particle engine with v4 Dots constants (gravity 100, stationary spring 0.01, fade 3, constrain off). No attractor yet." "" "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>")
```

---

### Task 3: Mouse/touch attractor + decay + idle veto

**Files:**
- Create: `crates/wc-sketches/src/dots/systems/mouse.rs` (`DotsMouseAttractorState` + `update_dots_mouse_attractor` + `decay_dots_mouse_attractor`)
- Modify: `crates/wc-sketches/src/dots/systems/sim_params.rs` (write the mouse attractor into `params.attractors[0]` when active)
- Modify: `crates/wc-sketches/src/dots/mod.rs` (init the resource, register the systems in the chained `Update` set, register the idle veto)

**Interfaces:**
- Produces: `DotsMouseAttractorState { power: f32, position: [f32; 2] }`; the two systems; a `dots_idle_veto`.
- Consumes: `wc_core::input::pointer::PointerState`, `wc_core::settings::EguiPointerCaptured`, `wc_core::lifecycle::RegisterIdleVetoExt`.

- [ ] **Step 1: Mouse attractor (`mouse.rs`)**

Mirror `line/systems/mouse.rs` (read it) — same `PointerState`/`Touches`/egui-capture gating and world-space conversion — but with Dots constants: `DOTS_MOUSE_POWER_PRESS = 1.0`, `DOTS_MOUSE_POWER_FLOOR = 2.0`, `DOTS_MOUSE_POWER_DECAY = 0.9` (v4 `dots/index.ts`: `createAttractor` sets `power = 1`; `ATTRACTOR_POWER_DECAY_FLOOR = 2`, `ATTRACTOR_POWER_DECAY_SPEED = 0.9`). Press sets `power = 1`; release zeros it; `decay_dots_mouse_attractor` does `power = FLOOR + (power - FLOOR) * DECAY` guarded by `power > 0`. (Note for the reviewer: with press=1 < floor=2 the held power rises asymptotically toward 2 — that is faithful to v4; do not "fix" it.)

- [ ] **Step 2: Feed the attractor into sim params**

In `update_dots_sim_params`, when `DotsMouseAttractorState.power > 0`, set `params.attractors[0] = Attractor { position: state.position, power: state.power * gravity? }`. CHECK Line's convention: Line bakes `power * gravity_constant` into the attractor power host-side (see `line/systems/sim_params.rs` + the `simulate.wgsl` comment "mouse.power * gravity_constant is already baked into attractor.power host-side"). Mirror EXACTLY what Line does (the `size_scale` already carries the gravity factor in Line — read carefully which of `power`/`size_scale` carries `gravity_constant`, and replicate so the force magnitude math matches the shared kernel). Set `params.attractor_count = 1` when active, else `0`.

- [ ] **Step 3: Idle veto + system registration (`mod.rs`)**

`init_resource::<DotsMouseAttractorState>()`. Register `dots_idle_veto` (returns true while `DotsMouseAttractorState.power > 0`) via `register_idle_veto` (mirror `line_idle_veto`). Add `update_dots_mouse_attractor`, `decay_dots_mouse_attractor`, `update_dots_sim_params` to the `Update` chain `.chain().run_if(sketch_active(AppState::Dots))`, ordered so the mouse state is updated before the sim-params writer reads it (mirror Line's chain order).

- [ ] **Step 4: Tests**

In `mouse.rs` tests: press sets `power = 1.0`; `decay` moves `power` from `1.0` toward `2.0` (assert it rises and stays < 2.0); release zeros power. In `sim_params.rs` tests: with `DotsMouseAttractorState { power: 1.0, position: [5.0, 5.0] }`, after `update_dots_sim_params`, `params.attractor_count == 1` and `params.attractors[0].power` equals the baked value (assert against the exact formula you used in Step 2). Add an idle-veto test mirroring Line's (veto true while power > 0).

- [ ] **Step 5: Build, test, gates**

```bash
cargo build -p wc-sketches --all-features
cargo nextest run -p wc-sketches --all-features
cargo clippy -p wc-sketches --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -F <(printf '%s\n' "feat(dots): mouse/touch attractor + decay + idle veto" "" "v4 Dots attractor (press power 1, floor 2, decay 0.9); pulls the grid via attractors[0]. Idle veto keeps Dots active while power decays." "" "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>")
```

---

## Self-Review

**Spec coverage** (against `2026-06-22-dots-fabric-design.md` §"Dots-specific components" + D2 row):
- Grid spawn (dot_spacing + 10-cell bleed) → Task 2 Step 1. ✓
- Dots params (gravity 100, drag pair, stationary 0.01, fade 3, constrain off) → Task 2 Step 2. ✓
- Star-quad render via shared `ParticleMaterial` → Task 2 Step 1 (spawn uses it; umbrella registers the Material2dPlugin in Task 1). ✓
- Mouse + touch attractor → Task 3. ✓
- Settings `dot_spacing`/`gamma` (Dev, requires_restart) → Task 1. ✓
- Manifest "Fabric" → Task 1. ✓
- Shared-plugin hoist (the D1→D2 handoff) → Task 1 Step 3. ✓
- Hand attractors / explode post / audio / screensaver → NOT this plan (D3–D6). ✓ scope holds.

**Placeholder scan:** No TBD/TODO-as-deliverable. The few "read Line's X and mirror it, swapping these values" directives are deliberate (greenfield code mirroring an established in-repo pattern, with the exact v4 values given); each names the file to read and the values to use. The one genuine open detail — exactly which of `power`/`size_scale` carries `gravity_constant` in the attractor baking — is resolved by Task 3 Step 2's instruction to read and replicate Line's convention so the force math matches the shared kernel.

**Type consistency:** `DotsRoot`, `DotsSettings { dot_spacing, gamma }`, `DotsMouseAttractorState { power, position }`, `spawn_dots`/`update_dots_sim_params`/`remove_dots_sim_params`/`update_dots_mouse_attractor`/`decay_dots_mouse_attractor`/`dots_idle_veto` — used consistently across Tasks 1–3. Consumes the D1 shared types by their final names (`ParticleSimParams`, `ParticleMaterial`, `CpuMirror`, `SimParams`, `Attractor`).

**Risks:** The plugin hoist (Task 1) touches Line — gated by Line's existing tests passing. The attractor-baking convention (Task 3 Step 2) must match the shared kernel's expectation — the test in Task 3 Step 4 pins it.
