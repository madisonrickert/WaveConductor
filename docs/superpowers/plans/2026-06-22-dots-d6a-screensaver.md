# Dots D6a — Screensaver attract-mode — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use `- [ ]` checkboxes.

**Goal:** Give Dots an idle attract-mode (a v5 kiosk addition — v4 Dots had none): when the sketch is idle, the grid slowly morphs under a divergence-free turbulence drift, with the attract-mode fraction-kill + lifetime-respawn keeping the field calm and self-healing. This reuses the shared particle kernel's attract machinery (which Dots' `SimParams`/`Particle` structurally inherited from D1).

**Architecture:** The shared `simulate.wgsl` kernel already implements the attract path (gated by `attract_gate`: fraction kill via `spawn_hash`, lifetime respawn via `age`/`lifespan`, curl-noise turbulence). D2 left Dots' attract particle fields at `0` (inert). D6a (1) seeds those fields at spawn (deterministic per-index hash, copied from Line's `hash`), (2) extracts a `bake_dots_sim_params` helper so the live writer and a new attract driver share param assembly, and (3) adds a `DotsScreensaverPlugin` whose `drive_dots_attract` system (gated `in_screensaver(AppState::Dots)`) bakes the params with the attract gate on + a turbulence drift. Dots' screensaver is simpler than Line's — no gravity-smear coupling and no wandering-pulse choreography; just the turbulence morph the shared kernel provides.

**Tech Stack:** Rust, Bevy 0.19, the shared `crate::particles` kernel's attract path, `wc_core::lifecycle::screensaver::in_screensaver`.

## Global Constraints

