# WaveConductor v5 — Dots ("Fabric") sketch port + shared particle foundation

**Status:** Design approved 2026-06-22. Staged across implementation plans D1–D7 (see *Plan staging*).

**Parity target:** Perceptual, against v4 `src/sketches/dots/` (screenshots `dots1.png`/`dots3.png`, the `explode` post-process, and `audio.ts`).

**Internal name** stays `Dots` (`AppState::Dots` already exists in `SKETCH_ORDER`). **User-facing manifest name is "Fabric"**, matching v4 HomePage (`renderHighlight("Fabric", dotsImg)`) — exactly as Line ships internally `Line` and displays "Gravity".

## Goal

Port v4's Dots sketch to v5 at perceptual parity, *and* extract the gravity particle engine — which v4 shared between Line and Dots and which v5 had embedded entirely inside Line — into a shared `particles/` foundation both sketches consume. The Dots port additionally carries the v5-era kiosk features Line accreted after its own parity (screensaver attract-mode, bone-wireframe hands), which v4 Dots never had.

## Scope

**In scope (the Dots perceptual-parity target):**

- Full-screen **grid** spawn (`dot_spacing` px + 10-cell bleed margin).
- Gravity simulation with Dots' parameters, including the v4 **stationary spring** (home-pull) that Line does not use.
- Star-quad rendering (shared star material).
- The **explode** chromatic-aberration post-process (the signature "fabric" look).
- Mouse + Leap/MediaPipe **hand attractors**.
- Stats-coupled **audio voice** (triangle pair → lowpass→bandpass cascade + noise + LFO).
- `dot_spacing` + `gamma` settings (Dev, `requires_restart`).
- Manifest tile ("Fabric").
- **v5 kiosk extras:** Dots screensaver/attract-mode and bone-wireframe hand rendering.

**In scope (foundation):**

- Extract `crates/wc-sketches/src/particles/` and refactor Line onto it with behavior preserved.

**Out of scope:**

- Templates / heatmap-image spawn / psychedelic palette / per-image color influence (Line-only features; v4 Dots has none of them — Dots leaves their shared uniforms at their off-sentinels).
- Generalizing spawn→sim→render→audio into a full "ParticleSketch" framework. Deferred until a *third* particle sketch proves the boundaries; the other three v4 sketches (Flame/Cymatics/Waves) share no particle code, so today there are exactly two consumers.

## Background: what v4 shared

In v4, the gravity engine (`@/particles`: `particleSystem`, `attractor`, `particleStats`, `particlePoints`, `leapAttractorPower`) and `@/materials/starMaterial` were imported by **Line and Dots only** — ~360 LOC, parameterized by a `ParticleSystemParameters` struct. The other three sketches share nothing particle-related. v5 embedded all of this inside `line/` (prefixed `Line*`) and grew it ~10× with GPU compute plus six plans of Line-only features. This design restores v4's seam: the engine returns to a shared module, and Line's added features remain Line-side, driving the shared uniforms.

## Architecture

### The shared `particles/` foundation

New module `crates/wc-sketches/src/particles/`, consumed by both Line and Dots:

