# WaveConductor v5 roadmap

The plan-by-plan sequence for shipping v5.0 with full parity to the v4 React/Three.js gallery. Each entry is a distinct plan with a clear end-state and an estimated effort window. Plans land in order on `rewrite/bevy`; each closes with a tag (`v5-<name>`).

This is the index. Detailed implementation plans live under `docs/superpowers/plans/`; the design spec is `docs/superpowers/specs/2026-05-22-bevy-rewrite-design.md`; per-plan housekeeping items accumulate in `docs/superpowers/next-plan-carry-forwards.md`.

## Status

| Plan | Topic | Status | Tag |
| ---- | ----- | ------ | --- |
| 1 | Foundation (workspace, CI, lint gates) | ✅ shipped | `v5-foundation` |
| 2 | Lifecycle (state machine, leafwing keyboard actions) | ✅ shipped | `v5-lifecycle` |
| 3 | Input (mouse, touch, hand-tracking provider, pointer state) | ✅ shipped | `v5-input` |
| 4 | Audio scaffolding (cpal stream, ring buffers, default-silent DspHost) | ✅ shipped | `v5-audio` |
| 5 | Settings (Reflect-based, persistence, dev/user panels, derive macro) | ✅ shipped | `v5-settings` |
| 6 | Line skeleton + sketch scaffolding pattern | ✅ shipped | `v5-line` |
| 7 | Line simulation parity + idle veto hook | ✅ shipped | `v5-line-sim` |
| 7.5 | Test harness: synthetic input + shared `tests/common/` | ✅ shipped | `v5-test-harness` |
| 8 | Line rendering parity (gravity smear, star sprites, attractor rings) | ✅ shipped | `v5-line-render` |
| 9 | Line audio + reactivity coupling | ✅ shipped | `v5-line-audio` |
| 10 | Line polish + PARITY.md sign-off | ⏳ next | `v5-line-parity` |
| 11+ | Next sketch (Flame / Dots / Cymatics / Waves — order TBD) | future | — |

## Line parity (Plans 7–10)

The Plan 6 ship is the sketch *scaffolding* — multi-attractor physics, the post-process shader, audio synthesis, and visual sign-off are all still ahead. Four plans bring v5 Line to functional and perceptual parity with v4.

### Plan 7 — Line simulation parity + idle veto hook

**Goal:** v5 Line simulates particles with the same physics as v4. Visual layer still flat-shaded; audio still silent. End-state: side-by-side trajectory comparison against v4 looks right *if you ignore the chromatic glow.*

**Carry-forwards Phase 0:** Absorb the 9 items in `docs/superpowers/next-plan-carry-forwards.md` (save-on-exit flush, reflection panel type coverage, auto-reenter on `requires_restart`, render-graph trace logs, `min_binding_size`, drop `test_settings.rs` from production, `Single<&Window>`, asset-path release config, gravity tuning + remove 1Hz diagnostic).

**New scope:**

- Multi-attractor support — up to N=8 attractors in the SimParams uniform, each with `(x, y, power: f32)`. Mouse is one entry; future Leap hands fill the rest.
- Mouse attractor lifecycle: power=10 on press, geometric decay (0.9/frame) with floor=2, zeroed on release.
- Dual drag constants — `PULLING_DRAG=0.93075` (any attractor active), `INERTIAL_DRAG=0.53914` (idle). Both baked via `pow(C, timeStep)` for framerate independence.
- Size-scaled gravity — `G *= min(2^(width/836 - 1), 1)` so the sketch feels consistent across canvas sizes.
- Per-particle `original_xy` + `constrainToBox` reset semantics — out-of-bounds particles teleport home.
- Per-particle fade-in alpha over `FADE_DURATION = 3s`.
- Horizontal-line initial spawn at mid-Y with `((i % 5) - 2) * 2` sawtooth jitter — replaces v5's square grid.
- `particleDensity: f32` (per canvas-px) replaces `particle_count: u32`. Setting handles window resize cleanly; no `requires_restart` flicker on resize.
- **Sketch-side `IsReadyToSleep` veto hook** in `crates/wc-core/src/lifecycle/idle.rs`. Currently `advance_activity` transitions on elapsed time alone; this plan adds a `bevy::ecs::system::SystemId` or `Resource<Option<fn(&World) -> bool>>` mechanism so a sketch can keep itself `Active` while attractor power is still decaying. Without it, Line will go `Idle` mid-fling.
- **Architectural decision: CPU mirror of particle state.** Maintain `Vec<Particle>` on the host alongside the GPU storage buffer. The sim runs both — GPU for render, CPU as authoritative state for `ParticleStats` (Plan 9). Adds ~50µs/frame at 12k particles; trivial compared to the alternative of a per-frame GPU readback stall. Lock in this data shape now so Plan 9 doesn't churn it.

**Est. effort:** 5–7 days.

