# Plan 9: Line Audio + Reactivity Coupling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** v5 Line is audibly indistinguishable from v4 Line at fixed input, and the particle-stats feedback loop drives both the synth params and the post-process shader uniforms. This is the biggest plan in the Line stack.

**Architecture:** Three coupled tiers land in this plan:

1. **Audio synthesis.** Plan 4 left `DspHost::render` writing zeros. Plan 9 builds the Line synth voice graph in fundsp (osc + chord + LFO + bandpass + noise + compressor) inside the audio thread. Activated/deactivated on `OnEnter`/`OnExit(AppState::Line)` via new `AudioCommand::AddLineSynth` / `RemoveLineSynth`. Background mp3 sample loops alongside the synth.

2. **`ParticleStats` CPU computation.** Plan 7's `LineCpuMirror` (parallel CPU integrator) provides per-particle velocities and positions. A new `ParticleStats` resource holds `averageVel`, `varianceLength`, `flatRatio`, `groupedUpness`, `normalizedEntropy`, `normalizedVarianceLength` — direct port of v4's `src/particles/particleStats.ts::computeStats`. Updated each `Update` while Line is `Active`.

3. **Reactivity coupling.** A per-frame `Update` system reads `ParticleStats` and writes:
   - `AudioCommand::SetLineParam` messages for filter freq, LFO freq, noise freq, volume
   - `LinePostParams.g_constant = triangleWaveApprox(t/5000) * (groupedUpness + 0.5) * 15000`
   - `LinePostParams.i_mouse_factor = (1/15) / (groupedUpness + 1)`

**Tech Stack:** `fundsp` 0.21 (already in `[workspace.dependencies]` since Plan 4, with `default-features = false`). Background sample loading via `symphonia` (new workspace dep) — decode mp3/ogg at startup into a `Vec<f32>` PCM buffer the audio thread owns. `bytemuck` continues for SimParams.

**Reference spec:** §5.4 (audio architecture).

**Reference v4 sources** (read-only via `git show main:<path>`):
- `src/particles/particleStats.ts` — the stats formulas
- `src/sketches/line/audio.ts` — the voice graph
- `src/sketches/line/index.ts::step()` — the per-frame coupling

**Branch:** `rewrite/bevy`. Pre-flight: HEAD at or after `v5-line-render` (`eaa0e78`).

---

## Scope check

Plan 9 closes audio + reactivity. Heatmap-image spawn, `PARITY.md` final sign-off, and 8-hour soak harness are Plan 10.

Six phases, six commits. Phase F pushes and tags `v5-line-audio`.

## File map

**Modified:**

- `Cargo.toml` (workspace) — add `symphonia` (mp3/ogg decoder).
- `crates/wc-core/Cargo.toml` — depend on `symphonia`.
- `crates/wc-core/src/audio/command.rs` — `AudioCommand::AddLineSynth`, `RemoveLineSynth`, `SetLineParam { key, value }`.
- `crates/wc-core/src/audio/dsp.rs` — build the Line synth in fundsp; activate/deactivate on commands; render the active mix.
- `crates/wc-core/src/audio/state.rs` — track `LineSynthActive: bool` (mirror for sketches that want to know).
- `crates/wc-sketches/src/line/particle_stats.rs` — *new* — port of v4's `computeStats`.
- `crates/wc-sketches/src/line/audio_coupling.rs` — *new* — per-frame system writing audio commands + post params.
- `crates/wc-sketches/src/line/mod.rs` — register the new modules; wire OnEnter/OnExit to add/remove the synth; install the coupling system.
- `crates/wc-sketches/src/line/systems/sim_params.rs` — let coupling system override `g_constant` and `i_mouse_factor`.

**New asset:**

- `assets/sketches/line/line_background.ogg` — ported from v4 `main:src/sketches/line/audio/line_background.ogg` (668 KB).

---

# Phase 0 — Plan 7/8 carry-forwards

Six items from the carry-forward log most relevant to Plan 9's work.

### Task 1: Pre-allocate `LinePostParams` uniform buffer (carry-forward #54)

In `crates/wc-sketches/src/line/post_process.rs`, mirror the compute pipeline pattern: allocate a persistent `Buffer` once in `PostProcessPipeline::from_world`, write via `queue.write_buffer` each frame in the node. No per-frame allocation.

### Task 2: Gate post-process to `AppState::Line` (carry-forward #53)

