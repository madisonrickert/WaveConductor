# Dots D4 — Audio voice + envelope coupling — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use `- [ ]` checkboxes.

**Goal:** Give Dots a synth voice — a faithful port of v4's `dots/audio.ts` (detuned triangle pair → lowpass→bandpass cascade + white noise + an LFO on both cutoffs) — driven each frame by an attack/release **activity envelope** (the spec's envelope-primary decision, not true GPU-readback field stats).

**Architecture:** A new `DotsSynth` fundsp voice in `wc-core/src/audio/` mirroring `LineSynth`'s structure (a `Box<dyn AudioUnit>` graph + `Shared` parameter handles), driven over the existing lock-free `AudioCommand` ring via new `AddDotsSynth`/`RemoveDotsSynth`/`SetDotsParam` variants the audio `engine` dispatches into a `DotsSynth` slot. A `dots/audio_coupling.rs` system computes a single CPU activity envelope from the mouse-attractor power (attack on press, release on let-go) and maps it to the synth's filter cutoff + volume each frame. The LFO rate (v4's `flatRatio`-driven field-shape term) cannot be synthesized from an envelope, so it stays at a fixed rate — the documented perceptual gap.

**Tech Stack:** Rust, fundsp (the DSP graph), the `wc-core::audio` engine + lock-free ring, Bevy `Update` systems.

## Global Constraints

- **The audio thread is real-time:** lock-free ring only, **no `Mutex`, no allocations after init** (the `DotsSynth` graph is built + `allocate()`d once on `AddDotsSynth`). Mirror `LineSynth`'s allocate-on-construct discipline.
- **v4 `dots/audio.ts` values, verbatim:** `BASE_FREQUENCY = 164.82`; osc1 = triangle at `detuned(BASE/2 = 82.41, 2 cents)` gain `0.3`; osc2 = triangle at `BASE = 164.82` gain `0.30`; LFO base `8.66 Hz`; lowpass + bandpass both `Q = 5.18`; `filterGain = 0.7`; noise gain = `volume × 0.05`; `setFrequency(freq)` sets BOTH filter cutoffs to `freq` and the LFO depth to `freq × 0.06`; `setVolume(v)` sets the source gain to `v` and noise gain to `v × 0.05`.
- **Envelope-primary (spec decision):** the production coupling drives the synth from an attractor-activity attack/release envelope, NOT a per-frame GPU readback of field stats. The `flatRatio` (LFO-rate) and `variance`-shape terms v4 used are not reproducible this way — that's an accepted perceptual gap (same trade Line's Plan 11 Phase F made). **Do NOT resurrect a per-frame CPU mirror or GPU reduction in production.**
- **Perceptual tuning is the operator's.** Build a faithful v4-structured graph with the v4 constants as the starting point; the actual *sound* (does it match v4, is it pleasant on the kiosk) is judged by ear by Madison via `cargo rund` — NOT auto-verifiable. Unit tests cover the param plumbing + the envelope math, not the audio.
- **No `unwrap()`/`expect()`** in non-test code unless documented; **no `as` casts** where `TryFrom` works; **`///`/`//!` docs**.
- **Verification gates:** fmt; clippy `--all-targets --all-features --workspace -D warnings`; nextest `--workspace --all-features` + `cargo test --doc`; `cargo doc`; `cargo xtask check-secrets`. Do NOT run `cargo rund` (interactive; sound is operator-verified).
- **Commit messages:** `git commit -F` (no backticks).

## Reference material (read these)

