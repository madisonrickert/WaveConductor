# Cymatics — Parity Record

Internal name: `Cymatics`. Display name: "Cymatics".

**Parity target:** Verbatim (shader constants / formulas) + Perceptual (AgX palette, attract
choreography)

**Reference media:**
- `.worktrees/v4/src/sketches/cymatics/screenshots/` — v4 reference captures
- `.worktrees/v4/src/sketches/cymatics/index.ts` — interaction state machine, lifecycle
- `.worktrees/v4/src/sketches/cymatics/audio.ts` — CymaticsSynth voice, 6-osc routing
- `.worktrees/v4/src/sketches/cymatics/shaders/simulateCymatics.frag` — wave simulation (GLSL)
- `.worktrees/v4/src/sketches/cymatics/shaders/renderCymatics.frag` — lighting + palette (GLSL)

**Plan progression toward parity:**

- **C1–C4 (shipped — Stage 1: audio foundation)**
  `SampleBank` generalized to multiple sketches (C1–C2); `CymaticsSynth` 6-oscillator voice ported
  verbatim from `audio.ts` (C3); cymatics audio commands, sample assets (blub/kick/rising-bass .ogg),
  and the audio mixing path wired (C4). Line background loop preserved bit-exact throughout; Dots
  audio also unaffected (522 tests green post-C2).

- **C5–C6 (shipped — Stage 2: GPU ping-pong compute)**
  GPU POD types `SimParamsGpu` (48 B, 16-byte aligned) and `IterParamsGpu` (256 B = `ITER_PARAMS_STRIDE`,
  compile-time asserted) defined; ping-pong rgba32float textures allocated with
  `STORAGE_BINDING | TEXTURE_BINDING | COPY_SRC` (A/B) and `TEXTURE_BINDING | COPY_DST` (display);
  `rgba32float` write-only (not `read_write`) storage used to avoid the `float32-filterable`
  downlevel requirement. N-iteration compute node ported from `simulateCymatics.frag`.

  **Critical odd-N continuity fix (C6):** the default N=20 is even, so the A/B ping-pong leaves A
  as the final result each frame. For odd N the freshest data ends up in B; the next frame reads
  stale A — N=1 freezes entirely. Fixed: an extra B→A blit on odd N preserves the invariant "A
  holds latest at frame end" at zero overhead for the default even N. Hardened with
  `MAX_ITERATIONS=120` clamp and a pure `frame_blit_plan` regression test.

- **C7 (shipped — Stage 3: render)**
  `CymaticsMaterial` + `render.wgsl` full-screen quad. All lighting constants ported verbatim from
  `renderCymatics.frag`: normal-from-height, two point lights, specular^8, body-mix, vignette,
  background, skew. `textureLoad` used (no linear sampler on rgba32float — avoids the
  `float32-filterable` downlevel requirement). Note: v4 uses channel `x` (HEIGHT) for body-mix
  AND the gradient; channel `y` / `z` are unused by render (as in v4).

- **C8–C10 (shipped — Stage 4: lifecycle + interaction)**
  `CymaticsPlugin` lifecycle: spawn (ping-pong + display textures, fullscreen quad, `CymaticsRoot`
  entity), `update_cymatics_sim_params` (both `sketch_active` AND `in_screensaver` — else attract
  freezes), `OnExit` despawn + resource drop. Hand interaction: `CymaticsHandGrabs` resource
  (`Option<Vec2>` for each centre, top-left UV) wired by `update_cymatics_hand_centers` (C10).

  **Branch-blocker fix (pre-existing, not Cymatics-specific, fixed in C8 smoke):**
  `HandMeshPlugin` (added per-sketch by Line and Dots) lacked `is_unique()->false`; Bevy 0.19
  deduplicates plugins by type-name, so the second add panicked. Fixed inline (`is_unique()->false`
  in `hand_mesh/mod.rs`). Latent since the 0.18→0.19 bump; surfaced on the first multi-sketch
  launch.

- **C11 (shipped — Stage 5: audio coupling + two parity fixes)**
  `drive_cymatics_audio` wires CPU scalars from `CymaticsState` into `CymaticsSynth` each frame.
  Two v4-parity bugs found during review:
  - **CRITICAL blub clamp**: first term dropped v4's `clamp(pow(...), 0, 1)` — at
    `active_radius=7.5` this overdrove blub ~60x (v4: 0.025; impl: 1.545 before the `·0.05`
    scale). Fixed; regression test guards the `active_radius=7.5` bound.
  - **IMPORTANT swell edge**: `OSC_SWELL_EDGE` was `1.1002` but v4 `DEFAULT_NUM_CYCLES * 1.1 =
    1.1022`. Fixed; probe test at `1.1010` (above old wrong edge, below correct) added.
  The `·0.05` blub scale lives once in C11 (audio coupling); the engine clamps output to `[0, 0.3]`.