Option (b) from the carry-forward: in `update_sim_params` (or a new system), zero `LinePostParams.g_constant` when `AppState != Line`. This way the shader runs but produces near-zero smear outside Line. Simpler than conditional render-graph edges.

### Task 3: `MOUSE_POWER_PRESS` in lifecycle test (carry-forward #50)

Replace the local `SEEDED_MOUSE_POWER = 10.0` in `crates/wc-sketches/tests/line_lifecycle.rs` with the imported `MOUSE_POWER_PRESS` const.

### Task 4: `SIM_PARAMS_SIZE` cast doc (carry-forward #51)

In `crates/wc-sketches/src/line/compute.rs`, add an inline comment on the `as u64` line in `SIM_PARAMS_SIZE` explaining that `u64::try_from(usize)` isn't const-stable.

### Task 5: `cursor_moved_reader.read().last()` design comment (carry-forward #52)

In `crates/wc-core/src/input/pointer.rs::pointer_merge_system`, add a one-line comment noting intermediate-position discard is intentional ("newest wins; we want pointer position, not motion path").

### Task 6: Commit Phase 0

```bash
git add crates/wc-sketches/src/line/post_process.rs \
        crates/wc-sketches/src/line/systems/sim_params.rs \
        crates/wc-sketches/tests/line_lifecycle.rs \
        crates/wc-sketches/src/line/compute.rs \
        crates/wc-core/src/input/pointer.rs

git commit -m "$(cat <<'EOF'
Plan 9 Phase 0: Plan 7/8 carry-forwards

Pre-allocate the LinePostParams uniform buffer (queue.write_buffer
each frame instead of create_buffer_with_data); zero g_constant
outside AppState::Line so the post-process is visually no-op there;
use production MOUSE_POWER_PRESS in the lifecycle test; document the
SIM_PARAMS_SIZE const-cast and the intentional drop of intermediate
CursorMoved positions in pointer_merge_system.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

# Phase A — `AudioCommand` extension + fundsp graph

The audio thread learns to build and tear down the Line synth.

### Task 7: Extend `AudioCommand`

**File:** `crates/wc-core/src/audio/command.rs`

Add variants:

```rust
/// Build and activate the Line sketch's synth voice graph. Idempotent: a
/// second AddLineSynth while one is active is a no-op.
AddLineSynth,
/// Stop the Line synth. Idempotent.
RemoveLineSynth,
/// Set a named parameter on the Line synth. `key` is a `&'static str`;
/// see [`super::dsp::LineSynth`] for the legal set. Out-of-range or unknown
/// keys are logged and dropped.
SetLineParam { key: &'static str, value: f32 },
```

Update the `match` arms in any consumer (`DspHost::apply`). Document the `&'static str` constraint (must be a `'static` string literal because the audio thread runs in an `Arc<...>`-free environment).

### Task 8: Build the Line synth in `DspHost`

**File:** `crates/wc-core/src/audio/dsp.rs`

This is the bulk of Phase A. Replace the silent `render` with the fundsp Line graph.

- [ ] Add an `Option<LineSynth>` field to `DspHost`, where `LineSynth` is a new struct holding the fundsp signal-processing graph + a `BlockRateAdapter`-managed param state (target frequencies for the bandpass filters, target gain values, etc.).

- [ ] `DspHost::apply(AddLineSynth)` constructs the graph and stores it. `RemoveLineSynth` drops it. `SetLineParam` updates the target state.

- [ ] `DspHost::render` mixes:
  - `gain` (the master volume + mute logic, already present)
  - Plus the Line synth's `tick`/`get_stereo()` output, if active
  - Plus the background sample player (Phase C)

The fundsp 0.21 graph shape (per v4's `createAudioGroup`):

```
let lfo = sine_hz(8.66) * 0.06;  // LFO output ±0.06 of filter freq

// Voices
let v_sq = square_hz(160.0) * detune(2.0) * 0.3;
let v_saw = saw_hz(320.0) * 0.3;
let v_low = saw_hz(80.0) * 0.9;
let chord_base = chord(320.0, &[0, 12, 19, 24, 28], &["sine", "saw", "saw", "saw", "sine"]) * 0.5;
let chord_high = chord(2560.0, ...) * 0.5;

// White noise → lowpass → lowshelf
let noise = white() * 1.0;
let noise_lp = noise >> lowpass_hz_q(0.0, 1.0);   // freq driven by SetLineParam
let noise_shelf = noise_lp >> lowshelf_hz(2200.0, 8.0);

// Bandpass cascade with LFO modulation
let mut bp_freq = 0.0_f64; // target updated by SetLineParam
let bp1 = ... >> bandpass_hz_q(bp_freq + lfo, 2.18);
let bp2 = bp1 >> bandpass_hz_q(bp_freq + lfo, 2.18);

// Compressor + double highshelf
let final = (noise_shelf + bp2 * 0.4) >> limiter(0.01)
    >> highshelf_hz(1280.0, -6.0)
    >> highshelf_hz(2560.0, -6.0);

// Source gain (set by SetLineParam volume)
final * source_gain
```

> **fundsp 0.21 API caveat:** the exact constructor names (`square_hz` vs `saw_hz` vs `sine_hz`, `lowpass_hz_q`, `bandpass_hz_q`, `highshelf_hz`, `limiter`) may differ slightly. The implementer reads `cargo doc -p fundsp --open` or the `fundsp` README and adapts. The *shape* is correct; the constructor names may need adjustment.

> **Smoothing:** fundsp 0.21 has `BlockRateAdapter` to ramp parameter changes smoothly. v4 uses `setTargetAtTime(target, time, 0.016)` which is roughly equivalent to a 16ms exponential approach. fundsp's `smooth` operator or `var` + `lowpass` cascade achieves this.

> **Workspace `fundsp` features:** Phase 4 set `default-features = false`. Enable the `wav` feature (`fundsp = { version = "0.21", default-features = false, features = ["wav"] }`) so we can decode the background WAV. Or use the Phase C approach (symphonia).

### Task 9: Plumb `apply` through

Update `DspHost::apply` to handle the three new variants. Update Plan 4's existing tests (`set_master_volume_clamps_range`, `set_muted_updates_state`, etc.) to confirm the new variants don't break existing behavior. Add new unit tests for synth lifecycle.

### Task 10: Commit Phase A

```
git commit -m "$(cat <<'EOF'
Plan 9 Phase A: extend AudioCommand and build the Line synth in fundsp

DspHost gains an Option<LineSynth> activated on AddLineSynth and torn
down on RemoveLineSynth. SetLineParam routes named param updates to
the graph's target state with BlockRateAdapter-driven smoothing.
fundsp's `wav` feature is enabled in the workspace to support Phase
C's background sample. The render path mixes (master_gain × (synth +
background)).

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