- v4: `/Users/madison/Developer/WaveConductor/.worktrees/v4/src/sketches/dots/audio.ts` (the graph) + `index.ts::step()` (the coupling: `lfo.frequency = flatRatio`, `setFrequency(120/normVarLen * avgVel/100)`, `setVolume(max(groupedUpness-0.05, 0))`).
- `crates/wc-core/src/audio/line_synth.rs` (the `Shared`-param + `Box<dyn AudioUnit>` voice pattern + `set_param` + `render`/process; the allocate-on-construct discipline). Build a SIMPLER graph than Line's — Dots has no evolution/drift/brown-noise; just the v4 cascade.
- `crates/wc-core/src/audio/command.rs` (the `AudioCommand` enum + `SetLineParam` shape) and `crates/wc-core/src/audio/engine.rs` (how `AddLineSynth`/`RemoveLineSynth`/`SetLineParam` are dispatched into the `LineSynth` slot; mirror for Dots).
- `crates/wc-sketches/src/line/audio_coupling.rs` + `crates/wc-sketches/src/line/leap_attractors.rs` `HandAudioDrive` (the envelope/drive pattern) and `crates/wc-sketches/src/line/mod.rs` `enter_line_audio`/`exit_line_audio` (the OnEnter/OnExit AudioCommand lifecycle).
- `crates/wc-sketches/src/dots/systems/mouse.rs` (`DotsMouseAttractorState.power` — the envelope's input).

---

### Task 1: `DotsSynth` voice + `AudioCommand` variants + engine dispatch (wc-core)

**Files:**
- Create: `crates/wc-core/src/audio/dots_synth.rs` (`DotsSynth`)
- Modify: `crates/wc-core/src/audio/mod.rs` (`pub mod dots_synth;`)
- Modify: `crates/wc-core/src/audio/command.rs` (add `AddDotsSynth`, `RemoveDotsSynth`, `SetDotsParam { key: &'static str, value: f32 }`)
- Modify: `crates/wc-core/src/audio/engine.rs` (a `DotsSynth` slot + dispatch the three commands, mirroring the `LineSynth` handling)
- Modify: `crates/wc-core/src/audio/state.rs` if `AudioMessage` echoes synth activation (mirror `LineSynthActivated`/`Deactivated` → `DotsSynthActivated`/`Deactivated`)

**Interfaces:**
- Produces: `wc_core::audio::dots_synth::DotsSynth` with `new()` (builds + allocates the graph), `set_param(key: &str, value: f32)`, and the `AudioUnit` render entry the engine calls. Param keys: `"bandpass_freq"` (sets both filter cutoffs), `"lfo_depth"` (the `freq × 0.06` LFO depth), `"volume"` (source gain; noise gain = `volume × 0.05`). `AudioCommand::{AddDotsSynth, RemoveDotsSynth, SetDotsParam { key, value }}`.

- [ ] **Step 1: Build the `DotsSynth` graph (`dots_synth.rs`)**

Mirror `line_synth.rs`'s skeleton (a struct owning `graph: Box<dyn AudioUnit>` + `Shared` handles, `new()` builds the fundsp graph and calls `allocate()`, `set_param` writes the `Shared`s, a render method the engine drives). Build the v4 graph with fundsp primitives: two `triangle` oscillators (osc1 at `82.41 Hz` detuned +2 cents, osc2 at `164.82 Hz`), summed and scaled by a `volume` `Shared`; through a `lowpass` (Q 5.18) then a `bandpass` (Q 5.18) whose center cutoffs are both driven by a `bandpass_freq` `Shared` plus an LFO term (`sine(8.66) × lfo_depth` `Shared`); a `noise` source scaled by `volume × 0.05`; mixed with the filtered voice (`filterGain 0.7`) to the output. Use the same clamp-cutoff-above-zero guard `LineSynth` uses (SVF filters need a strictly positive cutoff). Default the `Shared`s to v4's initial state (cutoffs clamped just above 0, volume 0, lfo_depth 0). Keep the graph SIMPLE — no evolution/drift modulators (those are Line-only enhancements).

- [ ] **Step 2: Add the `AudioCommand` variants + engine dispatch**

In `command.rs`: add `AddDotsSynth`, `RemoveDotsSynth`, `SetDotsParam { key: &'static str, value: f32 }` (mirror the Line variants exactly, including the `&'static str` note for keeping the enum `Copy`). In `engine.rs`: add a `dots_synth: Option<DotsSynth>` slot (alongside the Line one), handle `AddDotsSynth` (build the voice — idempotent if already present), `RemoveDotsSynth` (drop it — idempotent), `SetDotsParam` (forward to `dots_synth.set_param(key, value)` if present); in the per-sample render mix, sum the Dots voice when present (mirror exactly how the Line voice is summed). Echo activation via `AudioMessage::DotsSynthActivated`/`Deactivated` if Line does the equivalent. Only ONE sketch synth is active at a time in practice, but keep the slots independent (don't share a slot — that's a refactor; mirror Line).

- [ ] **Step 3: Tests**

In `dots_synth.rs` tests: `DotsSynth::new()` builds without panic; `set_param` on each key updates the corresponding `Shared` (read it back); rendering N samples after `set_param("volume", 0.0)` produces ~silence and after a positive volume produces non-zero output (mirror any `LineSynth` render test in `dsp_tests.rs`). In `command.rs`/`engine.rs` tests: an `AddDotsSynth` then `SetDotsParam` then `RemoveDotsSynth` sequence leaves the engine in the expected state (mirror the Line command test if one exists). Keep allocation off the render path (the construction allocates; `set_param`/render must not) — note it in the report.

- [ ] **Step 4: Build, test, gates**

```bash
cargo build -p wc-core --all-features
cargo nextest run -p wc-core --all-features
cargo clippy -p wc-core --all-targets --all-features -- -D warnings
```

Expected: PASS. (The SOUND is operator-verified — not run here.)

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -F <(printf '%s\n' "feat(audio): DotsSynth voice + AddDotsSynth/SetDotsParam commands + engine dispatch" "" "Faithful v4 dots/audio.ts cascade (detuned triangle pair -> lowpass -> bandpass + noise + 8.66Hz LFO). Simpler than LineSynth (no evolution/drift); operator tunes by ear." "" "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>")
```

---

### Task 2: Dots activity envelope + audio coupling + enter/exit lifecycle (wc-sketches)

**Files:**
- Create: `crates/wc-sketches/src/dots/audio_coupling.rs` (the envelope + per-frame `SetDotsParam` driver)
- Modify: `crates/wc-sketches/src/dots/mod.rs` (`enter_dots_audio`/`exit_dots_audio` on OnEnter/OnExit; register the coupling in the `Update` chain)
- Modify: `crates/wc-sketches/src/dots/systems/mod.rs` if the envelope state lives there

**Interfaces:**
- Produces: a `DotsAudioDrive` (or `Local`-held) attack/release envelope resource + `drive_dots_audio` (`Update`, `run_if(sketch_active(AppState::Dots))`); `enter_dots_audio`/`exit_dots_audio`.
- Consumes: `DotsMouseAttractorState.power`, the `AudioCommandSender` ring, `Time`.

- [ ] **Step 1: Activity envelope**

Add an attack/release envelope (a resource or `Local<f32>`) that rises toward 1.0 while the attractor is active (`DotsMouseAttractorState.power > 0`) and decays toward 0.0 when not — mirror the smoothed-envelope approach Line uses for `grouped_upness`/`HandAudioDrive` (read `line/audio_coupling.rs` + `leap_attractors.rs`). Use a simple per-frame exponential ease with attack/release time constants (start with values mirroring Line's defaults; the operator tunes). Allocation-free (scalar state).

- [ ] **Step 2: `drive_dots_audio`**

Each frame, push `SetDotsParam` commands from the envelope (mirror `line/audio_coupling.rs`'s ring-push pattern + the ring-full `warn`-once handling):
- `volume = activity_envelope` (clamped `[0,1]`, optionally `× synth_volume_scale` if Dots adds that knob later — not required now).
- `bandpass_freq` = map the envelope to a cutoff (e.g. a base + `envelope ×` range, approximating v4's `120/normVarLen * avgVel/100` band — pick a musically sensible range since the true stat is unavailable; document it as an envelope approximation and a tuning target).
- `lfo_depth` = `bandpass_freq × 0.06` (v4's relation).
- Leave the LFO RATE fixed at the v4 base (8.66 Hz) — the `flatRatio`-driven rate is the un-synthesizable term; document the gap.
Push these only while `sketch_active(Dots)`.

- [ ] **Step 3: Enter/exit audio lifecycle (`dots/mod.rs`)**

Mirror `enter_line_audio`/`exit_line_audio`: `OnEnter(AppState::Dots)` pushes `AddDotsSynth` (and any background restore if Dots has one — v4 Dots has NO background OGG, so skip the background command); `OnExit(AppState::Dots)` pushes `RemoveDotsSynth`. Handle the absent `AudioCommandSender` (headless tests) by early-returning cleanly, exactly as Line does. Register `drive_dots_audio` in the `Update` chain `.run_if(sketch_active(AppState::Dots))`.

- [ ] **Step 4: Tests**

In `audio_coupling.rs` tests: the envelope rises when `power > 0` and decays when `power == 0` across simulated frames (assert direction + bounds `[0,1]`); `drive_dots_audio` with a known envelope value and a captured `AudioCommandSender` (or a test double) pushes the expected `SetDotsParam` keys/values (assert the `volume`/`bandpass_freq`/`lfo_depth` relations, especially `lfo_depth == bandpass_freq × 0.06`). Mirror how Line tests its coupling (read `crates/wc-sketches/tests/` for an audio-ring test harness; if the ring isn't easily mockable, test the envelope math as a pure function and assert the param-derivation formulas directly).

- [ ] **Step 5: Build, test, gates**

```bash
cargo build -p wc-sketches --all-features
cargo nextest run --workspace --all-features
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo fmt --all -- --check
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -F <(printf '%s\n' "feat(dots): envelope-driven audio coupling + enter/exit synth lifecycle" "" "Attack/release activity envelope from attractor power drives DotsSynth volume + filter cutoff. LFO rate fixed (flatRatio gap, per spec). Operator tunes by ear." "" "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>")
```

---

## Self-Review

**Spec coverage** (against §"Audio" + D4 row):
- DotsSynth voice (v4 cascade: triangle pair → lowpass→bandpass + noise + LFO) → Task 1. ✓
- AudioCommands + engine dispatch → Task 1. ✓
- Envelope-primary coupling (no GPU readback / CPU mirror in production) → Task 2, explicit. ✓
- Enter/exit audio lifecycle → Task 2 Step 3. ✓
- `flatRatio` gap documented → Task 2 Steps 2 + the constraints. ✓
- Perceptual tuning = operator → constraints + every "tune by ear" note. ✓

**Placeholder scan:** No TBD-as-deliverable. The synth is ported from named v4 source with exact constants; the engine dispatch mirrors named Line code. The one inherently-judgment item — the exact envelope→cutoff mapping range (since the true `variance` stat is unavailable) — is specified as "a musically sensible range, documented as a tuning target," which is the honest state for an operator-tuned approximation, with the testable relations (`lfo_depth = bandpass_freq × 0.06`, volume = envelope) pinned by tests.

**Type consistency:** `DotsSynth`, `AudioCommand::{AddDotsSynth, RemoveDotsSynth, SetDotsParam}`, param keys `"bandpass_freq"`/`"lfo_depth"`/`"volume"`, `drive_dots_audio`/`enter_dots_audio`/`exit_dots_audio` — consistent across both tasks.

**Risks:** (1) The fundsp graph is intricate; the SOUND is operator-verified, so the tests guard plumbing/no-panic/param-routing only. (2) Real-time-thread discipline (no alloc/Mutex after init) is the key correctness constraint — pinned by mirroring Line's allocate-on-construct and tested by the "set_param/render don't allocate" reasoning. (3) Envelope approximation is a known perceptual gap (accepted by the spec).
