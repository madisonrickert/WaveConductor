# Line — Parity Record

**Parity target:** Perceptual

**Reference media:** v4 main branch, `src/sketches/line/screenshots/gravity4_cropped.png` and the festival-loop recording from `scenarios/festival-loop.toml` at Pi Party 2026-03 timestamp.

**Approved deviations from v4:**

- Particle initial layout is a centered grid (was: heatmap-sampled image at `src/sketches/line/heatmapSampler.ts`). Heatmap spawn deferred to Plan 7.
- Particle color is velocity-magnitude-driven warm gradient (was: `starMaterial.ts` lookup). Acceptable under perceptual parity.
- No audio-reactive scaling (was: FFT band coupling via `createAudioGroup`). Deferred to Plan 7.
- WGSL compute kernel replaces CPU-side `particleSystem.ts`; numerics may diverge but character ("particles flow toward where you point, with momentum") is preserved.
- Render uses vertex-index-driven quads (6 vertices per particle, triangle list mesh) rather than instanced quads. Visually identical; chosen because Bevy 0.18's `Material2d` path does not support N-instance single-entity draws without a custom render phase.

**Verdict:** Plan 6 ships the architecture and the perceptual core. Visual character recheck scheduled after Plan 7 wires audio coupling and heatmap spawn.
