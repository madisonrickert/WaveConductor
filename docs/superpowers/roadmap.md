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
| 10 | Line polish + heatmap spawn + soak harness | 🟡 shipped, parity gaps deferred to Plan 11 | — |
| 11 | Line parity completion (rings, touch/hand activation, file picker, code sign-off) | 🟡 code shipped, manual gates open | `v5-line-parity` |
| 11.5 | Overlay UI parity (translucent buttons, settings panel chrome, nav, auto-fade) | ⏳ Line parity gate | `v5-overlay-ui` |
| 11.6 | Hand-tracking provider + Leap manual verification | ⏳ Line parity gate | `v5-leap-verified` |
| 12 | Next sketch (Flame / Dots / Cymatics / Waves — order TBD) | future | — |

> **Line is most of the way there.** Plans 7–10 carried the sketch from scaffolding through multi-attractor physics, the gravity-smear post-process, the fundsp synthesis graph, the audio↔visual reactivity coupling, the heatmap-image spawn template, and the AGENTS.md-required 8-hour soak harness. The first hands-on run on 2026-05-25 surfaced parity gaps that don't fit cleanly inside Plan 10's "polish" scope and so deferred to Plan 11: rotationally-symmetric attractor `Annulus` rings (no visible spin), no touch / hand-tracking pathway to attractor press, no file picker for `spawn_template`, and the manual side-by-side sign-off that flips `PARITY.md` from "PENDING" to a real PASS. Plan 11 closes those and earns the `v5-line-parity` tag. The architectural pattern established here — per-sketch plugin under `wc-sketches`, settings via the `wc-core` registry, `OnEnter`/`OnExit` lifecycle, audio reactivity via `AudioCommand`, a `PARITY.md` per module closing with a tagged verdict — generalizes cleanly to Flame, Dots, Cymatics, and Waves (Plan 12+).
>
> **Two manual gates still stand between Line and "shipped":** Plan 11.5 (overlay UI parity — the v4 button chrome, settings panel styling, nav, and auto-fade) and Plan 11.6 (hand-tracking provider + on-hardware Leap verification — the kiosk install's primary input modality). Plan 11 produces the `v5-line-parity` tag on the *code path*; 11.5 and 11.6 close the surface around it. No Plan-12 sketch port begins until both have landed.

## Line parity (Plans 7–11.6)

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

**Outcome:** Plan 10 shipped the heatmap-image spawn, the 8-hour soak harness, and the Phase-0 carry-forward drain. The first manual run (2026-05-25) surfaced four parity gaps that the implementation pass alone couldn't catch — they require eyes-on testing or out-of-scope features — and so deferred to Plan 11 rather than being shoehorned into Plan 10's "polish" scope.

### Plan 11 — Line parity completion

**Goal:** Close the gaps surfaced by the Plan 10 hands-on run, sign `PARITY.md`, and tag `v5-line-parity`.

**Scope:**

- **Attractor ring rotation visibility.** Replace `bevy::math::primitives::Annulus` (rotationally symmetric — perfect circle, no visible spin) with a low-segment polygonal ring mesh (likely `RegularPolygon` with 6–8 sides, or a custom mesh with stroked-line geometry) so the per-frame `(10 - idx) / 20 * power` rotation is perceivable. Cross-check against v4's `src/sketches/line/index.ts` attractor visual code — if v4 uses Three.js `RingGeometry(inner, outer, thetaSegments=6)`, port that exact geometry.
- **Touch and hand-tracking attractor activation.** `update_mouse_attractor` currently reads `Res<ButtonInput<MouseButton>>::just_pressed(Left)` only. The pointer-merge layer already routes touch and hand-source positions, but neither can trigger press. Add: `Res<Touches>` for `TouchPhase::Started`/`Ended` events, and a hand-tracking gesture (pinch? fist closure? closeness threshold?) for synthetic press. Critical for the kiosk touchscreen install.
- **`rfd`-based file picker** for `spawn_template`. Replace the free-text input with a "Browse…" button that opens a native file dialog (`rfd::FileDialog::new().add_filter("Image", &["png"])`). New `SettingCategory::FilePath { extensions: &[&str] }` variant; renderer adds the button alongside the text field. ~30 LOC + the `rfd = "0.15"` dep.
- **Per-field `#[serde(default)]` on `LineSettings`** so adding a new field (e.g. `gamma` in Plan 8) doesn't make existing persisted TOML fail the whole-section deserialize and silently revert all sibling values to defaults. Apply the same pattern to other settings structs preemptively.
- **Manual side-by-side parity capture.** Madison runs v5 (`cargo run -p waveconductor`) against v4 (`npm run dev` on the v4 `main` branch) at 1280×720, captures matching idle, mid-press, and mid-decay states, and signs the `PARITY.md` verdict from PENDING → PASS. The pinned v4 reference commit goes in the verdict.
- **Heatmap-spawn end-to-end verification.** Spot-check with at least one real PNG (probably `assets/sketches/line/star.png` and a hand-picked photograph) plus a deliberately-wrong path to exercise the horizontal-line fallback. Currently only unit-tested.

**Out of scope (handed to dedicated parity gates):**

- Plan 8's known-deferred items (post-process gating outside `AppState::Line`, per-frame uniform-buffer reuse) — fold into the next render-graph work that touches the area.
- **Hand-tracking provider implementation** (no `HandTrackingState` writer exists yet — pure stub from Plan 3). Plan 11 ships the gesture-edge handling behind the `hand-tracking-gestures` feature flag, tested with synthetic input only; the real `LeaprsProvider` and on-hardware verification land in **Plan 11.6** as a Line parity gate.
- **Overlay UI chrome** (translucent buttons, settings panel styling, nav, auto-fade) — kept out of the sketch scope and landed as **Plan 11.5**, the other Line parity gate.

**Est. effort:** 2–3 days.

**Total Line code parity:** ~20–29 days from Plan 7 start to `v5-line-parity` tag. Plans 11.5 and 11.6 add another ~5–9 days of manual-gate work before Line is considered fully shipped.

### Plan 11.5 — Overlay UI parity (Line parity gate)

One of the two manual gates that stand between Plan 11's code-complete tag and Line being declared truly shipped. Plan 11.5 ports v4's overlay UI surface — the chrome that sits on top of every sketch, not the sketch itself — so the kiosk install presents the v4 visual language and the next sketch port can plug into a finished UI shell instead of inheriting Line's bare-bones controls.

**Scope:**

- **Translucent buttons** matching v4's visual style — likely `bevy_egui` with custom `Visuals` (background tint, border radius, alpha) tuned to v4. Buttons share a single style applied across nav + settings + sketch-specific affordances.
- **Settings panel** — extends Plan 5's existing `bevy_egui` panel with the v4 visual style. Replaces the default-egui-frame look with translucent backdrop + matching button styling. The Plan 5 reflection-driven widget set stays; only the chrome changes. Plan 11 already landed the [`EguiPointerCaptured`](../../crates/wc-core/src/settings/pointer_capture.rs) gate so the new chrome doesn't double-trigger sketch interaction.
- **Navigation buttons** — sketch-picker, fullscreen toggle, settings open/close, info/about. Match v4's icon set + placement.
- **Auto-fading UI** — overlay UI fades out after N seconds of pointer inactivity, fades back in on any pointer event. Matches v4's kiosk-friendly behavior. Coupled to the existing `InteractionTimer` (Plan 2) and a new `UiOpacity` resource animated by an `Update` system.

**Reference:** match v4's overlay UI style exactly. Reference media lives in `.worktrees/v4/src/` — locate the overlay components and styling there before designing.

**Why a parity gate:** Madison's first hands-on run after Plan 11 surfaced that the UI surface is still missing relative to v4 — without it, the kiosk install doesn't pass the "looks like v4" bar. Doing this before the next sketch port also means Plans 12+ wire into a finished UI shell instead of inheriting Line's minimal placeholder; doing it after every sketch would mean re-touching each one to retrofit the chrome.

**Est. effort:** 3–5 days. Most of the cost is matching v4's pixel/animation feel rather than the underlying state machine.

**Carry-forwards Phase 0:** absorbs whatever items are in `next-plan-carry-forwards.md` at the time.

### Plan 11.6 — Hand-tracking provider + Leap manual verification (Line parity gate)

The second of the two manual gates between Plan 11's code-complete tag and Line shipping. Plan 11 wired the gesture-edge handling (pinch press / release, [`LastPinchState`](../../crates/wc-sketches/src/line/systems/mouse.rs) per-chirality) behind the `hand-tracking-gestures` feature, but tested it with synthetic input only. The kiosk install's primary input modality is the Leap Motion Controller, so Line cannot ship to the pi-party deployment until on-hardware operation is verified end-to-end.

**Scope:**

- **`LeaprsProvider` real implementation** — replace the [`LeaprsProvider`](../../crates/wc-core/src/input/providers/leap_native.rs) stub (currently returns `HandTrackingStatus::Disconnected` and logs a warning). Add `leaprs` crate dep (`LeapC` bindings); wire `start` / `stop` / `poll` to actually open the connection and emit `HandTrackingFrame` messages each frame.
- **Provider selection at startup** — the binary's `ActiveProvider` selection needs a runtime toggle (env var, CLI flag, or build-time feature). When Leap is available, the binary inserts `LeaprsProvider`; otherwise falls back to the mock provider. The `hand-tracking-gestures` feature should pull in the real provider by default.
- **Hands-on Leap testing** — on Madison's Leap hardware, verify:
  - Pinch above [`PINCH_PRESS_THRESHOLD`](../../crates/wc-sketches/src/line/systems/mouse.rs) (0.85) spawns an attractor at the projected hand position.
  - Releasing the pinch zeros the attractor.
  - Holding the pinch holds the attractor and the audio voice tracks the held envelope.
  - Two hands → two attractors. Multi-attractor physics already supports N=8; the gesture path needs to feed both `left()` and `right()` simultaneously without one stomping the other.
- **Hand-position projection.** The `HandTrackingState` carries the hand's 3D position in Leap-space coordinates. Plan 11.6 picks a projection to world-space pixels (likely a top-down ortho mapping calibrated against the Leap's mounting position above the kiosk). Document the projection so the parity capture can reproduce it.
- **Soak test on Leap input** — re-run the 8-hour soak harness with synthetic hand-tracking events to verify no leaks in the new provider path. The real Leap hardware soak can wait for the pi-party deployment dress rehearsal.

