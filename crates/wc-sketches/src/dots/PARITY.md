# Dots — Parity Record

Internal name: `Dots`. Display name: "Fabric".

**Parity target:** Perceptual

**Reference media:**
- `.worktrees/v4/src/sketches/dots/screenshots/dots1.png` — idle chromatic fabric
- `.worktrees/v4/src/sketches/dots/screenshots/dots3.png` — idle chromatic fabric (alternate composition)
- `.worktrees/v4/src/sketches/dots/index.ts` — particle system, explode post-process, mouse attractor
- `.worktrees/v4/src/sketches/dots/audio.ts` — DotsSynth voice, LFO/bandpass/noise routing

**Plan progression toward parity:**

- **Plan D1 (shipped)** — shared `particles/` foundation + Line refactor + stationary-spring kernel
  term. Gravity engine relocated from `line/particles/` to shared `crates/wc-sketches/src/particles/`;
  Line passes `stationary_constant = 0.0` (spring term is a no-op for Line). Shared
  `simulate.wgsl` and `render.wgsl` established as the single engine Dots inherits.

- **Plan D2 (shipped)** — Dots scaffold + grid spawn + particle sim + mouse/touch attractor.
  `DotsPlugin` + `DotsSettings` registered; particles spawn on a grid with per-axis bleed and a
  random sawtooth jitter; `update_dots_mouse_attractor` applies v4's `PRESS_POWER × 100 ×
  size_clamp` formula; touch co-driven via `Res<Touches>`; lifecycle teardown drops
  `ParticleSimParams` + `CpuMirror` on `OnExit`.

- **Plan D3 (shipped)** — Explode post-process. `DotsPostProcessPlugin` ports v4's `shaders/dots/`
  WGSL: per-pixel spiral-loop (m2 spiral matrix corrected to column-major), per-channel shrink
  compounding, `pow` guard, `i_mouse` Y-flip. Incidental fixes: (a) Line's gravity-smear whiteout
  outside Line (`gamma = 0` in `gravity.wgsl` → `pow(rgb, 0) = 1.0`), gated via `LinePostParams`
  insert/remove `OnEnter/OnExit(Line)`; (b) render-world-removal bug (`ExtractResourcePlugin` does
  not propagate resource removal → stale post-process params leaked to the next sketch) fixed for
  both Line's smear and Dots' explode via `remove_*_post_params_if_absent` in `ExtractSchedule`.

- **Plan D4 (shipped)** — `DotsSynth` voice + envelope coupling (envelope-primary). `DotsSynth`
  slots alongside `LineSynth` in the shared engine; all v4 constants verbatim (`BANDPASS_BASE_HZ`,
  `OSC1_DETUNE`, `Q`, `NOISE_LEVEL`, etc.). Audio reactivity driven by an attack/release activity
  envelope keyed on mouse + hand presence, not GPU readback — same envelope-primary trade as Line.

- **Plan D5 (shipped)** — Leap/MediaPipe hand attractors + hand audio. `DotsHandAttractor` mirrors
  `LineHandAttractor`: v4's `leapAttractorPower` curve (grab^1.5 × 5^((-z+350)/160), EMA 0.005,
  floor < 0.05), power baked into the uniform per the same formula as the mouse. Hand presence drives
  the same envelope target as the mouse (loudest-wins MAX fold). Carry-forward: `dots_leap_power` +
  `palm_to_world` duplicate their Line counterparts; both scheduled for extraction to shared
  `particles/` in a later refactor.

- **Plan D6a (shipped)** — Screensaver attract-mode. `DotsScreensaverPlugin` drives the idle grid
  with curl-noise turbulence + fraction-kill + lifetime-respawn self-heal. v5 kiosk addition (v4
  Dots had no screensaver). Carry-forward: `hash.rs` (`wang_hash` / `hash_to_unit`) copied from
  Line; scheduled for extraction to shared `particles/`.

- **Plan D6b (shipped)** — Bone-wireframe hand rendering. `DotsHandMeshPlugin` +
  `DotsBoneCompositePlugin` port Line's per-hand skeleton visualization: 20-bone `LineList`
  icospheres, ice-blue `#b0d8ff`, dedicated overlay `Camera3d` (HDR, `Tonemapping::None`, no
  per-camera bloom; the bone image is composited additively into the main camera's HDR target
  pre-bloom so the main Bloom + AgX tonemap the bones coherently — known limit #75: no separate
  bloom rolloff), composited additively over the fabric. Incidental fix: Line's bone-composite leak
  (`Handle<Image>` clone keeps GPU texture alive after `OnExit`) fixed for both Line and Dots via
  `remove_*_hand_mesh_target_if_absent` in `ExtractSchedule`.

