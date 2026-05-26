# Line — Parity Record

**Parity target:** Perceptual

**Reference media:** v4 main branch, `src/sketches/line/screenshots/gravity4_cropped.png` and the festival-loop recording from `scenarios/festival-loop.toml` at Pi Party 2026-03 timestamp.

**Plan progression toward parity:**

- **Plan 6 (shipped, tag `v5-line`)** — sketch scaffolding, single-attractor inverse-linear gravity, flat-color quads.
- **Plan 7 (shipped, tag `v5-line-sim`)** — multi-attractor physics with dual drag, size-scaled gravity, mouse-power decay, `original_xy` + constrain-to-box, fade-in α, horizontal-line spawn with sawtooth jitter, `particle_density` setting.
- **Plan 8 (shipped, tag `v5-line-render`)** — gravity-smear post-process shader, star sprite, attractor ring meshes, `gamma` setting.
- **Plan 9 (shipped, tag `v5-line-audio`)** — fundsp-based synthesis (bandpass cascade + LFO + noise + master gain mixed with looped `line_background.ogg`), `ParticleStats` CPU reduction over `LineCpuMirror`, coupling system writing per-frame `SetLineParam` (lfo_freq/bandpass_freq/noise_freq/volume) and overriding `LinePostParams.g_constant` + `i_mouse_factor` from groupedUpness × triangle-wave.
- **Plan 10 (tag `v5-line-parity`)** — heatmap-image spawn, signed verdict.

**Approved deviations from v4** (carried forward; verdict deferred until Plan 10):

- WGSL compute kernel replaces CPU-side `particleSystem.ts` for rendering; a parallel Rust CPU mirror runs the same math on the host (introduced in Plan 7) to feed `ParticleStats` in Plan 9 without a GPU readback stall. The two integrators may drift by ≤1% over long timescales due to floating-point order-of-operations differences; acceptable for groupedUpness and friends.

**Verdict:** pending (Plan 10).