# Phase B — Background sample loading

Load `line_background.ogg`, decode to PCM, play in a loop alongside the synth.

### Task 11: Port `line_background.ogg`

```bash
mkdir -p assets/sketches/line
git show main:src/sketches/line/audio/line_background.ogg > assets/sketches/line/line_background.ogg
```

(Use the .ogg, not the .mp3. fundsp `wav` feature decodes WAV; we'll decode the OGG via `symphonia` since fundsp's WAV path doesn't help here.)

### Task 12: Add `symphonia` dep

`Cargo.toml` workspace:

```toml
symphonia = { version = "0.5", default-features = false, features = ["vorbis", "ogg"] }
```

`crates/wc-core/Cargo.toml` `[dependencies]`:

```toml
symphonia = { workspace = true }
```

### Task 13: Decode the sample at startup

In `crates/wc-core/src/audio/engine.rs::start_audio_engine`, load `assets/sketches/line/line_background.ogg`, decode to a `Vec<f32>` interleaved PCM at the stream's sample rate (resample if needed). Send to the audio thread via a one-shot channel or as part of the DspHost initialization.

### Task 14: Integrate into the render path

In `DspHost`, add a `background_pcm: Vec<f32>` field + a `playhead: usize`. Each `render()` call advances the playhead (mod the buffer length, for looping) and mixes the sample into the output.

Volume modulated by `SetLineParam { key: "background_volume", value }` so reactivity coupling can attenuate it.

### Task 15: Commit Phase B

---

# Phase C — `ParticleStats` CPU computation

Port `computeStats` to read `LineCpuMirror`.

### Task 16: Create `particle_stats.rs`

**File:** `crates/wc-sketches/src/line/particle_stats.rs` (new)

```rust
//! Per-frame statistics over the CPU particle mirror.
//!
//! Direct port of v4's `src/particles/particleStats.ts::computeStats`. Reads
//! [`LineCpuMirror`] and writes [`ParticleStats`] each frame while the sketch
//! is Active. Plan 9's reactivity-coupling system reads ParticleStats and
//! drives synth + post-process uniforms.

use bevy::prelude::*;

use super::sim_cpu::LineCpuMirror;

/// Statistics over the current particle population. All values are
/// dimensionless or normalized except `average_vel` (px/s) and
/// `variance_length` (px).
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct ParticleStats {
    pub average_vel: f32,
    pub variance_length: f32,
    pub flat_ratio: f32,
    pub grouped_upness: f32,
    pub normalized_entropy: f32,
    pub normalized_variance_length: f32,
    pub normalized_average_vel: f32,
}

/// Update `ParticleStats` from the current `LineCpuMirror` state.
///
/// Implements:
/// - `average_vel = sqrt(mean(vx² + vy²))`
/// - `variance_length = sqrt(varianceX² + varianceY²)`
/// - `flat_ratio = varianceX / varianceY` (1.0 if varianceY ≈ 0)
/// - `grouped_upness = sqrt(average_vel / variance_length)`
/// - `entropy = sum(length × log(length)) / N` where length = √(dx² + dy²)
///   from particle to mean-position. Normalized by `(width × 1.383870349)`.
/// - `normalized_variance_length = variance_length / (0.28866 × width)`
/// - `normalized_average_vel = average_vel / width`
pub fn update_particle_stats(
    mirror: Res<'_, LineCpuMirror>,
    window: Single<'_, '_, &bevy::window::Window>,
    mut stats: ResMut<'_, ParticleStats>,
) {
    let n = mirror.particles.len();
    if n == 0 {
        *stats = ParticleStats::default();
        return;
    }
    let n_f = n as f32;

    // Pass 1: mean position, mean velocity².
    let mut avg_x = 0.0;
    let mut avg_y = 0.0;
    let mut avg_vel2 = 0.0;
    for p in &mirror.particles {
        avg_x += p.position[0];
        avg_y += p.position[1];
        avg_vel2 += p.velocity[0] * p.velocity[0] + p.velocity[1] * p.velocity[1];
    }
    avg_x /= n_f;
    avg_y /= n_f;
    avg_vel2 /= n_f;

    // Pass 2: variances + entropy.
    let mut var_x2 = 0.0;
    let mut var_y2 = 0.0;
    let mut entropy = 0.0;
    for p in &mirror.particles {
        let dx = p.position[0] - avg_x;
        let dy = p.position[1] - avg_y;
        let dx2 = dx * dx;
        let dy2 = dy * dy;
        var_x2 += dx2;
        var_y2 += dy2;
        let length = (dx2 + dy2).sqrt();
        if length > 0.0 {
            entropy += length * length.ln();
        }
    }
    entropy /= n_f;
    var_x2 /= n_f;
    var_y2 /= n_f;

    let variance_x = var_x2.sqrt();
    let variance_y = var_y2.sqrt();
    let variance_length = (var_x2 + var_y2).sqrt();
    let average_vel = avg_vel2.sqrt();

    let flat_ratio = if variance_y > 0.0 { variance_x / variance_y } else { 1.0 };
    let width = window.width().max(1.0);

    stats.average_vel = average_vel;
    stats.variance_length = variance_length;
    stats.flat_ratio = flat_ratio;
    stats.grouped_upness = if variance_length > 0.0 {
        (average_vel / variance_length).sqrt()
    } else {
        0.0
    };
    stats.normalized_entropy = entropy / (width * 1.383870349);
    stats.normalized_variance_length = variance_length / (0.28866 * width);
    stats.normalized_average_vel = average_vel / width;
}
```

### Task 17: Register + system

In `crates/wc-sketches/src/line/mod.rs`:
- `pub mod particle_stats;`
- `app.init_resource::<particle_stats::ParticleStats>();`
- Append `particle_stats::update_particle_stats` to the gated `Update` chain after `step_cpu_mirror`.

### Task 18: Unit tests

Add unit tests on `particle_stats.rs` that build a synthetic `LineCpuMirror` and assert the stats values for known configurations (all-zero velocities → grouped_upness=0; uniform spread → flat_ratio≈1; clustered → variance_length small).

### Task 19: Commit Phase C

---

# Phase D — Reactivity coupling

The closing-the-loop system.

### Task 20: `audio_coupling.rs`

**File:** `crates/wc-sketches/src/line/audio_coupling.rs` (new)

Implements one `Update` system that runs each frame while Line is Active:

```rust
pub fn drive_audio_and_shader(
    stats: Res<'_, super::particle_stats::ParticleStats>,
    time: Res<'_, Time>,
    audio_cmd: NonSendMut<'_, wc_core::audio::ring::AudioCommandSender>,
    mut post: ResMut<'_, super::post_process::LinePostParams>,
) {
    // --- Audio modulation (matches v4 LineSketch.step()) ---
    audio_cmd.try_send(AudioCommand::SetLineParam {
        key: "lfo_freq",
        value: stats.flat_ratio,
    });
    if stats.normalized_entropy != 0.0 {
        audio_cmd.try_send(AudioCommand::SetLineParam {
            key: "bandpass_freq",
            value: 222.0 / stats.normalized_entropy,
        });
    }
    audio_cmd.try_send(AudioCommand::SetLineParam {
        key: "noise_freq",
        value: 2000.0 * stats.normalized_variance_length,
    });
    audio_cmd.try_send(AudioCommand::SetLineParam {
        key: "volume",
        value: (stats.grouped_upness - 0.05).max(0.0) * 5.0,
    });

    // --- Shader modulation (v4 LineSketch.step()) ---
    let t = time.elapsed_secs();
    post.g_constant = triangle_wave_approx(t / 5.0) * (stats.grouped_upness + 0.5) * 15000.0;
    post.i_mouse_factor = (1.0 / 15.0) / (stats.grouped_upness + 1.0);
}

/// Approximate normalized triangle wave using first three odd harmonics
/// (v4 src/math.ts::triangleWaveApprox).
fn triangle_wave_approx(t: f32) -> f32 {
    use std::f32::consts::PI;
    (8.0 / (PI * PI)) * (t.sin() - (1.0 / 9.0) * (3.0 * t).sin() + (1.0 / 25.0) * (5.0 * t).sin())
}
```

> **Note on `t / 5.0`:** v4 uses `triangleWaveApprox(now / 5000)` where `now` is in **milliseconds**. `t = time.elapsed_secs()` is in **seconds**, so the equivalent divisor is `5.0` (5s period instead of 5000ms). Same numeric result.

### Task 21: Wire OnEnter/OnExit audio lifecycle

In `LinePlugin::build`:
- `OnEnter(AppState::Line)` → send `AudioCommand::AddLineSynth`
- `OnExit(AppState::Line)` → send `AudioCommand::RemoveLineSynth`

These join `spawn_line` / `remove_sim_params` in the existing `OnEnter` / `OnExit` schedules.

### Task 22: Commit Phase D

---

# Phase E — Tests, PARITY.md, push, tag

### Task 23: Integration test for audio lifecycle

Append to `crates/wc-sketches/tests/line_input.rs`:

```rust
#[test]
fn entering_line_sends_addlinesynth_command() {
    // Build the sketches test app, enter Line, drain the AudioCommandSender's
    // outgoing ring, assert that AddLineSynth appeared.
    // The audio thread may not be running in test mode (cpal::Stream is not
    // created without a real audio device); the sender ring is still
    // populated by sketch-side code, so the test reads pop'd commands.
}
```

(Exact body depends on whether `AudioCommandSender` is a `NonSend` resource accessible in tests; the implementer adapts.)

### Task 24: Update PARITY.md

Mark Plan 9 shipped; note audio + reactivity now match v4 except for...
- Heatmap spawn (Plan 10)
- Final sign-off (Plan 10)

### Task 25: Commit Phase E

### Task 26: Push, watch CI, tag `v5-line-audio`, update roadmap

---

## Self-review checklist

- [ ] `cargo run -p waveconductor` opens the sketch, audio plays (background + synth), particles respond, shader smear modulates with motion
- [ ] All tests pass
- [ ] fmt + clippy + doc all clean
- [ ] No new allocations in the audio thread's render path
- [ ] PARITY.md mentions Plan 9 shipped
- [ ] Tag `v5-line-audio`
- [ ] Roadmap updated

## Carry-forwards for Plan 10

*(populated during execution)*

## Execution handoff

Plan saved. Two execution options as usual: subagent-driven or inline. The subagent-driven path is recommended for a plan this size.
