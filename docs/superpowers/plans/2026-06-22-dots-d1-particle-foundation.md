# Dots D1 — Shared particle foundation + Line refactor — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract Line's gravity particle engine into a shared `crates/wc-sketches/src/particles/` module (restoring v4's `particles/` seam), add a no-op-gated `stationary_constant` kernel term, and refactor Line onto it — with Line's behavior provably unchanged.

**Architecture:** Because the spec's "share everything" decision means Line and Dots use the *identical* `SimParams`/`Particle`/`simulate.wgsl`/`render.wgsl`/material, the compute harness is a **non-generic** shared plugin (one params type, one shader) — not generic over a params type. D1 moves four files (`particle.rs`, `compute.rs`, `material.rs`, `sim_cpu.rs`) + two shaders into the shared module, renames the `Line*` symbols to un-prefixed shared names, and adds the stationary spring gated so `stationary_constant == 0.0` (Line's value) is bit-identical. The shared plugins keep being registered from `LinePlugin` for now; D2 hoists them to `SketchesPlugin` when Dots becomes a second consumer.

**Tech Stack:** Rust, Bevy 0.19, bytemuck (`Pod`/`Zeroable` GPU structs), WGSL compute + Material2d render, `cargo nextest`, `cargo xtask capture` (visual regression).

## Global Constraints

- **Bevy 0.19.** `#[repr(C)]` bytemuck structs MUST stay byte-compatible with their WGSL counterparts; the `const _` size asserts in `particle.rs` enforce 16-byte multiples.
- **Behavior-preserving refactor.** This plan must not change Line's rendered output or simulation. The hard gate is: Line's existing tests, `cargo xtask capture` baselines, and clippy/doc all pass unchanged. The only new behavior (the stationary spring) is gated off for Line (`stationary_constant == 0.0`).
- **No `unwrap()`/`expect()` in non-test code** unless the panic is a documented invariant violation.
- **No `as` casts** on numeric types where `From`/`TryFrom`/`u32::try_from` would work.
- **Docs:** `///` on every public item; `//!` module-root doc on every `mod.rs`/module root.
- **Never allocate in a hot path** (per-frame systems, audio callback, worker loops).
- **Verification gates (run before claiming any task done):** `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features --workspace -- -D warnings`, `cargo nextest run --workspace --all-features` + `cargo test --doc --workspace`, `cargo doc --no-deps --workspace --document-private-items`, `cargo xtask check-secrets`. Dev smoke: `cargo rund`.
- **Commit message backticks** get shell-substituted — use `git commit -F <file>` or avoid backticks in `-m`.

## Symbol & path rename map (applies across Tasks 1–4)

| Old (in `line/`) | New (in `particles/`) |
|---|---|
| `crate::line::particle` / `super::particle` | `crate::particles::particle` / `super::particle` |
| `crate::line::compute` | `crate::particles::compute` |
| `crate::line::material` | `crate::particles::material` |
| `crate::line::sim_cpu` | `crate::particles::sim_cpu` |
| `LineComputePlugin` | `ParticleComputePlugin` |
| `LineSimParams` | `ParticleSimParams` |
| `LinePipeline` | `ParticlePipeline` |
| `LineComputeBindGroup` | `ParticleComputeBindGroup` |
| `LineMaterial` | `ParticleMaterial` |
| `LineCpuMirror` | `CpuMirror` |
| `SimParams` field `_turb_pad: f32` | `stationary_constant: f32` |
| asset `shaders/line/simulate.wgsl` | `shaders/particles/simulate.wgsl` |
| asset `shaders/line/render.wgsl` | `shaders/particles/render.wgsl` |

`Particle`, `Attractor`, `SimParams`, `MAX_ATTRACTORS` keep their names (already un-prefixed); only their module path changes.

---

### Task 1: Create `particles/` module + relocate the GPU structs (`particle.rs`)