- **`compute.rs`** — the render-graph compute harness, made generic over the params pod type + shader path (`ParticleComputePlugin<P>`). This is Line's current 288-line `compute.rs` boilerplate (extract resource → `RenderStartup` pipeline init → `PrepareBindGroups` → root-graph dispatch before `camera_driver`), single-sourced. The `LineSimParams` extract-resource becomes a generic carrier of `{ params: P, particles_handle, particle_count }`.
- **`particle.rs`** — `Particle`, `Attractor`, `SimParams`, `MAX_ATTRACTORS`, un-prefixed. Layouts unchanged from Line's current structs except the one new field below.
- **`material.rs`** — `ParticleMaterial` (renamed from `LineMaterial`), all six uniforms retained (particles, star texture, solid override, attract color, template color, palette).
- **`sim_cpu.rs`** — the CPU kernel reference: `Particle`/`SimParams` integrator (`step_one`, `step_cpu_mirror`, `CpuMirror`). Moved here in its **honest role** — a kernel-parity reference (the "two integrators stay ≤1% equivalent" anchor) and a test fixture for spawn-distribution checks without a GPU readback. **Not a production system** for either sketch (Line's Plan 11 Phase F removed its per-frame step on thermal grounds; see *Audio*).
- **Shaders move** to `assets/shaders/particles/simulate.wgsl` + `render.wgsl`.

### The one kernel change: stationary spring

v4's `particleSystem.ts` applies, every step, a home-spring toward `original_xy`:

```
d   = original_xy - position
F  += STATIONARY_CONSTANT * d * |d|          // length-scaled, nonlinear
position, velocity integrate as usual
if no active attractors:  original_xy -= d * 0.05   // slow idle home-drift
```

Line's `simulate.wgsl` does **not** implement this (Line uses `STATIONARY_CONSTANT = 0`). Dots uses `0.01`. The shared kernel gains a `stationary_constant` field on `SimParams` and the spring + idle-drift term, gated so that `stationary_constant == 0.0` is a **provable no-op** — Line's output stays bit-identical. Dots passes `0.01`.

### Design choice: share everything, gate features off per caller

Line's `SimParams`/`Particle`/`ParticleMaterial` already no-op-gate every Line-only feature behind a zero/off value (`attract_gate == 0`, `turbulence_amp == 0`, `attract_fraction`, `template_color_off`, `palette_off`, `solid_off`, lifetime via `lifespan == 0`). Dots consumes the **identical** structs, kernel, and material, and leaves those at their off-sentinels — the same discipline the kernel already lives by.

- **Cost:** Dots' GPU buffers carry a handful of unused fields (`spawn_color`, and `age`/`lifespan`/`spawn_hash` until the screensaver plan). At 48 bytes/particle and ~10–30k dots that is a few hundred KB to ~1.5 MB — negligible.
- **Payoff:** zero kernel/render divergence, single-sourced shaders, and Dots inherits screensaver/attract capability *structurally for free* — D6 only wires the driver and tunes params, because the shared `SimParams` already carries `attract_gate`/`turbulence_*`/`attract_fraction` and the shared `Particle` already carries `age`/`lifespan`/`spawn_hash`.

The earlier worry that sharing `ParticleMaterial` would force a complex optional-uniform material dissolves under this gate pattern: Dots simply spawns with the four feature uniforms at their off-sentinels (and turns `attract_color` on in D6, like Line's screensaver does).

### What stays in `line/`

Line's *feature systems* are unchanged and remain Line-side, now importing the shared types and driving the shared uniforms exactly as today: the screensaver driver, palette driver, template systems (`color_influence`, `reseed`, `prune_adjustments`), gravity-smear post-process (`line/post_process.rs` + `assets/shaders/line/gravity.wgsl`), bone composite, hand mesh, attractor ring visuals, `MouseAttractorState`, idle veto, audio coupling, settings, manifest. The refactor is a mechanical rename (`Line*` → shared names) plus the one no-op kernel term.

### Dots-specific components (`crates/wc-sketches/src/dots/`)

- **`mod.rs`** — `DotsPlugin`: registers settings, manifest ("Fabric"), `ParticleComputePlugin<SimParams>`, the explode post-process, hand attractors, screensaver, bone mesh; wires `OnEnter`/`OnExit` spawn/despawn under a `DotsRoot` marker and the per-frame `sketch_active(AppState::Dots)` system chain.
- **`spawn.rs`** — grid spawn: for `x` in `-10*spacing .. width + 10*spacing` step `spacing`, same in `y`; each particle's `original_xy` is its grid home; `alpha = 0` (fade-in). Allocates the storage buffer + `CpuMirror` spawn snapshot (test fixture parity with Line).
- **Params** — `GRAVITY_CONSTANT = 100`, Dots drag pair (`PULLING ≈ 0.96075`, `INERTIAL ≈ 0.23914`, baked via `pow(c, dt)`), `stationary_constant = 0.01`, `fade_duration = 3`, `dt ≈ 0.048` (v4 `0.016 * 3`). Constrain-box **disabled** by passing effectively-infinite `constrain_min`/`max`, so the OOB→home reset never fires.
- **`post_process.rs` + `assets/shaders/dots/explode.wgsl`** — a post-process render-graph node mirroring `line/post_process.rs`. Ports the explode fragment shader: 5 iterations of radial chromatic-aberration (per-channel `shrink` offsets compounding by `shrink_factor`), the `center -= m2*(center-0.5)*0.5928` spiral, accumulate `col + original`, then `pow(col, gamma)`. Uniform `DotsPostParams { i_mouse, i_resolution, shrink_factor = 0.98, gamma }`. `i_mouse` is the pointer in UV space with the v4 Y-flip.
- **`audio.rs` + `audio_coupling.rs`** — fundsp synth voice mirroring v4 `dots/audio.ts`: detuned triangle pair (base 164.82 Hz) → lowpass(Q≈5.18)→bandpass(Q≈5.18) cascade with an LFO (8.66 Hz) summed onto both cutoffs, plus white noise; `AddDotsSynth`/`RemoveDotsSynth` audio commands and a `SetDotsParam` coupling. See *Audio*.
- **`settings.rs`** — `DotsSettings { dot_spacing: f32 = 20, gamma: f32 = 1.0 }`, both Dev category, `requires_restart`, with `#[serde(default)]` per field (carry-forward #57).
- **Manifest** — registers `AppState::Dots`, display "Fabric", screenshot from `assets/sketches/dots/screenshot.png` (PNG; the workspace has no `jpeg` feature).
- **Hand attractors / screensaver / bone mesh** — reuse the shared hand-attractor infrastructure and `leapAttractorPower` curve, the shared kernel's attract gating, and a per-sketch `HandMeshPlugin` (carry-forward #74: land per-sketch, not upstream-extracted).

## Audio

v4 Dots audio reads four true per-frame reductions over the field via `computeStats`: `flatRatio` (horizontal/vertical spread) → LFO frequency, `varianceLength` + `averageVel` → filter frequency (`120/normVarLen * avgVel/100`), `groupedUpness` → volume (`max(groupedUpness - 0.05, 0)`). The particles live on the GPU.

**Decision — envelope approximation is the primary (production) path.** Line was born reading these exact stats off a per-frame CPU mirror (Plans 7–9), then **deliberately removed** that step in Plan 11 Phase F on thermal grounds, replacing it with attack/release envelopes keyed on attractor state. That is the strongest available prior for the soak target. D4 therefore implements **envelopes first** (Line-aligned, thermally proven), and does **not** resurrect a per-frame CPU mirror in production.

**Honest parity gap:** attractor-state envelopes cannot synthesize `flatRatio` (field shape), so Dots' envelope voice will be a looser perceptual match to its v4 original than a true-stats version — the same trade Line accepted. This is judged by ear during D4 hardware tuning.

**True-stats fallback (attempt only, evidence-gated):** if the envelope voice misses Dots' character by ear, a true-stats path may be attempted — either a GPU reduction pass with a tiny (~32-byte) 1-frame-latent async readback, or registering the shared `step_cpu_mirror` for Dots — but only with soak evidence that it holds thermals. Eyes open: Line already found the CPU-mirror cost not worth it.

## Plan staging

| Plan | Scope | Mirrors Line |
|---|---|---|
| **D1** | Extract shared `particles/` foundation (`compute`/`particle`/`material`/`sim_cpu` + shaders) + `stationary_constant` term; refactor Line onto it; **re-verify Line parity as a hard gate** (clippy/test/`xtask capture` line scenarios/soak unchanged). No Dots yet. | — (new) |
| **D2** | Dots scaffold + manifest + settings + grid spawn + sim + mouse attractor → dots pulled by the pointer. | Plans 6–7 |
| **D3** | Explode post-process. | Plan 8 |
| **D4** | Audio voice + envelope coupling (per *Audio*). | Plan 9 |
| **D5** | Leap/MediaPipe hand attractors. | Plan 11.6 |
| **D6** | v5 kiosk extras: screensaver attract-mode + bone-wireframe hands. | Plans 11.6 / 11.8 |
| **D7** | Parity closure: `dots/PARITY.md`, `xtask capture dots-*` scenario, `dots_soak` harness, manual tuning. | Plans 10–11 |

D1 is the only plan that touches Line; its exit criterion is that Line's existing capture baselines and behavior are unchanged.

## Testing

- **Per plan:** `cargo fmt --check`, `cargo clippy --all-targets --all-features --workspace -D warnings`, `cargo nextest run --workspace --all-features` + `cargo test --doc`, `cargo doc`, `cargo deny check`, `cargo xtask check-secrets`.
- **D1 (foundation):** the refactor is behavior-preserving, so its primary evidence is that **Line's existing tests, `xtask capture` baselines, and soak are unchanged** — no new Line baselines. Add unit tests that `stationary_constant = 0.0` leaves a step bit-identical, and that the shared `ParticleComputePlugin<P>` builds for a second params type.
- **Dots unit/integration:** grid-spawn count/extent and `original_xy = home`; the spawn-distribution check via the shared `CpuMirror` snapshot (Dots analog of `line_heatmap_e2e.rs`); explode-shader uniform plumbing; settings deserialize with `#[serde(default)]`; manifest registers "Fabric".
- **Visual:** a `cargo xtask capture dots-*` scenario (pinned sim timestep) reviewed by the operating agent against v4 `dots1.png`/`dots3.png` — no LLM API spend.
- **Soak:** a `dots_soak` harness (`#[ignore]`d) before any tag, per AGENTS.md's 8-hour requirement.

## Risks

1. **Line refactor regression (D1).** Mitigated by keeping it a mechanical rename + one no-op term, and gating on Line's existing capture/soak baselines being unchanged. The no-op-gate discipline (`stationary_constant = 0`) makes Line's kernel output provably identical.
2. **Generic compute harness ergonomics.** `ParticleComputePlugin<P>` must stay `Pod`-based (Line uses bytemuck, not encase). If the generic bound proves awkward, fall back to a macro or a thin per-sketch wrapper around shared free functions — the boilerplate is what we're de-duplicating, not necessarily via generics.
3. **Audio parity gap.** Envelopes can't reproduce `flatRatio`; accepted as a known trade (matches Line), re-evaluated by ear in D4.
4. **Explode post + Line's smear coexistence.** Each sketch owns its own post-process node gated by `AppState`; the shared foundation deliberately does **not** own a post-process (the two sketches' looks diverge here, exactly as in v4).
5. **GPU memory of shared struct fields in Dots.** Quantified as negligible (≤~1.5 MB); accepted in exchange for zero divergence and free screensaver capability.

## References

- v4 source: `.worktrees/v4/src/sketches/dots/` (`index.ts`, `audio.ts`, `shaders/explode/`), `.worktrees/v4/src/particles/` (`particleSystem.ts`, `particleStats.ts`, `attractor.ts`, `leapAttractorPower.ts`), `.worktrees/v4/src/routes/homePage/HomePage.tsx` (display name "Fabric").
- v5 Line (refactor source + pattern): `crates/wc-sketches/src/line/` (`compute.rs`, `particle.rs`, `material.rs`, `sim_cpu.rs`, `post_process.rs`, `settings.rs`, `mod.rs`), `assets/shaders/line/`.
- Carry-forwards: #57 (`#[serde(default)]`), #74 (HandMesh per-sketch, don't upstream-extract prematurely).
- `crates/wc-sketches/src/line/PARITY.md` (Plan 11 Phase F audio history).