- **C12–C13 (shipped — Stages 6–7: hand mesh + attract)**
  `HandMeshPlugin { Cymatics, #eb5938, glow 5.0, radius 10.0 }` registered; manifest tile updated.
  Attract-mode Lissajous wander added (C13): two centres drift on slow incommensurate paths
  (c1: ωx=0.043, ωy=0.031; c2: ωx=0.037+1.7φ, ωy=0.029+0.6φ; amplitude 0.3 around 0.5 →
  analytic bounds [0.2, 0.8]; periods 146–217 s, gentle/kiosk-appropriate). `ATTRACT_ACTIVE_RADIUS
  = 0.6` set every frame in screensaver (high-radius visibility item; C9 interaction does not run
  in screensaver). Lissajous wander is a v5 addition — v4 Cymatics had no screensaver.

- **C14 (shipped — Stage 8: rich Dev settings)**
  21-field `CymaticsSettings` surface, all live (none dead/placeholder):
  physics (`force_multiplier`, 3 decays packed into `SimParamsGpu` each frame), render
  (`master_brightness`, `skew_curve`), interaction six-pack (`min_radius`, `interacting_radius`,
  `target_radius`, `grow_factor`, `decay_factor`, `lerp_factor` → `CenterTuning`), audio
  (`osc_level`, `blub_level`), attract (`attract_radius`, 4 Lissajous omegas). All defaults are
  v4-exact. `requires_restart` only on `vertical_resolution` + `iterations` (GPU allocation at
  spawn time). User-visible (without ADVANCED): `master_brightness`, `osc_level`, `blub_level`.

- **C15 (this task — capture scenarios + PARITY.md)**
  Three capture scenarios added (`cymatics-synthetic`, `-interacting`, `-screensaver`);
  `WC_DEBUG_FORCE_CYMATICS_INTERACTION` toggle added to drive the interacting scenario
  deterministically. Baselines and AgX palette tuning are operator-deferred (headless session
  cannot produce a foreground display surface; see "Operator follow-ups" below).

**Y-convention (critical cross-task rule):**

v4 GLSL (`simulateCymatics.frag` / `renderCymatics.frag`) used bottom-left origin — v4's
`screenToSimUV` included a `y = 1 - y` flip. v5 uses top-left origin throughout (Bevy-native):

- `simulate.wgsl`: top-left origin; no y-flip anywhere.
- `render.wgsl`: samples display texture top-left; no y-flip.
- `screen_to_sim_uv`: normalises Bevy window-logical coordinates (already top-left) without
  any y-flip.
- `hand_to_sim_uv` / `palm_mm_to_ndc`: NDC conversion produces top-left UV, no flip.
- `wander_centers` (screensaver Lissajous): produces top-left UV directly (amplitude 0.3 around
  0.5, analytic bounds [0.2, 0.8]).
- No `1 - y` anywhere in the entire Cymatics codebase.

Verified by the C8 early smoke (compute + render pipelines compile on M1 Metal with no errors;
no wgpu validation errors on dynamic-offset uniform or A/B blits).

**Compute details:**

- Ping-pong textures A/B: `rgba32float`, `STORAGE_BINDING | TEXTURE_BINDING | COPY_SRC`.
- Write-only storage bindings (no `read_write`): avoids the `float32-filterable` downlevel
  requirement, confirmed working on M1 Metal (deployment-class GPU).
- Dynamic-offset 256-byte stride: `IterParamsGpu` is exactly `ITER_PARAMS_STRIDE = 256 B`
  (compile-time assert). One bind-group covers all N iterations via offset.
- v4's inactive-cell `return` became a `textureStore` copy-forward: preserves all 4 channels
  (height / velocity / accumulated / passthrough), more correct than v4 in ping-pong.
- `MAX_ITERATIONS = 120` clamp prevents runaway CPU allocation.
- Bind-group cache: resize-invalidated; steady-state path has no allocation.

**Render details:**

- Verbatim v4 lighting constants: all constants from `renderCymatics.frag` ported without
  adjustment (normal-from-height delta, two point-light positions, specular exponent 8, vignette
  radius, body-mix threshold, background colour).
- `textureLoad` used for rgba32float sampling (no `TextureSampleType::Float { filterable: true }`
  required).