- **Reuse the shared kernel's attract machinery** — do NOT add new attract logic to `simulate.wgsl`; Dots drives the existing `attract_gate`/`attract_fraction`/`turbulence_*` params. Read `assets/shaders/particles/simulate.wgsl` (the attract path) + `crate::particles::sim_cpu` (the CPU mirror) to confirm the contract.
- **Seed the attract particle fields at spawn** so the kernel's fraction kill + lifetime respawn work: `spawn_hash` = a deterministic per-index unit hash; `lifespan` = a per-index value in ~10–18 s (staggered, so respawns don't arrive in waves); `age = 0`. Mirror Line's `spawn_hash01` / `attract_lifespan` (in `line/systems/spawn.rs`) — copy the tiny `hash` helper (`wang_hash`/`hash_to_unit`) into Dots (carry-forward: Line's `hash` could move to shared `particles/` — note it). Faithful to Line's seeding so the shared kernel behaves identically.
- **Settings:** add the attract knobs Dots' driver needs to `DotsSettings` (Dev category), mirroring Line's `attract_particle_fraction` + `attract_turbulence` (with `#[serde(default = "...")]` per field). Pick the same defaults Line uses unless there's a Dots reason to differ.
- **No `unwrap()`/`expect()`** in non-test code unless documented; **no `as`** where `TryFrom` works; **`///`/`//!` docs**; the attract driver runs zero work outside the screensaver (gated by `in_screensaver`), satisfying the AGENTS "zero systems when idle/Active" rule (it runs only in Screensaver).
- **Idle/sleep gating note (D2 carry-forward #4):** Dots' idle veto keeps it Active while the pointer is held (v5 behaviour, diverges from v4 where the mouse never blocked sleep). Keep the v5 behaviour (better kiosk UX); document it. The screensaver entry itself is owned by the existing `SketchActivity` idle machinery in `wc-core` — D6a only supplies the attract DRIVER, not the idle/sleep transition logic.
- **Visual feel is operator-deferred** (the morph speed, fraction, look) — `cargo rund` + waiting for idle. Unit-test the param assembly + the seeding determinism; flag the look as operator-tuned.
- **Verification gates:** fmt; clippy `--all-targets --all-features --workspace -D warnings`; nextest `--workspace --all-features` + `cargo test --doc`; `cargo doc`; `cargo xtask check-secrets`. Do NOT run `cargo rund`.
- **Commit messages:** `git commit -F` (no backticks).

## Reference material (read these)

- `crates/wc-sketches/src/line/screensaver/mod.rs` (`LineScreensaverPlugin`, `drive_line_attract` — the attract driver gated `in_screensaver(AppState::Line)`, the `AttractGate`/`Turbulence` baker inputs) — mirror, minus the smear/choreography.
- `crates/wc-sketches/src/line/systems/spawn.rs` (`spawn_hash01`, `attract_lifespan`, `ATTRACT_LIFESPAN_MIN/MAX_SECS`, `make_particle`) + `crates/wc-sketches/src/line/hash.rs` (the `wang_hash`/`hash_to_unit` helper to copy).
- `crates/wc-sketches/src/line/systems/sim_params.rs` (`bake_sim_params`, `AttractGate`, `Turbulence`, `WindowGeom` — the shared-baker shape Line uses; Dots will extract its own analog).
- `crates/wc-sketches/src/dots/systems/sim_params.rs` (`update_dots_sim_params` — refactor to a baker) + `dots/systems/spawn.rs` (the grid spawn — seed the attract fields) + `dots/settings.rs` + `dots/mod.rs`.
- `crates/wc-core/src/lifecycle/screensaver.rs` (`in_screensaver`).

---

### Task 1: Seed attract fields at spawn + extract `bake_dots_sim_params`

**Files:**
- Create: `crates/wc-sketches/src/dots/hash.rs` (copied `wang_hash`/`hash_to_unit` + `spawn_hash01`/`dots_attract_lifespan`)
- Modify: `crates/wc-sketches/src/dots/systems/spawn.rs` (seed `age`/`lifespan`/`spawn_hash` per particle)
- Modify: `crates/wc-sketches/src/dots/systems/sim_params.rs` (extract `bake_dots_sim_params` from `update_dots_sim_params`)
- Modify: `crates/wc-sketches/src/dots/mod.rs` (`pub mod hash;` if needed)

**Interfaces:**
- Produces: `dots::hash::{spawn_hash01, dots_attract_lifespan}`; a `pub(crate) fn bake_dots_sim_params(dt, geom, attractors, count, gate, turbulence) -> SimParams` (mirror Line's `bake_sim_params` signature) that `update_dots_sim_params` now calls (live path: gate off, turbulence off).

- [ ] **Step 1: Copy the hash helper + seed the grid**

Copy `line/hash.rs`'s `wang_hash`/`hash_to_unit` into `dots/hash.rs` (add a carry-forward comment: shared candidate). Add `spawn_hash01(i)` and `dots_attract_lifespan(i)` (uniform in `DOTS_ATTRACT_LIFESPAN_MIN_SECS=10.0 .. MAX=18.0`, salted like Line so the lifespan stream decorrelates from the spawn-hash stream). In `dots/systems/spawn.rs`'s grid loop, replace the inert `age=0, lifespan=0, spawn_hash=0` with `age=0.0, lifespan=dots_attract_lifespan(i), spawn_hash=spawn_hash01(i)` (keep `spawn_color=white`). The live (non-attract) path is unaffected because the kernel only reads these when `attract_gate != 0`.

- [ ] **Step 2: Extract `bake_dots_sim_params`**

Refactor `update_dots_sim_params` so the param assembly (drag bake, size_scale, fade, stationary, constrain, the attractor array, AND the attract gate + turbulence fields) lives in a pure-ish `bake_dots_sim_params(dt, geom, attractors: [Attractor; MAX_ATTRACTORS], count: u32, gate: DotsAttractGate, turbulence: DotsTurbulence) -> SimParams`. The live `update_dots_sim_params` calls it with `gate { enabled: false }` and `turbulence { amp: 0.0 }` (so the live path is bit-unchanged — verify with the existing D2 tests). Define small `DotsAttractGate { enabled: bool, fraction: f32 }` + `DotsTurbulence { amp, scale, time }` inputs (mirror Line's `AttractGate`/`Turbulence`). Keep the fixed stack attractor buffer (no allocation).

- [ ] **Step 3: Tests**

`spawn_hash01`/`dots_attract_lifespan` determinism + range (mirror Line's `attract_lifespan_is_deterministic_and_in_range` + `spawn_hash_is_uniform_enough`). A test that `update_dots_sim_params` still produces the D2 live values (`attract_gate == 0`, `turbulence_amp == 0`, stationary 0.01, etc.) AFTER the refactor — i.e. the extraction is behavior-preserving for the live path. A `bake_dots_sim_params` unit test with `gate.enabled = true` + `turbulence.amp = 1.0` asserts `attract_gate == 1`, `attract_fraction == gate.fraction`, `turbulence_amp == 1.0`.

- [ ] **Step 4: Build, test, gates**

```bash
cargo build -p wc-sketches --all-features
cargo nextest run --workspace --all-features
cargo clippy --all-targets --all-features --workspace -- -D warnings
```

Expected: PASS, including ALL existing D2 Dots sim-param tests (the bake extraction must be behavior-preserving for the live path).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -F <(printf '%s\n' "feat(dots): seed attract particle fields + extract bake_dots_sim_params" "" "Per-index spawn_hash + 10-18s staggered lifespan (inert on the live path; the shared kernel reads them only when attract_gate != 0). bake_dots_sim_params shared by the live writer and the coming attract driver." "" "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>")
```

---

### Task 2: `DotsScreensaverPlugin` + `drive_dots_attract` + attract settings

**Files:**
- Create: `crates/wc-sketches/src/dots/screensaver.rs` (`DotsScreensaverPlugin`, `drive_dots_attract`)
- Modify: `crates/wc-sketches/src/dots/settings.rs` (`attract_particle_fraction`, `attract_turbulence`)
- Modify: `crates/wc-sketches/src/dots/mod.rs` (`pub mod screensaver;`; add `DotsScreensaverPlugin`)

**Interfaces:**
- Produces: `DotsScreensaverPlugin`; `drive_dots_attract` (gated `in_screensaver(AppState::Dots)`).
- Consumes: `DotsSettings` (the new attract knobs), `Time`, `Single<&Window>`, `ParticleSimParams`, `bake_dots_sim_params`.

- [ ] **Step 1: Attract settings**

Add to `DotsSettings`: `attract_particle_fraction: f32` (Dev, the survivor fraction for the fraction kill — default mirror Line's) and `attract_turbulence: f32` (Dev, the drift speed — default mirror Line's), each with `#[serde(default = "default_<name>")]`.

- [ ] **Step 2: `drive_dots_attract`**

Mirror `drive_line_attract`'s structure, MINUS the smear/post and the choreography walkers: each frame (gated `in_screensaver(AppState::Dots)`), build the geom from the window, set `gate = DotsAttractGate { enabled: true, fraction: settings.attract_particle_fraction }`, `turbulence = DotsTurbulence { amp: settings.attract_turbulence, scale: <a constant like Line's TURBULENCE_SCALE>, time: time.elapsed_secs() }`, with an empty/zero attractor array (Dots' screensaver has no wandering pulses — just the turbulence morph; `count = 0`), and write `sim.params = bake_dots_sim_params(time.delta_secs(), geom, attractors, 0, gate, turbulence)`. This turns on the kernel's fraction kill + lifetime respawn + turbulence drift for the idle grid.

- [ ] **Step 3: `DotsScreensaverPlugin` wiring**

Register `drive_dots_attract.run_if(in_screensaver(AppState::Dots))` (mirror `LineScreensaverPlugin`). Add `DotsScreensaverPlugin` in `dots/mod.rs`. (No attract-color material driver in D6a — keep it minimal; a Dots attract-color could be a later follow-up, noted.)

- [ ] **Step 4: Tests**

`drive_dots_attract` via `RunSystemOnce` (insert `ParticleSimParams`, `DotsSettings`, a `Window`, `Time`): after running, `sim.params.attract_gate == 1`, `attract_fraction == settings.attract_particle_fraction`, `turbulence_amp == settings.attract_turbulence`, `attractor_count == 0`. A `DotsSettings` serde test for the two new fields (forward-compat defaults). Mirror Line's screensaver-driver test if one exists.

- [ ] **Step 5: Build, test, gates**

```bash
cargo build -p wc-sketches --all-features
cargo nextest run --workspace --all-features
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo fmt --all -- --check
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -F <(printf '%s\n' "feat(dots): screensaver attract-mode (turbulence drift + fraction kill)" "" "DotsScreensaverPlugin drives the shared kernel's attract path while idle: the grid slowly morphs under curl-noise turbulence with the fraction-kill + lifetime-respawn self-heal. v5 kiosk addition (v4 Dots had none). Feel operator-tuned." "" "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>")
```

---

## Self-Review

**Spec coverage** (against §"v5 kiosk extras" screensaver + D6 row):
- Dots screensaver attract-mode reusing the shared kernel's attract gating → Tasks 1+2. ✓
- Attract particle fields seeded at spawn → Task 1 Step 1. ✓
- The live path stays bit-unchanged (attract fields inert when gate off) → Task 1 Step 2 + the behavior-preserving test. ✓
- Bone-wireframe hands → SEPARATE plan (D6b). ✓ scope holds.

**Placeholder scan:** No TBD-as-deliverable. The seeding + baker mirror named Line code; the driver mirrors `drive_line_attract` minus the Line-only smear/choreography. The screensaver's VISUAL feel is operator-deferred, with the param assembly + seeding determinism unit-tested.

**Type consistency:** `bake_dots_sim_params`, `DotsAttractGate { enabled, fraction }`, `DotsTurbulence { amp, scale, time }`, `spawn_hash01`/`dots_attract_lifespan`, `DotsScreensaverPlugin`/`drive_dots_attract`, `attract_particle_fraction`/`attract_turbulence` — consistent across tasks; feeds the shared `SimParams` by its D1 fields.

**Risks:** (1) The bake extraction must be behavior-preserving for the live path — pinned by re-running the D2 live-value tests. (2) The attract seeding must match Line's so the shared kernel behaves identically — pinned by the determinism/range tests. (3) The screensaver feel is operator-verified. (4) The copied `hash` is a flagged carry-forward.