### Plan 8 — Line rendering parity

**Goal:** v5 Line *looks like* v4 Line. The gravity post-process is the single most important visual element — concentric ring trails emanating from the focal point with chromatic-shifted color separation. Screenshots in `src/sketches/line/screenshots/` on `main` show the target.

**Scope:**

- Port `star.png` (64×64 RGBA soft-diamond glow) — load as a Bevy `Image`, route into the line particle material.
- Replace flat-quad fragment shader with **textured point-sprite path**: sprite alpha-blended, 13px screen-space (matching v4's `size: 13, sizeAttenuation: false`), `vertexColors: true`.
- Attractor visual entity: 10 nested rings (mesh from `Annulus` or compute-shader-generated geometry), `0xC5E2CC` color, additive blending, rotation speed `(10 - idx) / 20 * power` per ring, scale `sqrt(power) / 5`. Spawned on attractor activation, despawned at power=0.
- **Gravity post-process pipeline** — WGSL port of `src/sketches/line/shaders/gravity/fragment.glsl`. 11-iteration ray-march of incoming + outgoing UV samples, distorted by `gravity(p, attractionCenter, G) = delta * (G / max(dot(delta, delta), 1e-4))`. Per-iteration chromatic shift via incoming `(1.0417, 1.0, 0.96, 1.0)` and outgoing `(0.96, 1.0, 1.0417, 1.0)` color factor accumulators. Mouse pull blend via `iMouseFactor`. Final gamma curve.
- Render-graph integration: post-process node attached after the Core2d main pass, reads the rendered scene as input texture and writes the gravity-smeared output.
- Uniforms wired from main world: `G`, `iMouseFactor`, `gamma`, `iMouse`, `iResolution`, `iGlobalTime` (driven from `Time` + temporary constants — Plan 9 plugs in the audio-reactive values).
- New setting: `gamma: f32` (default 1.0, dev category, no restart).

**Est. effort:** 3–5 days.

### Plan 9 — Line audio + reactivity coupling

**Goal:** v5 Line is audibly indistinguishable from v4 Line at fixed input, and the particle-stats feedback loop drives both the synth params and the shader uniforms. This is the biggest plan in the Line stack.

**Plan 4 left these explicitly deferred:** `DspHost::render` writes zeros (see the `// TODO Plan 6` at `crates/wc-core/src/audio/dsp.rs:74`). The cpal stream, ring buffers, and `SetMasterVolume`/`SetMuted` commands are real; per-sketch synthesis is not yet wired.

**Scope:**

- Extend `AudioCommand` with sketch-aware synthesis lifecycle: `AddSynth(SynthDef)`, `RemoveSynth(SynthId)`, `SetSynthParam(SynthId, ParamKey, f32)`. Sketch-side helpers in `wc-sketches`.
- Wire `fundsp` into `DspHost::render` — replace the zero-fill placeholder with real synthesis from the active synth graphs. The Plan 4 mute test gets re-validated against a non-silent source at this point.
- Port v4's `createAudioGroup` (Line voice graph) to a fundsp `SynthDef`:
  - Two oscillators at `BASE_FREQUENCY = 320`: square (`BASE/2`, detuned ±2 cents) and sawtooth (`BASE`), each gain=0.30
  - Low sawtooth at `BASE/4`, gain=0.90
  - Two 5-note chord stacks at `BASE` and `BASE×8` (intervals: unison, +12, +12+7, +24, +24+4 semitones)
  - White noise → lowpass (cutoff parameter) → lowshelf (2200Hz, +8dB) → gain
  - LFO at 8.66Hz modulating two bandpass filter cutoffs (Q=2.18)
  - Final stage: compressor (threshold=−50, knee=12, ratio=2) → double highshelf (`BASE×4`, `BASE×8`, both −6dB)
- **Background sample loading** — `line_background.mp3/ogg` (258KB/668KB on `main`). Per spec §5.12 we cannot use `bevy_audio` (one-shot SFX only). `fundsp/wav` feature is disabled. Decision deferred to plan-writing: either re-encode to WAV + enable `fundsp/wav`, or add `symphonia` for in-process mp3/ogg decode into a `Vec<f32>` PCM buffer. Loop the sample via fundsp `wave()` source.
- **ParticleStats** CPU computation (port of `src/particles/particleStats.ts`):
  - `averageVel = sqrt(sum(dx² + dy²) / N)`
  - `varianceLength = sqrt(varianceX² + varianceY²)`
  - `flatRatio = varianceX / varianceY` (1 = circular, large = horizontally flat, near-0 = vertically thin)
  - `groupedUpness = sqrt(averageVel / varianceLength)` — the single load-bearing scalar across audio AND visual
  - `normalizedEntropy = entropy / (width × 1.3839)` where `entropy = sum(length × log(length)) / N`
  - `normalizedVarianceLength = varianceLength / (0.28866 × width)`
- Per-frame coupling, `Update` system:
  - `flatRatio → LFO frequency target`
  - `222 / normalizedEntropy → bandpass filter cutoff target`
  - `2000 × normalizedVarianceLength → noise filter cutoff target`
  - `max(groupedUpness - 0.05, 0) × 5 → synth volume`
  - `triangleWaveApprox(now/5000) × (groupedUpness + 0.5) × 15000 → shader G`
  - `(1/15) / (groupedUpness + 1) → shader iMouseFactor`
- All synth-param writes flow through the `AudioCommandSender` ring; never block.

**Est. effort:** 7–10 days. The big risks are fundsp API drift from v4 Web Audio semantics and the mp3 decoding decision.

### Plan 10 — Line polish + PARITY sign-off

**Goal:** Line ships. `PARITY.md` in the sketch module is signed and the side-by-side capture matches v4 within the agreed perceptual tolerance.

**Scope:**

- **Heatmap-image spawn template.** Port `src/sketches/line/heatmapSampler.ts` — image → CDF on luminance × alpha → weighted random sampling via binary search. Add `spawn_template: Option<PathBuf>` (image picker setting). Sub-pixel jitter on sampled coordinates. Fallback to default horizontal-line spawn when the image is all-black or fully transparent.
- Any remaining items from `next-plan-carry-forwards.md` not absorbed in Plan 7's Phase 0.
- `crates/wc-sketches/src/line/PARITY.md` — parity target = `perceptual` (per spec §8). Reference media: pin v4 commit hash for a fixed-input capture. List approved deviations. Verdict line.
- Idle / screensaver behavioral parity check — v4 uses `idleTimeoutSeconds = 30` and `screenSaverTimeoutSeconds = 30` (additive). Match the totals in v5's `InteractionTimer` config.
- 8-hour soak test on Line — required per AGENTS.md before any release tag. Lock in the harness now so subsequent sketches inherit it.

**Est. effort:** 3–4 days.

**Total Line parity:** ~18–26 days from Plan 7 start to v5-line-parity tag.

## Beyond Line

Per spec §8 the v4 deck contains five sketches. Plans 11+ port them. Order is provisional — the actual sequence depends on which sketch's data demands surface architectural gaps soonest.

| Sketch | Parity target | Notes |
| ------ | ------------- | ----- |
| Line | Perceptual | Plans 7–10. |
| Flame | Perceptual | IFS fractal; recognizability matters, chaotic detail can drift. Similar render-graph shape to Line — leverages Plans 7–9 plumbing. |
| Dots | Perceptual | Particle character matters; shares most infrastructure with Line. Likely the lightest sketch port. |
| Cymatics | Physics-matched | 2025-era human-authored sketch. The visual *is* the simulation; numerical drift = wrong sketch. Likely the hardest port. |
| Waves | Perceptual | Audio-reactive with FFT bin response. Requires microphone capture + rustfft path that Plan 4 explicitly deferred. |

Each sketch ships its own `PARITY.md` and absorbs whatever carry-forwards have accumulated. Audio coupling generalizes after Plan 9; the synthesis registration shape established for Line should serve Waves and Cymatics with sketch-specific synth defs only.

## Pre-release tier

These land before tagging `v5.0.0` and merging `rewrite/bevy` → `main`. They are *not* per-sketch but cut across the workspace:

- **Distribution** (spec §5.7) — macOS DMG, Windows portable exe, AppImage, web bundle. CI matrix + signing + notarization. Asset-path config for release bundles is one of the Plan 7 carry-forwards and lands incrementally.
- **8-hour soak test** (AGENTS.md) — required before every release tag. Harness lands in Plan 10.
- **Perf audit harness** (spec §5.9) — FrameTimeDiagnosticsPlugin / EntityCountDiagnosticsPlugin / SystemInformationDiagnosticsPlugin readout into a CSV log. `bevy_framepace` spike (spec §5.12) — adopt if it improves thermal behavior, skip if free-running already meets the bar.
- **v4 perf-mode shim** (spec §5.11) — small IPC + start/stop bridge so v4 can stay on `main` until v5.0 is feature-complete.
- **Microphone capture + rustfft path** (deferred from Plan 4) — prerequisite for the Waves sketch.

## Convention

Each plan:

- Lives at `docs/superpowers/plans/YYYY-MM-DD-v5-plan-N-<topic>.md`.
- Closes with a commit + tag `v5-<topic>`.
- Has a Phase 0 that absorbs whatever items are in `next-plan-carry-forwards.md` at the time of writing. New items added during the plan's review pass roll forward to the next plan's Phase 0.
- Sketch-touching plans also update or create `crates/wc-sketches/src/<sketch>/PARITY.md` once the sketch reaches its parity target.

Plans are written via the `superpowers:writing-plans` skill and executed via `superpowers:subagent-driven-development`.