**Approved deviations from v4:**

- **WGSL compute kernel replaces CPU `particleSystem.ts`**: The particle simulation runs in a WGSL
  compute shader (`simulate.wgsl`), not in a TypeScript animation loop. A CPU mirror
  (`sim_cpu.rs`) runs for system tests but is NOT stepped in production.

- **Envelope-primary audio (flatRatio / variance gap)**: v4's `DotsSynth` in `audio.ts` read
  per-frame `computeStats`-derived `flatRatio` (particle distribution flatness) and `variance` to
  modulate LFO rate and cutoff shape. v5 replaces this with a smooth attack/release envelope keyed
  on attractor presence. The musical shape (rising on press, sustained during hold, decaying after
  release) is perceptually equivalent; the `flatRatio` LFO-rate and `variance`-shape subtleties are
  accepted perceptual gaps. Tuning constants in `audio_coupling.rs` shape the response.

- **Screensaver is a v5 addition**: v4 Dots had no idle attract-mode. The `DotsScreensaverPlugin`
  is a v5 kiosk-specific feature; no v4 baseline exists for screensaver output.

- **Idle veto while pointer held**: v5 keeps Dots awake while the mouse attractor's power is
  non-zero (the idle veto in `wc-core`). v4 Dots had no such gate — `hasActiveAttractors` never
  blocked screensaver entry because the mouse power asymptotes to 2.0, below the v4 threshold of
  2.01. The v5 behavior (pointer held → Dots stays Active) is deliberate kiosk UX; not a parity
  gap but a documented v5 divergence.

- **Known limit #75 — bones bypass main bloom rolloff**: The wireframe skeleton composites via the
  dedicated overlay camera, so its glow does not pass through the main camera's bloom rolloff curve.
  Same limit applies to Line and is tracked in issue #75.

## Verdict

**Status:** PENDING — operator sign-off required before tagging.

D1–D6b delivered the full implementation: shared particle foundation (D1), grid physics + mouse
attractor (D2), explode post-process (D3), audio synthesis + envelope coupling (D4), hand attractors
+ hand audio (D5), screensaver attract-mode (D6a), bone-wireframe skeletons (D6b). Automated tests
cover plumbing, math, and lifecycle; visual and hardware verification are operator-deferred per the
checklist below.

## Operator pre-tag checklist

Complete each item on the deployment machine (`cargo rund`) before creating the `v5-dots` tag.

### Visual (`cargo rund`)

- [ ] **D2 grid + pointer**: navigate to Dots, click and drag — confirm the grid pulls toward the
  cursor and disperses on release.
- [ ] **D3 explode chromatic fabric**: confirm the explode post renders the chromatic swirling
  "fabric" matching `dots1.png` / `dots3.png` — NOT a whiteout or flat color. Confirm spiral
  direction (particles converge inward toward cursor, not outward).
- [ ] **D3 cursor-centering**: confirm the explode effect is centered on the cursor position, not
  a fixed screen center.
- [ ] **D3 no Line-smear leak**: navigate Home → Dots and Dots → Line — confirm Line's gravity
  smear does not bleed onto the Dots scene or the Home page.
- [ ] **D3 Home unaffected**: confirm the Home screen renders normally after the `LinePostParams`
  gating change introduced in D3 (no whiteout, no visual corruption).

### Audio (`cargo rund` — ear tuning)

