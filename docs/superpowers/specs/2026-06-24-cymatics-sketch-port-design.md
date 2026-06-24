# WaveConductor v5 — Cymatics sketch port

**Status:** Design approved 2026-06-24. Sub-project #2 of the Cymatics work; depends
on the shared `hand_mesh` module (sub-project #1, merged on `v5-alpha`).

**Parity target:** Perceptual, against v4 `src/sketches/cymatics/` (`index.ts`,
`computeCellState.frag`, `renderCymatics.frag`, `audio.ts`, and the `screenshots/`).

**Internal name** `Cymatics` (`AppState::Cymatics` already exists in the state enum).
User-facing manifest name is **"Cymatics"** unless v4's HomePage used a different
display label (confirm during implementation; Line ships internally `Line` /
displays "Gravity", Dots / "Fabric").

## Goal

Port v4's Cymatics wave-simulation sketch to v5 at perceptual parity, bringing it
onto the v5 patterns established by Line and Dots, and carrying the v5-era upgrades
Madison asked for: a deeper settings surface, a screensaver/attract mode (v4 had
none), and the GPU-only data-flow discipline (graphics stay on the GPU; the audio
thread is fed only from CPU-side interaction scalars, never a GPU readback).

Cymatics is the **first 2D field/stencil compute sketch** in v5. Dots' shared
`particles/` engine is a particle *storage buffer* (single-pass, in-place) and does
not fit a neighbour-reading wave field, so Cymatics establishes a new **ping-pong
storage-texture compute** seam.

## Scope

**In scope (perceptual-parity target):**

- 2D wave simulation on a ping-pong texture pair (discrete wave equation from
  `computeCellState.frag`: 4-diagonal-neighbour force → velocity → height with
  decay, dual moving wave-sources, `activeRadius` alive-mask, `iGlobalTime` phase).
- N simulation sub-steps per render frame (v4 `numIterations`, default 20).
- Fullscreen render (`renderCymatics.frag`: height-gradient normal, two-light
  specular, body-colour mix, vignette, background, `skewIntensity`).
- Mouse/touch + **two-hand grab** interaction moving the two wave centres
  (the v4 centre state machine: grab-assignment, mirror-the-other-centre,
  `activeRadius` grow/decay, `numCycles` ramp).
- **Full faithful audio** (Madison's choice): the 6-oscillator + LFO +
  filtered-noise synth *and* the three samples (kick, risingbass one-shots; blub
  looping with volume + playback-rate control), all driven by CPU-side scalars.
- Bloomed wireframe **hand-mesh overlay** via the shared `hand_mesh` module
  (orange `#eb5938`).
- Manifest tile.

**In scope (v5 upgrades):**

- **Rich Dev settings surface** (physics, visual, interaction, audio, attract).
- **Attract/screensaver mode**: auto-wandering wave sources (slow incommensurate
  Lissajous), low-power.
- Audio engine **sample-bank generalization** (shared `wc-core` infra; the one
  new shared component this port introduces — see *Audio*).

**Out of scope:**

- A second grid-sim sketch (Waves) sharing the compute — note the seam, build only
  for Cymatics (YAGNI).
- WebGL2/CPU fallback (WebGPU-only target).

## Background: v4 Cymatics

- **Simulation** (`computeCellState.frag`): RGBA float cell state = `(height,
  velocity, accumulated_height, _)`. Per cell: force from 4 diagonal neighbours
  ×`FORCE_MULTIPLIER`, `velocity += force; velocity *= VELOCITY_DECAY`, `height +=
  velocity; height *= HEIGHT_DECAY`, two wave-sources injected as `2*sin(iGlobalTime)`
  within a texel radius, multiplied by an `aliveAmount` mask grown from
  `activeRadius`, with an early-out for inactive cells. Run via a Three.js
  `GPUComputationRenderer` ping-pong, N iterations/frame, `simulationTime` advancing
  `cycles·2π/N` per iteration.
- **Render** (`renderCymatics.frag`): reads the cell texture (linear-filtered),
  builds a normal from the height gradient, two-light specular (power-8), mixes
  `BASE_COL`/`BASE_BODY_COL` by height, adds a vignette + radial background, and a
  `skewIntensity` body-colour push. Aspect-corrected sim→screen UV.
- **Interaction** (`index.ts` `step()`): mouse press / two-hand grab move `center` /
  `center2`; free centres mirror the other; `activeRadius` lerps up while
  interacting, decays when idle; `numCycles` (frequency) ramps. `isReadyToSleep()`
  returns true when `activeRadius` is near its floor.
- **Audio** (`audio.ts`): six `OscillatorNode`s (base/unison/fifth/sub/high4/
  high4second) summed into a gain, LFO-modulated, plus white noise → bandpass; three
  `AudioClip`s (kick + risingbass one-shots, blub a volume/rate-controlled loop). All
  parameters set from the CPU scalars computed in `step()` (`activeRadius`,
  `numCycles`, `centerSpeed`, `slowDownAmount`). **No GPU readback.**

## Architecture

### Module layout — `crates/wc-sketches/src/cymatics/`

One concept per file, `mod.rs` entry, shaders external.

- `mod.rs` — `CymaticsPlugin::build`: settings registration, manifest, lifecycle
  (OnEnter/OnExit), idle veto, and registration of the shared `HandMeshPlugin`
  with Cymatics' config. Documents signal/data flow.
- `settings.rs` — `CymaticsSettings` (rich Dev surface, derive macro).
- `compute/mod.rs` — the ping-pong compute pipeline + render-graph node.
- `compute/sim_params.rs` — `CymaticsSimParams` (extract resource) + the
  per-iteration phase uniform.
- `render.rs` — `CymaticsMaterial` (fullscreen `Material2d`) + spawn.
- `systems/interaction.rs` — mouse/touch centre state machine + `activeRadius` /
  `numCycles` drivers (v4 `step()` logic, CPU-side).
- `systems/hand.rs` — two-hand grab → centres, via `HandTrackingState` / the
  attractor pattern.
- `systems/audio_coupling.rs` — compute the v4 scalars → push `SetCymaticsParam` /
  `TriggerCymaticsSample` through the lock-free ring.
- `screensaver.rs` — wandering wave-source attract driver.

Shaders: `assets/shaders/cymatics/simulate.wgsl`, `assets/shaders/cymatics/render.wgsl`.

### GPU simulation — ping-pong storage textures

Of three options (storage textures / storage buffers / `read_write` storage texture)
the recommended approach: **two `rgba32float` textures A/B** (Bevy `Image` assets,
usage `TEXTURE_BINDING | STORAGE_BINDING`). `rgba32float` (not f16) preserves the
small accumulated-height integration. A **render-graph compute node** (added before
`camera_driver`, mirroring Dots' `particle_compute`) runs **N iterations/frame**:
read texture X via `textureLoad`, write Y via `textureStore` (write-only storage —
avoids the `read_write`-storage downlevel requirement), swap each iteration. The node
leaves the final result in a **stable display texture** so the renderer samples a
fixed handle regardless of N's parity.

**Per-iteration phase** (v4 advanced `iGlobalTime` each sub-step) is fed by a
**dynamic-offset uniform array** — N pre-filled `IterParams { time }` entries, one
`set_bind_group` offset per dispatch. WebGPU-compatible (no push constants, which
WebGPU lacks). The main world fills the N times (base + i·dt) once per frame into
`CymaticsSimParams`; the node loops dispatches with dynamic offset i. Resolution is
`vertical_resolution × round(vertical_resolution · aspect)`; resize is
`requires_restart` (v4 behaviour). `simulate.wgsl` is a near-verbatim port of the
wave equation, decay constants (as settings), dual wave-source, alive-mask, and the
inactive-cell early-out.

### Render — fullscreen Material2d

`CymaticsMaterial` (`Material2d`, `AsBindGroup`) on a screen-sized quad tagged
`CymaticsRoot`, sampling the display texture (linear filter, as v4). `render.wgsl`
ports the lighting/specular/vignette/skew. Drawn by the normal 2D pass, so the shared
hand-mesh composite and bloom/tonemapping layer on top.

**v5 pipeline parity note:** v5 cameras are HDR + bloom + AgX; v4 output final sRGB
directly. The shared hand-mesh composite writes into the HDR `Rgba16Float` view
target, so Cymatics' camera must be HDR for bones to composite. The cymatics colours
will therefore pass through AgX — tune the palette under AgX during implementation
(or, if the look can't be matched, the camera's tonemapping is a tunable per the
Line/Dots precedent). Flagged as a parity-tuning item, not a blocker.

### GPU-only / no readback

Preserved by construction: the sim texture lives entirely in the render world; the
main world computes every audio parameter from CPU-side interaction scalars
(`activeRadius`, `numCycles`, `centerSpeed`, `slowDownAmount`) exactly as v4. Nothing
reads GPU → CPU. This satisfies the stated requirement without added machinery.

### Audio — sample-bank generalization + CymaticsSynth (shared-infra component)

v5 audio is pure-DSP (fundsp) with a **single** looping `background_pcm` baked into
`DspHost` at engine start (decoded by the existing `audio/background.rs` symphonia +
linear-resample helpers; the binary inserts `BackgroundSampleAsset` bytes). Full
faithful Cymatics audio means generalizing that one loop into a small **named sample
bank with triggerable one-shot + rate-controllable looping voices**. No new
dependency (symphonia + `symphonia-codec-vorbis`/`-format-ogg` are already in the
graph); the three v4 samples convert to `.ogg` (matching Line's `line_background.ogg`).

Per the pre-release "no backcompat cruft" rule, **replace** the single-background path
rather than keeping both:

- **`SampleBank`** (in `wc-core/audio`): decoded+resampled named PCM buffers, built at
  engine start from sample-asset resources the binary inserts (`line_background`,
  `cymatics_kick`, `cymatics_risingbass`, `cymatics_blub`). `line_background` becomes
  one bank entry; the existing `background_volume` knob keeps working.
- **Sample voices** (real-time-safe, preallocated): one-shot (kick, risingbass) and
  looping with a fractional `f32` playhead for `playbackRate` + volume (blub).
  Fractional rate = linear interpolation in `render`, no allocation.
- **`CymaticsSynth`** (fundsp, like `LineSynth`/`DotsSynth`): the 6-oscillator stack
  (base/unison/fifth/sub/high4/high4second) + LFO-modulated gain + bandpass-filtered
  white noise, with a `set_param` key table mirroring v4 (`osc_volume`,
  `osc_freq_scalar`, `blub_volume`, `blub_rate`, plus the noise/LFO derivations).
- **Commands** (keep `AudioCommand: Copy`): add `AddCymaticsSynth` /
  `RemoveCymaticsSynth` / `SetCymaticsParam { key, value }` and
  `TriggerCymaticsSample(CymaticsSampleId)` where `CymaticsSampleId` is a small `Copy`
  enum (`Kick`, `RisingBass`) — a clean one-shot edge trigger (the main thread detects
  the v4 `triggerJitter` edge; ~500 ms throttle).
- `audio_coupling.rs` computes the v4 scalars each frame, derives the v4 `step()`
  audio formulas (blub volume/rate, osc volume/freq-scalar), and pushes params; on an
  interaction-start edge it pushes `TriggerCymaticsSample(Kick)` + `(RisingBass)`.
  Audio coupling is gated off in screensaver (silent), matching Dots.

This is the one place the port touches shared code. It is lower-risk than the
hand-mesh dedup (one existing consumer — Line's background loop — and an additive
generalization), so it stays a component of this spec rather than its own; the plan
sequences it first and gates it on Line's audio still working.

### Interaction & lifecycle

- `CymaticsRoot` marker; `AppState::Cymatics` (exists); `SketchActivity` sub-state
  (exists). **OnEnter**: allocate the ping-pong textures, spawn the fullscreen quad +
  material, insert `CymaticsSimParams`, push `AddCymaticsSynth`. **OnExit**:
  `despawn_with::<CymaticsRoot>`, drop the sim resources (frees VRAM via handle
  ref-count), push `RemoveCymaticsSynth`.
- **Interaction** (`sketch_active`-gated `Update`): the two-centre grab state machine
  (mouse press / two-hand grab → `center`/`center2`, mirror-the-other logic,
  `activeRadius` grow/decay, `numCycles` ramp) ported from v4 `step()`. Mouse/touch via
  the v5 pointer input; hands via `HandTrackingState` + a per-hand attractor mirroring
  Dots/Line.
- **Idle veto**: keep `Active` while `activeRadius` is above its floor (v4
  `isReadyToSleep`).

### Hand-mesh overlay — shared module (now trivial)

Register the shared `crate::hand_mesh::HandMeshPlugin` with
`HandMeshConfig { app_state: AppState::Cymatics, bone_color: #eb5938 (235,89,56),
glow_intensity: 5.0, bone_radius: 10.0 }`. The global `HandMeshCompositePlugin` is
already registered once in `SketchesPlugin`. Cymatics renders in the main 2D pass with
**no post-process node**, so it adds **no** `HandMeshCompositeSet` ordering edge — the
composite runs in `EarlyPostProcess` after the 2D pass by default (the shared module
tolerates the absent edge, confirmed in sub-project #1's review).

### Attract mode — wandering wave sources

`screensaver.rs`, gated on `in_screensaver(AppState::Cymatics)`: the two wave centres
drift on slow incommensurate Lissajous paths (reusing Line's wandering-pulses idea),
emitting gentle continuous ripples with `activeRadius` held at a low ambient value.
Low-power via reduced `vertical_resolution`/`iterations` during attract + the
screensaver FPS cap. Audio coupling gated off (silent), matching Dots.

### Settings — rich Dev surface

`CymaticsSettings` via the derive macro. **User**: master visual brightness, a couple
of audio levels. **Dev** (ADVANCED toggle): `vertical_resolution` + `iterations`
(`requires_restart`); physics (`force_multiplier`, the three decay factors, wave-source
params); visual (the four colours, two light dirs, light brightnesses, vignette, skew
curve); interaction (min/interacting/target radii, grow/decay/lerp factors); audio
levels; attract-mode params. Restart-required fields drive the existing fade-out/reload
path.

## Plan staging

The implementation plan will stage roughly:

1. **Audio sample-bank + CymaticsSynth infra** (wc-core; gate: Line audio still works).
2. **Compute pipeline + `simulate.wgsl`** (ping-pong textures, N-iter node).
3. **Fullscreen render + `render.wgsl`**.
4. **Lifecycle + interaction** (centres, mouse/hand, idle veto).
5. **Audio coupling** (CPU scalars → ring; sample triggers).
6. **Hand-mesh** (register shared `HandMeshPlugin { config }`) + manifest tile.
7. **Attract mode** (wandering wave sources, low-power).
8. **Settings depth** + a `cargo xtask capture cymatics` scenario + AgX colour tuning
   + soak check.

## Testing

- AGENTS.md gate set (fmt, clippy, nextest `--all-features`, doctests, doc, deny,
  check-secrets).
- A `cargo xtask capture` scenario (`cymatics-synthetic` idle + an interacting variant
  + an attract variant), baselines seeded on this machine and visually confirmed (the
  operator reviews PNGs; no LLM spend).
- Audio: unit-test the sample-bank voices (one-shot trigger, looping fractional-rate
  playback, bank lookup) and the `CymaticsSynth` param table headlessly, like the
  Line/Dots synth tests.
- 8-hour soak before any release tag (multi-hour thermal-stability target).

## Risks

- **AgX/HDR vs v4 sRGB output** — Cymatics' palette may not match v4 under AgX.
  Mitigation: tune colours under AgX (parity item in stage 8); tonemapping is tunable
  per Line/Dots precedent.
- **rgba32float storage-texture support** on the WebGPU target — confirmed in the core
  spec; verify write-only storage of `rgba32float` on the deployment GPU early.
- **Sample-bank generalization regressing Line's background loop** — gated on Line's
  audio in stage 1; the generalization is additive (single loop becomes one bank entry).
- **N-iteration dynamic-offset uniform** sizing (iterations max 120) — bounded uniform
  array; validate alignment (256-byte dynamic-offset stride).
- **Per-frame allocation** in the compute/audio steady state — pre-allocate the
  per-iteration time buffer and audio scratch; refill with `clear()` (AGENTS.md hot-path
  rule).

## References

- v4 source: `.worktrees/v4/src/sketches/cymatics/` (`index.ts`,
  `computeCellState.frag`, `renderCymatics.frag`, `audio.ts`).
- v5 audio engine: `crates/wc-core/src/audio/{engine,dsp,background,command,line_synth,
  dots_synth}.rs`.
- v5 compute precedent (pattern, not reuse): `crates/wc-sketches/src/particles/compute.rs`.
- Shared hand-mesh (sub-project #1): `crates/wc-sketches/src/hand_mesh/` and
  `docs/superpowers/specs/2026-06-24-shared-hand-mesh-module-design.md`.
- Dots port precedent: `docs/superpowers/specs/2026-06-22-dots-fabric-design.md`.
- Visual harness: `tests/visual/CLAUDE.md`.