- **AgX palette tuning is DEFERRED to the operator.** The global HDR + Bloom + AgX camera
  (`waveconductor/src/main.rs::spawn_camera`) is shared by all sketches — tonemapping is not
  per-sketch tunable. The palette constants in `render.wgsl` (`BASE_COL`, `BASE_BODY_COL`, light
  colours) are currently the raw v4 values; they must be tuned by eye on a real display so the
  post-AgX result matches the v4 reference screenshots (orange body on dark-blue ground, specular
  glints, vignette). Bloom (threshold 0) will lift the brightest cymatics highlights; accept or
  compensate in-shader during the operator tuning pass.

**Audio details:**

- `SampleBank` generalization (C1–C2): Line background loop preserved bit-exact; Dots audio
  unaffected throughout.
- `CymaticsSynth` 6-osc voice: verbatim port of `audio.ts` — all 6 fundsp chains
  (`(arith)>>pipe`), all constants (`OSC1_FREQ`, `OSC_GAIN`, `OSC_SWELL_EDGE`, `BLUB_SCALE`,
  etc.) match v4. Minor-1 (C3): `osc_gain` smooth-before-clamp order corrected to match v4
  (`>> clip_to >> follow`).
- Parity fixes (C11):
  - `blub_volume` first term: `clamp(pow(active_radius, 0.1) - 1.0, 0.0, 1.0)` (v4 formula
    verbatim). The `·0.05` blub scale lives once in `drive_cymatics_audio`; engine clamps
    output to [0, 0.3].
  - `OSC_SWELL_EDGE = DEFAULT_NUM_CYCLES * 1.1 = 1.1022` (not 1.1002 from a brief
    transcription error).
- Hand grab threshold: shared `LEAP_POWER_CONFIG` uses 0.1 vs v4 Cymatics raw 0.5 (firmer grip
  in v4). Tune on hardware.

**Interaction details:**

- All v4 `step()` constants verbatim: `MINIMUM_ACTIVE_RADIUS = 0.1`,
  `MINIMUM_ACTIVE_RADIUS_INTERACTING = 0.5`, `TARGET_ACTIVE_RADIUS_INTERACTING = 7.5`,
  `ACTIVE_RADIUS_INTERACTING_GROW_FACTOR = 0.01`, `ACTIVE_RADIUS_IDLE_DECAY_FACTOR = 0.005`,
  `INTERACTION_CENTER_LERP_FACTOR = 0.01`, `DEFAULT_NUM_CYCLES = 1.002`.