- [ ] **D4 synth character**: confirm the DotsSynth voice has the right warm bandpass + LFO
  character (matches v4 `audio.ts` by ear). Reference constants in
  `crates/wc-sketches/src/dots/audio_coupling.rs`: `BANDPASS_BASE_HZ`, `BANDPASS_RANGE_HZ`,
  `ENVELOPE_ATTACK_RATE`, `ENVELOPE_RELEASE_RATE`. Tune as needed.
- [ ] **D4 envelope shape**: confirm the sound rises on click, sustains during hold, and decays
  smoothly after release. The `flatRatio` LFO-rate modulation and `variance`-shape subtleties are
  an accepted gap (see Approved Deviations above).

### Hardware hand-tracking (`cargo rund` + Leap/MediaPipe)

- [ ] **D5 grab pulls grid**: close fist over the tracker — confirm particles converge to the
  projected palm position. Open hand — confirm particles disperse as power geometrically decays.
- [ ] **D5 proximity scales force**: confirm that depth (distance from sensor) scales the attractor
  force via the v4 `grab^1.5 × 5^((-z+350)/160)` curve.
- [ ] **D5 mouse + hand coexist**: confirm mouse click and an active hand grab can both influence
  the grid simultaneously (loudest-wins MAX fold for audio).
- [ ] **D6b wireframe skeletons render**: confirm 20-bone wireframe skeletons appear per tracked
  hand, overlaid on the fabric without corrupting the explode post.
- [ ] **D6b bone hue**: confirm the bone wireframe color is ice-blue (`#b0d8ff`). Retune if needed.

### Screensaver + soak (`cargo rund`)

- [ ] **D6a screensaver feel**: leave Dots idle past the screensaver timeout — confirm the grid
  morphs slowly and continuously (curl-noise turbulence), the field self-heals as particles respawn
  to their home positions, and the visual reads as calm and alive (not frozen, not frantic).
- [ ] **D6a soak-watch (idle grid drift)**: during the 8-hour soak below, observe whether the idle
  grid holds its general layout or drifts/loosens over hours. `stationary_constant = 0.01` runs in
  attract mode, so `original_xy` eases along the turbulence flow and respawns return to a slowly
  drifting home rather than the literal spawn grid. If undesirable, thread a softened
  `stationary_constant` into the attract bake in `screensaver.rs` (the lever is documented
  inline).

### Capture baselines (deployment machine + display required)

- [ ] **Seed `dots-synthetic` baseline**: `cargo xtask capture dots-synthetic --update-baselines`.
  Read the four captured PNGs (`frame_0030.png`, `frame_0060.png`, `frame_0120.png`,
  `frame_0240.png`) and confirm the fabric is visible and physically plausible (no black screen, no
  corruption). Commit the baseline PNGs.
- [ ] **Seed `dots-screensaver` baseline**: `cargo xtask capture dots-screensaver
  --update-baselines`. Read the seven captured PNGs and confirm: (a) fabric visible in every frame,
  (b) the field slowly morphs between frames (non-zero `delta_prev`), (c) no corruption. Commit the
  baseline PNGs.

### 8-hour soak

- [ ] **Run `dots_soak` before tagging**: `cargo test --release -p wc-sketches --test dots_soak --
  --ignored dots_soak_8h`. Asserts the sketch stays in `AppState::Dots` across ~1.7M update ticks.
  Watch for drift (soak-watch item above) and thermal anomalies.

### Carry-forwards (not blocking the tag, but logged)

- [ ] **Extract shared utilities** (`#74`): `dots_leap_power` + `palm_to_world` (D5),
  `wang_hash` / `hash_to_unit` (D6a), and the bone material/mesh/shader/composite (D6b) all
  duplicate their Line counterparts. Extract to `crates/wc-sketches/src/particles/` in a follow-up,
  fixing Line + Dots together.
- [ ] **Fix wrong "premultiplied-alpha composite" doc wording** in `bone_wireframe.rs` for both
  Line and Dots (the composite is purely additive; alpha is not consulted). Fix alongside the #74
  extraction.
- [ ] **Remove unused `#[allow]` in `bone_composite.rs`** for both Line and Dots. Fix alongside
  the #74 extraction.