**Out of scope:**

- **Mediapipe / webcam fallback provider** — kiosk targets Leap exclusively. Mediapipe is a future hand-tracking option if Madison wants to demo on a laptop without the Leap controller, but not a parity gate.
- **In-air gestures beyond pinch** (grab, swipe, point) — Line uses pinch only. Future sketches that need richer gestures will extend `HandGestureEvent` and the gesture-detection systems.

**Est. effort:** 2–4 days. Risk lives in (1) `leaprs` crate ergonomics — last-touched several years ago, may need patching — and (2) the projection calibration against actual kiosk mounting.

**Carry-forwards Phase 0:** absorbs whatever items are in `next-plan-carry-forwards.md` at the time of writing.

## Beyond Line

Per spec §8 the v4 deck contains five sketches. Plans 12+ port them. Order is provisional — the actual sequence depends on which sketch's data demands surface architectural gaps soonest.

| Sketch | Parity target | Notes |
| ------ | ------------- | ----- |
| Line | Perceptual | Plans 7–11. |
| Flame | Perceptual | IFS fractal; recognizability matters, chaotic detail can drift. CPU-bound (tree structure doesn't parallelize to GPU); audio coupling stays where v4 has it (visitor stats during the same per-frame CPU traversal). No GPU↔CPU sync concern. |
| Dots | Perceptual | Particle character matters; shares most infrastructure with Line. **Keep particles on CPU** (matches v4); if particle counts ever demand GPU, port the audio coupling to the approximated-envelope pattern from Plan 11 Phase F. |
| Cymatics | Physics-matched | 2025-era human-authored sketch. The visual *is* the simulation; numerical drift = wrong sketch. GPU compute (ping-pong wave PDE) — and v4's audio coupling already reads CPU-side input scalars (`activeRadius`, `numCycles`, `centerSpeed`, `slowDownAmount`), never GPU state. This is the architectural reference for the universal pattern below. |
| Waves | Perceptual | Audio→visual coupling (FFT of microphone). Requires microphone capture + rustfft path that Plan 4 explicitly deferred. Visuals are a closed-form CPU heightmap; no GPU compute needed. |

Each sketch ships its own `PARITY.md` and absorbs whatever carry-forwards have accumulated.

### Universal audio-coupling pattern (codified during Plan 11 Phase F)

**Audio derives from CPU-side simulation *inputs*, never from GPU-side simulation *outputs*.**

The pattern surfaced when Line's Plan 7 GPU-compute pipeline created a CPU↔GPU sync problem: the audio coupling needed per-frame particle statistics, but the authoritative particle state lived on the GPU. Plans 7–10 worked around it with a `LineCpuMirror` running parallel physics on the host. Plan 11 Phase F replaced that mirror with smoothed CPU envelopes driven by `MouseAttractorState` events — the audio coupling now reads attractor power directly, not the per-particle reduction, at ~1µs/frame instead of ~50µs.

The architectural insight: v4's Cymatics sketch already does this naturally. Its GPU compute simulation is driven by CPU-side parameters (`activeRadius`, `numCycles`, etc.); the audio reads those same parameters. The GPU is never read back. **Cymatics is the reference; Line's Phase F brings it into the same shape.**

Apply to future sketches:

- **Identify the CPU-side inputs that drive the simulation** (mouse position, attractor power, time-since-event, mode/setting changes, etc.).
- **Derive audio control signals from those inputs**, not from per-particle / per-cell statistics computed off GPU state.
- **Use smoothed envelopes** (attack/release on rising/falling edges of input events) to produce the right *perceptual shape* — rising on activity, plateauing during sustained input, decaying after release. Tune the constants against v4 perceptually; document them as named consts with rustdoc.
- **Approved deviation**: audio output won't be mathematically equivalent to v4 frame-by-frame, but IS perceptually equivalent. Document in `PARITY.md` per sketch.

Implications per sketch:

- **Flame**: no change needed — visitor stats are already CPU-side, IFS is CPU-bound, no GPU coupling.
- **Dots**: simplest path is to keep particles on CPU (v4-faithful, no mirror, no envelope work). Only fall back to envelope approximation if particle counts force a GPU port.
- **Cymatics**: copy v4's pattern directly. CPU drives the inputs that feed the GPU compute; audio reads from those same CPU inputs.
- **Waves**: audio is INPUT (microphone FFT), not output. Visuals derive from CPU heightmap. No coupling concern.
- **Future post-v4 sketches**: design with this pattern from day one. If you need a per-particle reduction for audio, that's a smell — derive from inputs instead.

The synthesis registration shape established for Line (`AudioCommand::AddLineSynth` / `RemoveLineSynth` + per-synth-param messages over a lock-free ring) is the right pattern; future sketches add their own `Add<Sketch>Synth` variants with sketch-specific param keys.

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