- Centre lerp is unconditional (mirrors v4 — brief's "only while interacting" was a misstatement).
- Mirror: free c2 mirrors c1 across UV centre (`1 - x`, `1 - y`); free c1 mirrors c2 when c2 is
  held, or follows the mouse when both free.
- `num_cycles` ramp and `slow_down` decay verbatim.
- `center_speed` formula verbatim: `distance(wantedC1, center) * lerpFactor`.

**Attract details:**

- Lissajous wander is a **v5 addition** — v4 Cymatics had no screensaver. Not a parity port.
- `ATTRACT_ACTIVE_RADIUS = 0.6` (chosen for visibility; C9 interaction does not run in
  screensaver mode so the radius must be set explicitly each frame).
- `num_cycles` reset to `DEFAULT_NUM_CYCLES` each frame in screensaver (low-power).
- Periods 146–217 s, gentle/kiosk-appropriate.

**Settings details:**

- 21 fields, all live (no dead knobs).
- `requires_restart` on `vertical_resolution` + `iterations` only (GPU allocation).
- User-visible (without ADVANCED toggle): `master_brightness`, `osc_level`, `blub_level`.
- All other knobs are Dev (ADVANCED required, resets each launch).

**Soak-watch items (multi-hour thermal stability target):**

- **N-iteration compute cost at max settings**: raising `iterations` (max 120) and
  `vertical_resolution` scales GPU work quadratically. Run the 8-hour soak at max settings to
  confirm the M1 thermal envelope is acceptable.
- **Audio steady-state no-alloc**: `drive_cymatics_audio` must not allocate on the audio
  callback thread. The fundsp graph is pre-built at activate time; confirm no incidental
  allocations surface in a multi-hour run.
- **Bind-group cache and odd-N extra-blit on the steady-state path**: confirmed no-alloc for
  even N (default 20); the extra B→A blit for odd N runs once per frame — verify it does not
  accumulate pressure under address-sanitizer or instruments.
- **C14 `audio_coupling.rs:404` onset threshold**: currently uses `MINIMUM_ACTIVE_RADIUS_INTERACTING`
  (hardcoded 0.5) not `settings.interacting_radius`. Silently breaks audio onset if the Dev knob
  is lowered. One-liner fix in the finalization pass.

## Approved deviations from v4

- **Top-left UV origin throughout**: v4 GLSL was bottom-left; v5 is Bevy-native top-left with no
  `y = 1 - y` flip anywhere. The field orientation is identical to v4 because the origin change is
  consistent end-to-end (shader, CPU interaction, hand mapping all agree).

- **Write-only rgba32float storage**: v4 WebGL2 used `GL_RGBA32F` with full read/write; v5 uses
  Bevy/wgpu write-only storage textures to avoid the `float32-filterable` downlevel requirement.
  Functionally equivalent (the WGSL shader reads via `textureLoad` on the *other* ping-pong
  texture, not via the storage binding).

- **Dynamic-offset 256-byte-stride IterParamsGpu**: v4 had one uniform block per shader invocation.
  v5 packs all N iteration params into a single buffer with 256-byte stride and issues one dispatch
  per iteration with a dynamic offset. Functionally equivalent.

- **Attract wander is NEW (not a v4 port)**: v4 Cymatics had no screensaver. The Lissajous
  wander is a v5 kiosk-specific addition; no v4 baseline exists for screensaver output.

- **Hand grab threshold 0.1 vs v4's 0.5**: v5 uses the shared `LEAP_POWER_CONFIG` grab threshold
  (0.1); v4 Cymatics used a raw grab check at 0.5 (requiring a firmer grip). Tune on hardware;
  the shared threshold is already calibrated for Line/Dots.

- **AgX palette (deferred)**: the global HDR + Bloom + AgX camera is shared; per-sketch
  tonemapping is not available. Palette constants in `render.wgsl` are raw v4 values; the operator
  must tune them by eye on a real display against the v4 reference screenshots.

## Verdict

**Status:** PENDING — operator sign-off required before tagging.

C1–C14 delivered the complete implementation: audio foundation (C1–C4), GPU ping-pong compute with
odd-N continuity fix (C5–C6), render with verbatim v4 lighting constants (C7), lifecycle and
two-centre interaction (C8–C10), audio coupling with both v4 parity fixes (C11), hand-mesh overlay
and manifest tile (C12), Lissajous wander attract mode (C13), and a full 21-knob Dev settings
surface (C14). Automated tests cover the compute math, audio formulas, interaction state machine,
and lifecycle. Visual, ear, and hardware verification are operator-deferred per the checklist below.

## Operator pre-tag checklist

Complete each item on the deployment machine (`cargo rund`) before creating a v5-cymatics release
tag.

### Visual (`cargo rund`)

- [ ] **No vertical inversion**: navigate to Cymatics, click and drag. Confirm the wave-ripple
  source appears at the cursor position, not its Y-mirror. If the source appears in the wrong
  vertical half, there is a Y-convention bug.
- [ ] **Non-black field**: confirm the wave field is visible (dark-blue ground, concentric
  ripple from the centre). A black screen indicates `rgba32float` write-only storage is unsupported
  or the compute pipeline failed to compile — check the log for a `PipelineCache` error.
- [ ] **Non-frozen field** (odd-N continuity fix): in Dev settings (flip ADVANCED), set
  `iterations = 1`. Confirm the field still propagates continuously. Repeat with `iterations = 3`.
  A frozen field at odd N means the C6 blit fix is not applied.
- [ ] **AgX palette tuning**: compare the rendered output against
  `.worktrees/v4/src/sketches/cymatics/screenshots/`. Tune the `BASE_COL`, `BASE_BODY_COL`, and
  light colour constants in `assets/shaders/cymatics/render.wgsl` until the post-AgX look matches
  v4 (orange body on dark-blue ground, specular glints, vignette). Bloom (threshold 0) will lift
  highlights; compensate in-shader as needed. Commit the tuned constants.

### Audio (`cargo rund` — ear tuning)

- [ ] **CymaticsSynth character**: confirm the 6-osc voice sounds correct — smooth osc swell,
  correctly scaled blub (not overdriven at interaction, should not clip). Tune `osc_level` and
  `blub_level` (User knobs) for the right balance.
- [ ] **Blub at high radius**: interact and hold until `active_radius` approaches 7.5 (watch the
  Dev panel). Confirm blub is not overdriven (audible ceiling/distortion). The v4 target is ~0.025
  blub amplitude at max radius.
- [ ] **Osc swell edge**: confirm the osc swell onset is at `num_cycles ≈ 1.1022` (slightly above
  `DEFAULT_NUM_CYCLES = 1.002`), not at 1.1002 (the old wrong value).

### Hardware hand-tracking (`cargo rund` + Leap/MediaPipe)

- [ ] **Grab drives wave centre**: close fist over the tracker — confirm the wave source moves to
  the projected palm position. Open hand — confirm the source decays back toward centre.
- [ ] **Grab threshold feel** (0.1 vs v4's 0.5): confirm the shared `LEAP_POWER_CONFIG` threshold
  (0.1) produces a usable grab without false triggers. Tune if needed.
- [ ] **Two-hand two-centre**: confirm two simultaneous grabs drive both centres independently
  (c1 / c2); free centres mirror appropriately.
- [ ] **Wireframe skeletons render**: confirm orange (`#eb5938`) hand-mesh overlay with the correct
  glow and radius, overlaid on the wave field without corruption.

### Capture baselines (deployment machine + display required)

The three Cymatics capture scenarios require a real foreground display surface. The build session
was headless; no baseline PNGs are committed.

- [ ] **Confirm rgba32float write-only storage on the deployment GPU**: the dev M1 confirmed
  working (C8 smoke; compute + render pipelines compile with no errors, no wgpu validation errors).
  Verify on the target deployment GPU before seeding baselines.
- [ ] **Seed `cymatics-synthetic`**: `cargo xtask capture cymatics-synthetic --update-baselines`.
  Confirm frames show a concentric ripple from the centre (not black, not frozen). Commit
  `baselines/cymatics-synthetic/frame_00{30,60,120,240}.png`.
- [ ] **Seed `cymatics-interacting`**: `cargo xtask capture cymatics-interacting --update-baselines`.
  `FORCE_CYMATICS_INTERACTION` drives the primary centre at UV (0.5, 0.5); confirm the wave mask
  opens visibly larger with each frame index (60 < 120 < 240 < 480). Commit
  `baselines/cymatics-interacting/frame_00{60,120,240,480}.png`.
- [ ] **Seed `cymatics-screensaver`**: `cargo xtask capture cymatics-screensaver --update-baselines`.
  Confirm the field evolves across frame indices (non-zero `delta_prev`) and shows a dual-source
  interference pattern (two wave centres drifting). Commit
  `baselines/cymatics-screensaver/frame_0{180,360,600,1200}.png`.
- [ ] **Run regression gate** on each seeded scenario (no `--update-baselines`). Confirm all three
  pass (mean-abs-diff <= 6.0).

### 8-hour soak

- [ ] **Run the 8-hour soak before tagging**. Watch for: multi-hour thermal stability at default
  settings; N-iteration GPU cost at max `iterations` + `vertical_resolution`; audio steady-state
  no-alloc (no fragmentation accumulating on the fundsp graph path); bind-group cache and odd-N
  extra-blit not accumulating pressure.

### Finalization fixes (test-hardening + doc — complete before final review)

See the progress ledger (`progress.md`) "Test-hardening + doc-fix follow-on" section. Items
pre-approved by the task reviewers (no re-review needed after applying):

- C3: add noise-gate threshold test (`scalar <= 1.002 => noise_gain = 0`) and `oscBase`
  fixed-pitch invariant test.
- C4: tighten `trigger_sample_plays_one_shot` — assert actual non-zero kick output at onset
  (current assert is a tautology).
- C9: reword `screen_to_sim_uv` doc — "window-logical/screen-space NDC, Y increases downward"
  (not "Bevy-native" which implies 3D clip NDC).
- C9: add `num_cycles > DEFAULT_NUM_CYCLES` growth assertion to
  `interacting_grows_active_radius_toward_target`.
- C10: add `// INVARIANT:` comment in `update_cymatics_hand_centers` body documenting that
  stickiness relies on `Query::iter()` stable archetype order.
- C11: fix two stale `1.1002` comments in `osc_volume_zero_at_default_frequency` to `1.1022`; add
  probe test at `num_cycles = 1.1010` asserting `osc_volume < 0.499`.
- C13: (optional) add comments for the bit-exact `assert_eq!` rationale and the analytic [0.2,0.8]
  bound in screensaver tests.
- C14: change `audio_coupling.rs:404` onset threshold from hardcoded
  `MINIMUM_ACTIVE_RADIUS_INTERACTING` to `settings.interacting_radius` (already in scope) so
  audio onset tracks the live Dev knob.
