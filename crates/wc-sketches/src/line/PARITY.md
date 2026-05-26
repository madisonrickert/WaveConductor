# Line — Parity Record

**Parity target:** Perceptual

**Reference media:** v4 main branch, `src/sketches/line/screenshots/gravity4_cropped.png` and the festival-loop recording from `scenarios/festival-loop.toml` at Pi Party 2026-03 timestamp.

**Plan progression toward parity:**

- **Plan 6 (shipped, tag `v5-line`)** — sketch scaffolding, single-attractor inverse-linear gravity, flat-color quads.
- **Plan 7 (shipped, tag `v5-line-sim`)** — multi-attractor physics with dual drag, size-scaled gravity, mouse-power decay, `original_xy` + constrain-to-box, fade-in α, horizontal-line spawn with sawtooth jitter, `particle_density` setting.
- **Plan 8 (shipped, tag `v5-line-render`)** — gravity-smear post-process shader, star sprite, attractor ring meshes, `gamma` setting.
- **Plan 9 (shipped, tag `v5-line-audio`)** — fundsp-based synthesis (bandpass cascade + LFO + noise + master gain mixed with looped `line_background.ogg`), `ParticleStats` CPU reduction over `LineCpuMirror`, coupling system writing per-frame `SetLineParam` (lfo_freq/bandpass_freq/noise_freq/volume) and overriding `LinePostParams.g_constant` + `i_mouse_factor` from groupedUpness × triangle-wave.
- **Plan 10 (shipped, tag `v5-line-parity`)** — heatmap-image spawn, soak harness, signed verdict.
  - **Phase A** — heatmap-image spawn template ported from v4's
    `src/sketches/line/heatmapSampler.ts`. New `LineSettings.spawn_template:
    String` field (Text setting, `requires_restart`); empty = horizontal-line
    layout (v5-line-sim baseline), non-empty = PNG path passed to
    `heatmap::sample_from_heatmap` which builds a CDF over luminance × alpha
    and inverse-CDF-samples `count` window-space positions. Errors (missing
    file, undecodable, all-zero weight) fall back to the horizontal layout.
  - **Phase B** — 8-hour soak harness at
    `crates/wc-sketches/tests/line_soak.rs`. `#[ignore]`-d so normal CI does
    not run it; Madison invokes it before tagging a release via
    `cargo test --release -p wc-sketches --test line_soak -- --ignored line_soak_8h`.
    Drives ~1.7M `app.update()` ticks with synthetic cursor motion and a
    press/release cycle, asserts the sketch stays in `AppState::Line`.
    Runs under `MinimalPlugins`, so the renderer is not exercised; a
    `DefaultPlugins` variant is reserved for Plan 11+.

**Approved deviations from v4** (carried forward; verdict signed below):

- WGSL compute kernel replaces CPU-side `particleSystem.ts` for rendering; a parallel Rust CPU mirror runs the same math on the host (introduced in Plan 7) to feed `ParticleStats` in Plan 9 without a GPU readback stall. The two integrators may drift by ≤1% over long timescales due to floating-point order-of-operations differences; acceptable for groupedUpness and friends.

## Verdict

**Status:** PENDING MANUAL VERIFICATION.

The harness, math, and asset ports are complete. Plans 7–10 land the
multi-attractor physics, the gravity-smear post-process, the fundsp
synthesis graph + `line_background.ogg` loop, the audio↔visual
reactivity coupling, and the heatmap-image spawn template — all
covered by automated integration tests
(`crates/wc-sketches/tests/line_input.rs`,
`crates/wc-sketches/tests/line_lifecycle.rs`) and a `#[ignore]`-d
8-hour soak (`crates/wc-sketches/tests/line_soak.rs`).

**Tag points for the side-by-side capture:**

- **v5** — `v5-line-parity` tag on this repository (`rewrite/bevy`
  branch HEAD at the time of tagging).
- **v4** — `main` branch on this same repository at commit
  `3b85676` ("Bump version to 4.2.0"). The v4 codebase lives at the
  repository root prior to the `rewrite/bevy` branch — `npm run dev`
  on `main` boots it.

**Human-in-the-loop step:** Madison runs both apps at 1280×720 and
confirms perceptual parity in three states:

1. **Idle** — no input. Particles settle to the horizontal-line spawn
   (or to the heatmap if `spawn_template` is set).
2. **Mid-press at center** — left button held at canvas center. Gravity
   smear concentric rings, chromatic shift, attractor ring rotation.
3. **Mid-decay (~5s after release)** — power decays geometrically, idle
   veto holds the sketch Active, audio reactivity tracks `groupedUpness`
   back down through silence.

**Known remaining parity deviations:** none expected. The CPU mirror
integration drift listed under "Approved deviations" is the only
documented divergence and is bounded at ≤1% over long timescales —
imperceptible at the festival-loop horizon.