Move the data layouts first (they're the leaf dependency of compute/material/sim_cpu). Rename the dead `_turb_pad` pad to a real `stationary_constant` field — this is byte-identical (same 4-byte slot in the same 16-byte row, so the `const _` size asserts still hold) and behavior-neutral (no code reads it yet; the kernel logic lands in Task 5).

**Files:**
- Create: `crates/wc-sketches/src/particles/mod.rs`
- Move: `crates/wc-sketches/src/line/particle.rs` → `crates/wc-sketches/src/particles/particle.rs` (via `git mv`)
- Modify: `crates/wc-sketches/src/lib.rs` (add `pub mod particles;`)
- Modify: `crates/wc-sketches/src/line/mod.rs` (remove `pub mod particle;`)
- Modify (import path `line::particle`/`super::particle` → `particles::particle`): `crates/wc-sketches/src/line/compute.rs`, `crates/wc-sketches/src/line/sim_cpu.rs`, `crates/wc-sketches/src/line/settings.rs`, `crates/wc-sketches/src/line/systems/reseed.rs`, `crates/wc-sketches/src/line/audio_coupling.rs`, `crates/wc-sketches/src/line/screensaver/choreography.rs`, `crates/wc-sketches/src/line/systems/spawn.rs`, `crates/wc-sketches/src/line/systems/sim_params.rs`
- Modify (`_turb_pad` → `stationary_constant`): `crates/wc-sketches/src/line/sim_cpu.rs:202`, `crates/wc-sketches/src/line/systems/sim_params.rs:192`

**Interfaces:**
- Produces: module `crate::particles::particle` exporting `Particle`, `Attractor`, `SimParams`, `MAX_ATTRACTORS`. `SimParams` now carries `pub stationary_constant: f32` (was `_turb_pad`), same `#[repr(C)]` byte layout. `crate::particles` module root exists.

- [ ] **Step 1: Move the file and create the module root**

```bash
cd /Users/madison/Developer/WaveConductor
mkdir -p crates/wc-sketches/src/particles
git mv crates/wc-sketches/src/line/particle.rs crates/wc-sketches/src/particles/particle.rs
```

Create `crates/wc-sketches/src/particles/mod.rs`:

```rust
//! Shared GPU particle engine — the gravity simulation imported by every
//! particle-based sketch (Line and Dots today). Restores v4's `@/particles`
//! seam: the engine lives in one place, parameterized by [`particle::SimParams`]
//! values; each sketch supplies its own spawn layout, post-process, and audio.
//!
//! ## Modules
//! - [`particle`] — `#[repr(C)]` GPU layouts (`Particle`, `Attractor`,
//!   `SimParams`), kept byte-compatible with `assets/shaders/particles/*.wgsl`.
//! - [`compute`] — the render-graph compute harness ([`compute::ParticleComputePlugin`]).
//! - [`material`] — [`material::ParticleMaterial`], the star-quad `Material2d`.
//! - [`sim_cpu`] — a CPU reference integrator + spawn-snapshot fixture
//!   ([`sim_cpu::CpuMirror`]); a kernel-parity anchor, **not** a production system.

pub mod compute;
pub mod material;
pub mod particle;
pub mod sim_cpu;
```

(`compute`/`material`/`sim_cpu` submodules are declared here now but only become valid as Tasks 2–4 move those files. To keep the tree compiling after *this* task, temporarily declare only `pub mod particle;` and add the others in their tasks.)

Replace the `mod.rs` body for Task 1 with just:

```rust
//! Shared GPU particle engine — see the full module doc; submodules are added
//! as Tasks 2–4 relocate them.
pub mod particle;
```

- [ ] **Step 2: Rename the `_turb_pad` field in the moved `particle.rs`**

In `crates/wc-sketches/src/particles/particle.rs`, replace the `_turb_pad` field (currently the last scalar before `attractors`) with:

```rust
    /// v4 `STATIONARY_CONSTANT` — the home-spring strength. Each particle is
    /// pulled toward its `original_xy` with a length-scaled force, and when no
    /// attractor is active its home eases toward it (idle drift). `0.0` is a
    /// provable no-op (Line passes 0.0); Dots passes `0.01`. Occupies the slot
    /// formerly held by `_turb_pad`, so the scalar header stays 64 bytes and the
    /// `attractors` array remains 16-byte aligned — struct size is unchanged.
    pub stationary_constant: f32,
```

Remove the now-obsolete `#[allow(clippy::pub_underscore_fields, ...)]` that decorated `_turb_pad` (the new field is not an underscore field). Update the doc on the preceding turbulence block (it currently says "`_turb_pad` keeps `attractors` 16-byte aligned") to name `stationary_constant` instead.

- [ ] **Step 3: Update the two explicit-construction sites and all import paths**

In `crates/wc-sketches/src/line/sim_cpu.rs:202` and `crates/wc-sketches/src/line/systems/sim_params.rs:192`, change `_turb_pad: 0.0,` → `stationary_constant: 0.0,`.

In `lib.rs`, add `pub mod particles;` (alphabetical: before `pub mod line;` if sorted, else end). In `line/mod.rs`, delete the `pub mod particle;` line.

In each importer listed under **Files**, rewrite `use crate::line::particle::…` / `use super::particle::…` → `use crate::particles::particle::…`. Find them all:

```bash
rg -n 'line::particle|super::particle' crates/wc-sketches/src
```

- [ ] **Step 4: Build + run the struct tests + clippy**

```bash
cargo build -p wc-sketches --all-features
cargo nextest run -p wc-sketches --all-features
cargo clippy -p wc-sketches --all-targets --all-features -- -D warnings
```

Expected: PASS. The `const _` size asserts in `particle.rs` confirm the layout is still 16-byte-aligned (rename was byte-neutral). `sim_cpu` integrator tests pass unchanged (the field is still unread).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor(particles): relocate GPU struct layouts to shared particles/ module

Rename the dead SimParams _turb_pad slot to stationary_constant (byte-neutral;
no reader yet). First step of restoring v4's shared particle engine seam.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Relocate the compute harness (`compute.rs`) + its shader

Move the render-graph compute boilerplate and rename its `Line*` plugin/resources to un-prefixed shared names. Move `simulate.wgsl` in the *same* task so the shader path string never points at a missing file.

**Files:**
- Move: `crates/wc-sketches/src/line/compute.rs` → `crates/wc-sketches/src/particles/compute.rs`
- Move: `assets/shaders/line/simulate.wgsl` → `assets/shaders/particles/simulate.wgsl`
- Modify: `crates/wc-sketches/src/particles/mod.rs` (add `pub mod compute;`)
- Modify: `crates/wc-sketches/src/line/mod.rs` (remove `pub mod compute;`; rename the `compute::LineComputePlugin` registration)
- Modify (rename `LineComputePlugin`/`LineSimParams`/`LinePipeline`/`LineComputeBindGroup`): `crates/wc-sketches/src/line/systems/spawn.rs`, `crates/wc-sketches/src/line/systems/sim_params.rs`, `crates/wc-sketches/src/line/systems/mod.rs`, `crates/wc-sketches/src/line/systems/reseed.rs`, `crates/wc-sketches/src/line/sim_cpu.rs`, `crates/wc-sketches/src/line/screensaver/mod.rs`, `crates/wc-sketches/tests/line_lifecycle.rs`, `crates/wc-sketches/tests/line_input.rs`, `crates/wc-sketches/tests/common/mod.rs`

**Interfaces:**
- Consumes: `crate::particles::particle::SimParams` (Task 1).
- Produces: `crate::particles::compute::{ParticleComputePlugin, ParticleSimParams, ParticlePipeline, ParticleComputeBindGroup}`. `ParticleSimParams` keeps the same fields: `{ params: SimParams, particles_handle: Handle<ShaderBuffer>, particle_count: u32 }`. The compute pipeline loads `shaders/particles/simulate.wgsl`.

- [ ] **Step 1: Move the files**

```bash
git mv crates/wc-sketches/src/line/compute.rs crates/wc-sketches/src/particles/compute.rs
git mv assets/shaders/line/simulate.wgsl assets/shaders/particles/simulate.wgsl
```

Add `pub mod compute;` to `particles/mod.rs`; remove `pub mod compute;` from `line/mod.rs`.

- [ ] **Step 2: Rename symbols, labels, and the shader path inside `particles/compute.rs`**

Apply the rename map: `LineComputePlugin`→`ParticleComputePlugin`, `LineSimParams`→`ParticleSimParams`, `LinePipeline`→`ParticlePipeline`, `LineComputeBindGroup`→`ParticleComputeBindGroup`. Change `use super::particle::SimParams;` stays valid (same dir now). Change the shader load:

```rust
    let shader = asset_server.load::<bevy::shader::Shader>("shaders/particles/simulate.wgsl");
```

Rename the GPU debug labels for clarity (string-only, no behavior): `line_compute_bgl`→`particle_compute_bgl`, `line_compute_pipeline`→`particle_compute_pipeline`, `line_sim_params_uniform`→`particle_sim_params_uniform`, `line_compute_bind_group`→`particle_compute_bind_group`, `line_compute_pass`→`particle_compute_pass`, and the private system `line_compute`→`particle_compute`. Update the module `//!` doc's `[`Line*`]` references.

- [ ] **Step 3: Update every importer + the registration in `line/mod.rs`**

In `line/mod.rs`, change `app.add_plugins(compute::LineComputePlugin);` → `app.add_plugins(crate::particles::compute::ParticleComputePlugin);`, and the `Material2dPlugin` line stays for Task 3. Update the rustdoc in `mod.rs` that references `compute::LineSimParams`.

Rewrite the remaining importers (use the map). Find them:

```bash
rg -n 'LineComputePlugin|LineSimParams|LinePipeline|LineComputeBindGroup|line::compute' crates/wc-sketches
```

In `tests/common/mod.rs`, update the comment at line ~136 that names `LineComputePlugin` → `ParticleComputePlugin` (the harness still deliberately does not add it).

- [ ] **Step 4: Build + tests + clippy + a render smoke via `cargo rund`**

```bash
cargo build -p wc-sketches --all-features
cargo nextest run -p wc-sketches --all-features
cargo clippy -p wc-sketches --all-targets --all-features -- -D warnings
```

Expected: PASS. Then a manual render smoke (the shader actually loads from its new path):

```bash
cargo rund
```

Expected: window opens, enter Line, particles simulate and respond to the pointer exactly as before. (Prompt Madison to eyeball this if running headless.)

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor(particles): move compute harness to shared module, drop Line prefix

LineComputePlugin/LineSimParams/LinePipeline/LineComputeBindGroup -> Particle*;
simulate.wgsl -> shaders/particles/. Non-generic shared plugin (one params type,
one shader). Still registered from LinePlugin; D2 hoists to SketchesPlugin.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Relocate the star material (`material.rs`) + its shader

**Files:**
- Move: `crates/wc-sketches/src/line/material.rs` → `crates/wc-sketches/src/particles/material.rs`
- Move: `assets/shaders/line/render.wgsl` → `assets/shaders/particles/render.wgsl`
- Modify: `crates/wc-sketches/src/particles/mod.rs` (add `pub mod material;`)
- Modify: `crates/wc-sketches/src/line/mod.rs` (remove `pub mod material;`; rename the `Material2dPlugin::<…>` type)
- Modify (rename `LineMaterial`→`ParticleMaterial`): `crates/wc-sketches/src/line/systems/spawn.rs`, `crates/wc-sketches/src/line/systems/color_influence.rs`, `crates/wc-sketches/src/line/systems/palette.rs`, `crates/wc-sketches/src/line/systems/reseed.rs`, `crates/wc-sketches/src/line/screensaver/mod.rs`

**Interfaces:**
- Produces: `crate::particles::material::ParticleMaterial` — the `Asset + AsBindGroup` star-quad `Material2d` with the same seven `@group(2)` bindings (particles, star_texture, sampler, solid_color, attract_color, template_color, palette_params) and the same `solid_off()`/`attract_color_off()`/`template_color_off()`/`palette_off()` associated fns. Loads `shaders/particles/render.wgsl`.

- [ ] **Step 1: Move the files**

```bash
git mv crates/wc-sketches/src/line/material.rs crates/wc-sketches/src/particles/material.rs
git mv assets/shaders/line/render.wgsl assets/shaders/particles/render.wgsl
```

Add `pub mod material;` to `particles/mod.rs`; remove `pub mod material;` from `line/mod.rs`.

- [ ] **Step 2: Rename `LineMaterial`→`ParticleMaterial` and the shader paths inside `particles/material.rs`**

In the moved file, rename the struct + impls + the four associated `*_off()` helpers' `Self` references stay valid. Change both shader refs:

```rust
    fn vertex_shader() -> ShaderRef {
        "shaders/particles/render.wgsl".into()
    }
    fn fragment_shader() -> ShaderRef {
        "shaders/particles/render.wgsl".into()
    }
```

- [ ] **Step 3: Update importers + the `Material2dPlugin` registration**

In `line/mod.rs`: `app.add_plugins(Material2dPlugin::<material::LineMaterial>::default());` → `app.add_plugins(Material2dPlugin::<crate::particles::material::ParticleMaterial>::default());`.

Rewrite all `LineMaterial` references (the map). Find them:

```bash
rg -n 'LineMaterial|line::material' crates/wc-sketches
```

In `spawn.rs`, the `materials: ResMut<'_, Assets<LineMaterial>>` param and the `materials.add(LineMaterial { … })` literal + the `LineMaterial::*_off()` calls all become `ParticleMaterial`.

- [ ] **Step 4: Build + tests + clippy + render smoke**

```bash
cargo build -p wc-sketches --all-features
cargo nextest run -p wc-sketches --all-features
cargo clippy -p wc-sketches --all-targets --all-features -- -D warnings
cargo rund   # enter Line: stars render identically (texture + blend unchanged)
```

Expected: PASS; Line's particles render as soft stars exactly as before.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor(particles): move star material to shared module (LineMaterial -> ParticleMaterial)

render.wgsl -> shaders/particles/. All seven group(2) bindings unchanged.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: Relocate the CPU reference integrator (`sim_cpu.rs`)

Move the CPU kernel reference + spawn-snapshot fixture in its honest role (parity anchor + test fixture, not a production system). Rename `LineCpuMirror`→`CpuMirror`. No kernel-logic change here (the stationary spring lands in Task 5).

**Files:**
- Move: `crates/wc-sketches/src/line/sim_cpu.rs` → `crates/wc-sketches/src/particles/sim_cpu.rs`
- Modify: `crates/wc-sketches/src/particles/mod.rs` (add `pub mod sim_cpu;`)
- Modify: `crates/wc-sketches/src/line/mod.rs` (remove `pub mod sim_cpu;`; rename `sim_cpu::LineCpuMirror`)
- Modify (rename `LineCpuMirror`→`CpuMirror`): `crates/wc-sketches/src/line/systems/spawn.rs`, `crates/wc-sketches/tests/line_heatmap_e2e.rs`

**Interfaces:**
- Consumes: `crate::particles::particle::{Particle, SimParams, MAX_ATTRACTORS, Attractor}`, `crate::particles::compute::ParticleSimParams`.
- Produces: `crate::particles::sim_cpu::{CpuMirror, step_cpu_mirror, step_one}`. `CpuMirror { pub particles: Vec<Particle> }`. `step_one(p: &mut Particle, params: &SimParams)` — the pure per-particle integrator.

- [ ] **Step 1: Move the file**

```bash
git mv crates/wc-sketches/src/line/sim_cpu.rs crates/wc-sketches/src/particles/sim_cpu.rs
```

Add `pub mod sim_cpu;` to `particles/mod.rs`; remove from `line/mod.rs`.

- [ ] **Step 2: Rename `LineCpuMirror`→`CpuMirror` + fix internal imports**

In the moved file, rename the struct, and ensure its `use` lines point at siblings: `use super::particle::{Particle, SimParams, MAX_ATTRACTORS};` and `use super::compute::ParticleSimParams;` (in `step_cpu_mirror`). Update the module `//!` doc: it now describes a *shared* reference integrator used by particle sketches' tests; keep the "not registered in any production schedule" note.

- [ ] **Step 3: Update importers**

In `line/systems/spawn.rs`: `use crate::line::sim_cpu::LineCpuMirror;` → `use crate::particles::sim_cpu::CpuMirror;`, and `commands.insert_resource(LineCpuMirror { … })` → `CpuMirror`. In `line/mod.rs`: `commands.remove_resource::<sim_cpu::LineCpuMirror>();` → `crate::particles::sim_cpu::CpuMirror`. In `tests/line_heatmap_e2e.rs`: `use wc_sketches::line::sim_cpu::LineCpuMirror;` → `use wc_sketches::particles::sim_cpu::CpuMirror;` and the two `resource::<LineCpuMirror>()` reads. Find all:

```bash
rg -n 'LineCpuMirror|line::sim_cpu' crates/wc-sketches
```

- [ ] **Step 4: Build + tests + clippy**

```bash
cargo build -p wc-sketches --all-features
cargo nextest run -p wc-sketches --all-features
cargo clippy -p wc-sketches --all-targets --all-features -- -D warnings
```

Expected: PASS, including `line_heatmap_e2e` (reads the renamed `CpuMirror` spawn snapshot) and the `sim_cpu` integrator tests.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor(particles): move CPU reference integrator to shared module (LineCpuMirror -> CpuMirror)

Kernel-parity anchor + spawn-snapshot test fixture; not a production system.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: Add the no-op-gated `stationary_constant` kernel term (CPU + WGSL)

The one real kernel change. Add v4's home-spring + idle home-drift to both the CPU reference (`step_one`) and the WGSL kernel (`simulate.wgsl`), gated on `stationary_constant > 0.0` so Line (which passes `0.0`) is bit-identical. TDD on the CPU integrator; the WGSL is kept in lockstep (verified by the parity discipline + the Task 6 capture gate).

**Files:**
- Modify: `crates/wc-sketches/src/particles/sim_cpu.rs` (the `step_one` body + new tests)
- Modify: `assets/shaders/particles/simulate.wgsl` (the WGSL `SimParams` field rename + kernel term)

**Interfaces:**
- Consumes: `SimParams::stationary_constant` (Task 1).
- Produces: no new symbols; `step_one`/the kernel now apply the spring when `stationary_constant > 0.0`.

- [ ] **Step 1: Write the failing tests in `particles/sim_cpu.rs`**

Add to the `#[cfg(test)] mod tests`:

```rust
#[test]
fn stationary_spring_pulls_displaced_particle_toward_home() {
    // Home at origin, particle displaced to +x, no attractors, spring on.
    let mut params = zero_attractor_params();
    params.stationary_constant = 0.1;
    let mut p = Particle {
        position: [10.0, 0.0],
        velocity: [0.0, 0.0],
        original_xy: [0.0, 0.0],
        alpha: 1.0,
        age: 0.0,
        lifespan: 0.0,
        spawn_hash: 0.0,
        spawn_color: f32::from_bits(0x00FF_FFFF),
        _pad: 0.0,
    };
    step_one(&mut p, &params);
    // Spring accel = stationary_constant * (home - pos) * |home - pos|
    //              = 0.1 * (-10) * 10 = -10 in x; velocity gains it (then drag).
    assert!(p.velocity[0] < 0.0, "spring must pull toward home; got {}", p.velocity[0]);
}

#[test]
fn stationary_constant_zero_is_a_noop() {
    // Line's value: with no attractors, no spring, zero initial velocity, the
    // particle must not move (drag on zero velocity stays zero).
    let params = zero_attractor_params(); // stationary_constant defaults to 0.0
    let mut p = Particle {
        position: [10.0, 0.0],
        velocity: [0.0, 0.0],
        original_xy: [0.0, 0.0],
        alpha: 1.0,
        age: 0.0,
        lifespan: 0.0,
        spawn_hash: 0.0,
        spawn_color: f32::from_bits(0x00FF_FFFF),
        _pad: 0.0,
    };
    step_one(&mut p, &params);
    assert_eq!(p.velocity, [0.0, 0.0], "stationary_constant==0 must add no force");
    assert_eq!(p.position, [10.0, 0.0], "particle must not move");
    assert_eq!(p.original_xy, [0.0, 0.0], "home must not drift when spring is off");
}
```

(`zero_attractor_params()` already sets every field; confirm it now sets `stationary_constant: 0.0` — Task 1 renamed `_turb_pad: 0.0` there.)

- [ ] **Step 2: Run the tests — confirm `stationary_spring_…` fails**

```bash
cargo nextest run -p wc-sketches stationary -E 'test(stationary)'
```

Expected: `stationary_constant_zero_is_a_noop` PASSES (no code reads the field yet), `stationary_spring_pulls_displaced_particle_toward_home` FAILS (`velocity[0] == 0.0`, spring not applied).

- [ ] **Step 3: Implement the spring in `step_one`**

In `particles/sim_cpu.rs`, immediately after the attractor force loop (after the `for a in &params.attractors[..active_count]` block closes, before the turbulence block):

```rust
    // v4 stationary spring: pull each particle toward its spawn home with a
    // length-scaled (nonlinear) force, and ease home toward the particle when no
    // attractor is active (idle drift). Gated on stationary_constant > 0.0 so
    // Line (which passes 0.0) is provably unchanged. Dots passes 0.01.
    if params.stationary_constant > 0.0 {
        let hx = p.original_xy[0] - p.position[0];
        let hy = p.original_xy[1] - p.position[1];
        let home_len = (hx * hx + hy * hy).sqrt();
        accel[0] += params.stationary_constant * hx * home_len;
        accel[1] += params.stationary_constant * hy * home_len;
        if params.attractor_count == 0 {
            p.original_xy[0] -= hx * 0.05;
            p.original_xy[1] -= hy * 0.05;
        }
    }
```

- [ ] **Step 4: Mirror the change in `assets/shaders/particles/simulate.wgsl`**

First rename the WGSL `SimParams` field to match Rust: change `_turb_pad: f32,` → `stationary_constant: f32,` and update the comment above the turbulence block. Then, immediately after the attractor force loop (after `p.velocity = p.velocity + accel * params.dt;`? — no: BEFORE that line, while `accel` is still being accumulated), insert the spring into the `accel` accumulation. Place it right after the `for` loop closes and before `p.velocity = p.velocity + accel * params.dt;`:

```wgsl
    // v4 stationary spring (gated; stationary_constant == 0 -> no-op = Line parity).
    if (params.stationary_constant > 0.0) {
        let home = p.original_xy - p.position;
        let home_len = length(home);
        accel = accel + params.stationary_constant * home * home_len;
        if (params.attractor_count == 0u) {
            p.original_xy = p.original_xy - home * 0.05;
        }
    }
```

Add a one-line note in the file header that `step_one` in `particles/sim_cpu.rs` is the CPU mirror of this term and the two must change together.

- [ ] **Step 5: Run the tests — both pass**

```bash
cargo nextest run -p wc-sketches --all-features
cargo clippy -p wc-sketches --all-targets --all-features -- -D warnings
```

Expected: both new tests PASS; all existing `sim_cpu` tests (which use `stationary_constant == 0`) PASS unchanged.

- [ ] **Step 6: Render smoke — Line is visually unchanged (spring off)**

```bash
cargo rund   # enter Line, drag: identical behavior (Line passes stationary_constant 0.0)
```

Expected: no visible change to Line.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(particles): add no-op-gated stationary-spring kernel term

v4 home-spring + idle home-drift in step_one (CPU) and simulate.wgsl (GPU),
gated on stationary_constant > 0. Line passes 0.0 -> bit-identical; Dots will
pass 0.01. CPU + WGSL kept in lockstep.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 6: Foundation verification gate + docs

Prove the refactor is behavior-preserving against Line's existing baselines, add a smoke test for the shared plugin, and record the engine move in PARITY.md.

**Files:**
- Create/Modify test: `crates/wc-sketches/tests/particles_foundation.rs` (new smoke test)
- Modify: `crates/wc-sketches/src/line/PARITY.md` (note the engine moved to `particles/`)
- Modify: `crates/wc-sketches/src/particles/mod.rs` (optional re-exports for ergonomics)

**Interfaces:**
- Consumes: the full `crate::particles` public API from Tasks 1–5.

- [ ] **Step 1: Add a build-smoke test for the shared compute plugin**

Create `crates/wc-sketches/tests/particles_foundation.rs`:

```rust
//! Smoke coverage for the shared particle foundation: the compute plugin builds
//! into an app, and the relocated public API is reachable at its new path.

use bevy::prelude::*;
use wc_sketches::particles::compute::ParticleComputePlugin;
use wc_sketches::particles::particle::{SimParams, MAX_ATTRACTORS};

#[test]
fn sim_params_layout_is_16_byte_aligned() {
    // Mirrors the in-module const asserts; guards the stationary_constant rename
    // from drifting the GPU layout.
    assert_eq!(std::mem::size_of::<SimParams>() % 16, 0);
    assert!(MAX_ATTRACTORS >= 1);
}

#[test]
fn particle_compute_plugin_builds() {
    // ParticleComputePlugin no-ops cleanly without a RenderApp (it early-returns
    // when get_sub_app_mut(RenderApp) is None under MinimalPlugins), so adding it
    // must not panic.
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_plugins(ParticleComputePlugin);
    app.update();
}
```

- [ ] **Step 2: Run the new test**

```bash
cargo nextest run -p wc-sketches --test particles_foundation
```

Expected: PASS.

- [ ] **Step 3: Run the full verification suite**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
cargo doc --no-deps --workspace --document-private-items
cargo xtask check-secrets
```

Expected: all PASS (the ~29 pre-existing doc-link warnings and the rustfmt nightly-feature notices are the known-harmless exceptions from AGENTS.md).

- [ ] **Step 4: Re-run Line's visual capture baselines — must be unchanged**

```bash
cargo xtask capture --list            # confirm the Line scenario name(s)
cargo xtask capture <line-scenario>   # for each Line scenario
```

Expected: capture succeeds and the diff against the committed baseline is within tolerance (no regression). The operating agent reviews the PNGs against the prior baseline (no LLM API spend). If any Line scenario drifts, the refactor changed behavior — stop and diagnose before proceeding. (The 8-hour `line_soak` is Madison's pre-tag step, not part of this plan's gate.)

- [ ] **Step 5: Record the move in `PARITY.md` and tidy `particles/mod.rs`**

Append to `crates/wc-sketches/src/line/PARITY.md` a short note: the gravity engine (`compute`/`particle`/`material`/`sim_cpu` + `simulate`/`render.wgsl`) moved to the shared `crate::particles` module in Dots Plan D1, restoring v4's `@/particles` seam; Line's behavior is unchanged (`stationary_constant = 0.0`).

Optionally add ergonomic re-exports to `particles/mod.rs`:

```rust
pub use material::ParticleMaterial;
pub use particle::{Attractor, Particle, SimParams, MAX_ATTRACTORS};
```

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "test(particles): foundation smoke test + PARITY note; verify Line unchanged

Shared particle engine extraction (D1) complete: Line refactored onto
crate::particles with capture baselines and full test suite green.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage** (against `2026-06-22-dots-fabric-design.md` §Architecture / §"shared particles/ foundation" / D1 row):
- Shared `particles/` module with `compute`/`particle`/`material`/`sim_cpu` + moved shaders → Tasks 1–4. ✓
- Non-generic shared compute plugin (collapses the spec's `ParticleComputePlugin<P>` per risk #2) → Task 2, noted in Architecture. ✓
- `stationary_constant` no-op-gated kernel term, Line passes 0.0 → Task 1 (field) + Task 5 (logic). ✓
- Line refactored onto it, behavior preserved, hard gate = capture/tests unchanged → Task 6. ✓
- `sim_cpu` joins foundation in its honest reference/fixture role → Task 4, doc preserved. ✓
- Registration stays in `LinePlugin` for D1; D2 hoists to `SketchesPlugin` → stated in Architecture + Task 2 commit. ✓
- D1 is the only plan touching Line; Dots not built here → scope holds. ✓

**Placeholder scan:** No TBD/TODO/"handle edge cases"/"similar to". The one real kernel change shows full CPU + WGSL code; renames give exact symbol maps + file lists + `rg` find commands. ✓

**Type consistency:** `ParticleSimParams { params, particles_handle, particle_count }`, `CpuMirror { particles }`, `ParticleMaterial` `*_off()` helpers, `step_one(&mut Particle, &SimParams)`, `stationary_constant: f32` — names used consistently across Tasks 1–6 and the rename map. The spring math (`accel += stationary_constant * home * |home|`; idle drift `home -= … * 0.05` when `attractor_count == 0`) is identical in the CPU (Step 3) and WGSL (Step 4) of Task 5. ✓

**Risks carried from spec:** generic-harness ergonomics (risk #2) resolved by going non-generic; GPU-field memory (risk #5) untouched here (no struct trimming). Line-regression (risk #1) is the Task 6 gate.
