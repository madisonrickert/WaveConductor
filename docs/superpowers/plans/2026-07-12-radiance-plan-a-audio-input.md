# Radiance Plan A: Audio Input + Analysis — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A request-driven cpal audio *input* stream whose real-time-clean callback feeds a per-frame analysis system publishing `Res<AudioAnalysis>` (RMS, AGC gain, 8 log-spaced bands, spectral-flux onset, debounced beat), with the input-device list registered as the `"audio_input_devices"` runtime enum — Unit A of the Radiance spec (`docs/superpowers/specs/2026-07-11-radiance-dancer-aura-sketch-design.md`).

**Architecture:** A second cpal stream mirroring the output engine (`audio/engine.rs`) in reverse: the input callback downmixes to mono and pushes into an `rtrb` ring (no alloc, no lock, no log; errors flip an `Arc<AtomicBool>`); a `PreUpdate` exclusive *capture driver* reacts to the pinned `AudioCaptureRequest` resource (insert = start, remove = stop, `paused` = pause; device change = rebuild, failure = neutral analysis + cooldown retry); a chained `PreUpdate` system drains the ring into a pre-allocated `AnalysisEngine` (circular history, Hann window, `fundsp::fft::real_fft`, asymmetric AGC, band smoothing, spectral flux, beat debounce) — all buffers allocated at stream build, zero steady-state allocation. The engine is a pure, device-free struct so every analysis behavior is unit-testable headlessly.

**Tech Stack:** `cpal` 0.16 (input stream + device enumeration), `fundsp` 0.23 (`fundsp::fft::real_fft` — ungated, microfft-backed; the cargo `fft` feature we disable only gates `convolve`), `rtrb` 0.3, Bevy 0.19, existing `settings::runtime_enum` registry. **NO new dependencies.**

## Global Constraints

- `cargo fmt --all -- --check` passes (rustfmt.toml nightly warnings on stable are expected and harmless).
- `cargo clippy --all-targets --all-features --workspace -- -D warnings` passes — lints are hard errors, pedantic included, test code included.
- `cargo nextest run --workspace --all-features` passes (CI's runner; nextest skips doctests).
- `cargo test --doc --workspace` passes (covers doctests nextest skips).
- `cargo doc --no-deps --workspace --document-private-items` builds clean (CI sets `RUSTDOCFLAGS="-D warnings"`; **no `--all-features`** — never intra-doc-link to a feature-gated or lower-visibility item, demote to a plain code span).
- `cargo deny check` passes (no new crates, so this cannot newly fail; run it anyway).
- `cargo xtask check-secrets` passes (no home-directory paths, emails, or secret prefixes anywhere, comments included).
- **No allocation in hot paths:** the cpal input callback and every per-frame system. Pre-allocate at init; `fill`/`copy_from_slice`/index writes only in steady state. Event-frequency allocation (stream build/teardown, device enumeration, error formatting) is fine and documented inline.
- No `unwrap()` / `expect()` in non-test code unless the panic is a documented invariant violation; test modules take the house `#[allow(clippy::expect_used, ...)]` block with a `reason`.
- No `as` numeric casts where `From` / `TryFrom` / `u32::try_from` works; DSP init-time casts use the house targeted-`#[allow]`-with-reason pattern (see `audio/sample_bank.rs::mix_frame`).
- `///` rustdoc on every public item; `//!` module docs on every module root describing role, data flow, and which thread each piece runs on.
- Public API at the top of each file, private helpers below, `#[cfg(test)] mod tests` at the footer.
- Commit messages contain **NO backticks** and are passed with plain `git commit -m` (one `-m` for the subject, a second `-m` for the trailer). End every commit message with the line: Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
- Stage named paths only; never `git add -A`. Confirm with `git show --stat HEAD` after committing.
---

## File Structure

| File | Action | Responsibility |
| --- | --- | --- |
| `crates/wc-core/src/audio/input/mod.rs` | Create | Module docs (data-flow diagram), pinned resources `AudioAnalysis` + `AudioCaptureRequest`, `AudioInputPlugin`, debug-only `WC_AUDIO_INPUT_SMOKE` harness |
| `crates/wc-core/src/audio/input/analysis.rs` | Create | `Agc`, `AnalysisEngine` (circular history, FFT bands, flux/beat), `AnalysisState` resource, `drain_and_analyze` system, all tuning consts |
| `crates/wc-core/src/audio/input/analysis_tests.rs` | Create | Sibling test file for `analysis.rs` (the `dsp.rs`/`dsp_tests.rs` `#[path]` idiom) |
| `crates/wc-core/src/audio/input/capture.rs` | Create | `AudioInputRing`, `AudioInputErrorFlag`, `AudioInputStatus`, `CaptureRuntime`, cpal stream build (`AudioInputStream`), pure `decide()` + `drive_capture` exclusive system |
| `crates/wc-core/src/audio/input/devices.rs` | Create | Pinned `AvailableAudioInputDevices`, `RuntimeEnumOptionsSource` impl (`OPTIONS_KEY = "audio_input_devices"`), enumeration systems |
| `crates/wc-core/src/audio/mod.rs` | Modify | `pub mod input;` declaration + `AudioInputPlugin` added inside `AudioPlugin::build` (the "core audio plumbing" pinned in the contracts) |

## Execution notes

- **Builds are deferred to execution time.** This plan was written without running cargo (disk pressure). Wiring-only tasks verify with `cargo check -p wc-core`; every other task verifies with its named nextest filter.
- **Tests must never insert `AudioCaptureRequest` into an app that has `drive_capture` registered** — that would open a real OS input stream (a live mic) on the test machine. Drain-system tests build the ring/engine resources by hand and register only `drain_and_analyze`. The plugin-level test updates the app *without* a request, which exercises the cheap no-op path only.
- **Pinned contracts** (`radiance-shared-contracts.md`, 2026-07-12): `AudioAnalysis` (+ this plan adds a `peak` field — additive, allowed), `AudioCaptureRequest`, `AvailableAudioInputDevices`, `OPTIONS_KEY = "audio_input_devices"`, module path `crates/wc-core/src/audio/input/`. Never rename or retype any of these.
- **Failure posture (from the spec, deliberate):** a missing or failed device — including a *named* device that is not currently present — yields neutral analysis and an `AudioInputStatus::Errored` diagnostic, with a 2 s retry cooldown. There is **no silent fallback to the default device** when a named device is absent; the operator chose it, and it may reappear (kiosk TV/interface waking up).
- These systems run in every `AppState`. That is sanctioned core-plumbing behavior (same class as the settings-reload listeners in AGENTS.md): with no `AudioCaptureRequest` present, `drive_capture` and `drain_and_analyze` early-out after a couple of resource existence checks. Plan C inserts/removes the request on Radiance enter/exit and drives `paused` from `SketchActivity`.

---

### Task 1: Module scaffold — pinned resources + plugin skeleton + core wiring

**Files:**
- Create: `crates/wc-core/src/audio/input/mod.rs`
- Modify: `crates/wc-core/src/audio/mod.rs` (module list after line 52 `pub mod background;` block; plugin registration inside `AudioPlugin::build`, currently lines 84–104)
- Test: `#[cfg(test)] mod tests` at the footer of `crates/wc-core/src/audio/input/mod.rs`

**Interfaces:**
- Consumes: `bevy::prelude::*`; `crate::audio::AudioPlugin` (registration point).
- Produces: `AudioAnalysis` (pinned + `peak` field + `neutral()` + `Default`), `AudioCaptureRequest` (pinned), `AudioInputPlugin` — every later task hangs off these exact names.

- [ ] **Step 1: Write the failing test**

Create `crates/wc-core/src/audio/input/mod.rs` with module docs, the two pinned resources, a plugin that (for now) only initializes `AudioAnalysis`, and the tests. Writing type + test together is unavoidable for a new module; the test is still written to fail first because the module is not yet declared in `audio/mod.rs` (Step 2 confirms the compile failure), then wiring makes it pass.

```rust
//! Audio *input* capture and analysis (Radiance Unit A).
//!
//! The output engine's architecture (`super::engine`) run in reverse:
//!
//! ```text
//!   ┌─────────────────────────────────┐      ┌───────────────────────────┐
//!   │ Bevy main thread (per frame)    │      │ cpal input thread (kHz)   │
//!   │                                 │      │                           │
//!   │  Plan C inserts/removes         │      │  input callback           │
//!   │   Res<AudioCaptureRequest> ─────┼──┐   │   downmix to mono         │
//!   │                                 │  │   │   push f32 ──▶ rtrb ring  │
//!   │  PreUpdate: drive_capture ◀─────┼──┘   │   errors ──▶ AtomicBool   │
//!   │   (build/pause/teardown stream) │      └───────────────────────────┘
//!   │  PreUpdate: drain_and_analyze   │                  │
//!   │   ring ─▶ AnalysisEngine ─▶ Res<AudioAnalysis> ◀───┘
//!   └─────────────────────────────────┘
//! ```
//!
//! ## Activation contract (pinned across the Radiance plans)
//!
//! Inserting [`AudioCaptureRequest`] starts capture; removing it stops
//! capture. `paused: true` pauses the cpal stream and holds
//! [`AudioAnalysis`] at [`AudioAnalysis::neutral`]. The request is
//! sketch-agnostic: Plan C inserts it on entering Radiance and removes it on
//! exit; nothing in this module names a sketch.
//!
//! ## Failure posture
//!
//! Missing/failed/vanished device: [`AudioAnalysis`] holds neutral values,
//! the failure surfaces in `capture::AudioInputStatus` (diagnostics), and the
//! capture driver retries on a cooldown. Never panics, never blocks, never
//! silently falls back to a different device than the one requested.
//!
//! ## Always-on cost
//!
//! These systems are registered unconditionally (core plumbing, like the
//! settings-reload listeners): with no request present they no-op after a
//! couple of resource-existence checks per frame.

pub mod analysis;
pub mod capture;
pub mod devices;

use bevy::prelude::*;

/// Number of log-spaced spectral bands published in [`AudioAnalysis::bands`].
pub const AUDIO_BAND_COUNT: usize = 8;

/// Main-thread snapshot of the live audio-input analysis.
///
/// Always present once [`AudioInputPlugin`] is added; holds
/// [`AudioAnalysis::neutral`] whenever capture is inactive, paused, or
/// failed. Updated each `PreUpdate` by `analysis::drain_and_analyze`.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct AudioAnalysis {
    /// Post-AGC smoothed level, approximately `0..1`.
    pub rms: f32,
    /// Current AGC gain multiplier (`1.0` when neutral).
    pub gain: f32,
    /// Log-spaced band energies, post-AGC, approximately `0..1`.
    /// Band edges are `analysis::BAND_EDGES_HZ` (50 Hz – 12.8 kHz, octave
    /// spaced).
    pub bands: [f32; AUDIO_BAND_COUNT],
    /// Spectral-flux onset strength this frame, `>= 0`. Normalized against a
    /// slow running mean of flux, so ~1 is "typical activity" and spikes of
    /// 2–3+ indicate an onset.
    pub onset: f32,
    /// Debounced beat estimate, `0..1`: snaps to 1 on a detected beat and
    /// decays exponentially between beats.
    pub beat_confidence: f32,
    /// Post-AGC decaying peak-hold level, approximately `0..1`. Additive
    /// field beyond the pinned contract (spec computes RMS *and* peak).
    pub peak: f32,
    /// Capture stream is healthy and producing samples.
    pub active: bool,
}

impl AudioAnalysis {
    /// The inactive/failed/paused value: zeros, unity gain, not active.
    pub const fn neutral() -> Self {
        Self {
            rms: 0.0,
            gain: 1.0,
            bands: [0.0; AUDIO_BAND_COUNT],
            onset: 0.0,
            beat_confidence: 0.0,
            peak: 0.0,
            active: false,
        }
    }
}

impl Default for AudioAnalysis {
    fn default() -> Self {
        Self::neutral()
    }
}

/// Activation contract: INSERT this resource to start capture; REMOVE it to
/// stop. Sketch-agnostic — Plan C inserts it `OnEnter(AppState::Radiance)`
/// and removes it `OnExit`.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct AudioCaptureRequest {
    /// Which input device to capture from. `None` = system default input
    /// device. Names come from `devices::AvailableAudioInputDevices`.
    pub device_name: Option<String>,
    /// `true` during Idle/Screensaver: the cpal stream is paused and
    /// [`AudioAnalysis`] holds neutral values (attract mode is not
    /// audio-reactive).
    pub paused: bool,
}

/// Wires audio-input capture + analysis into the app.
///
/// Registered by [`super::AudioPlugin`] (core audio plumbing — never by a
/// sketch). Publishes [`AudioAnalysis`] and the
/// `devices::AvailableAudioInputDevices` runtime-enum source; reacts to
/// [`AudioCaptureRequest`] insert/remove/change every frame.
pub struct AudioInputPlugin;

impl Plugin for AudioInputPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AudioAnalysis>();
        // Later tasks extend this: devices registry (Task 7), capture driver
        // + analysis systems (Tasks 6/8), smoke harness (Task 8).
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions; expect_used is denied workspace-wide for non-test code"
)]
mod tests {
    use super::*;

    #[test]
    fn neutral_analysis_is_zeroed_with_unity_gain_and_inactive() {
        let neutral = AudioAnalysis::neutral();
        assert!((neutral.rms - 0.0).abs() < f32::EPSILON);
        assert!((neutral.gain - 1.0).abs() < f32::EPSILON);
        assert!(neutral.bands.iter().all(|b| b.abs() < f32::EPSILON));
        assert!((neutral.onset - 0.0).abs() < f32::EPSILON);
        assert!((neutral.beat_confidence - 0.0).abs() < f32::EPSILON);
        assert!((neutral.peak - 0.0).abs() < f32::EPSILON);
        assert!(!neutral.active);
        assert_eq!(AudioAnalysis::default(), neutral);
    }

    #[test]
    fn plugin_installs_neutral_analysis_resource() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(AudioInputPlugin);
        app.update();
        let analysis = app.world().resource::<AudioAnalysis>();
        assert_eq!(*analysis, AudioAnalysis::neutral());
    }
}
```

Note: `pub mod analysis; pub mod capture; pub mod devices;` will not compile until those files exist. For THIS task only, create the three files as doc-comment-only stubs so the module tree compiles:

`crates/wc-core/src/audio/input/analysis.rs`:
```rust
//! Ring drain + DSP analysis for the audio-input path. Populated in Tasks
//! 2–6 (AGC, `AnalysisEngine`, `drain_and_analyze`).
```

`crates/wc-core/src/audio/input/capture.rs`:
```rust
//! cpal input-stream lifecycle for the audio-input path. Populated in Tasks
//! 6 (ring/flag/status types) and 8 (stream build + capture driver).
```

`crates/wc-core/src/audio/input/devices.rs`:
```rust
//! Input-device enumeration + runtime-enum registration. Populated in Task 7.
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p wc-core audio::input`
Expected: FAIL to compile — `crates/wc-core/src/audio/mod.rs` has no `pub mod input;`, so `audio::input` does not exist yet.

- [ ] **Step 3: Write minimal implementation (wire the module into `audio/mod.rs`)**

In `crates/wc-core/src/audio/mod.rs`:

(a) Add to the module list (alphabetical — between `pub mod flame_synth;` and `pub mod line_synth;`):

```rust
pub mod input;
```

(b) In `AudioPlugin::build`, insert a statement before the existing `app` method chain. The body currently begins with `app` followed by the `// AudioState is always present ...` comment and `.init_resource::<AudioState>()`; insert these four lines immediately above that `app`, leaving the entire existing chain untouched (`add_plugins` is a separate statement so the existing chain does not reflow):

```rust
        // Audio *input* capture + analysis (Radiance Unit A). Registered here
        // so the input path is core audio plumbing, present in every app that
        // has audio output — sketches only insert/remove AudioCaptureRequest.
        app.add_plugins(input::AudioInputPlugin);
```

(c) Extend the `//!` module docs of `audio/mod.rs`: after the existing "## What systems consume" section, add one bullet:

```rust
//! - [`input::AudioAnalysis`] (`Res<…>`) — live audio-*input* analysis
//!   (RMS/bands/onset) for audio-reactive sketches; neutral unless a sketch
//!   has inserted [`input::AudioCaptureRequest`]. See the `input` module.
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p wc-core audio::input`
Expected: PASS (2 tests). Also run `cargo check -p wc-core` to confirm the stub files compile under `missing_docs`.

- [ ] **Step 5: Commit**

```
git add crates/wc-core/src/audio/input/mod.rs crates/wc-core/src/audio/input/analysis.rs crates/wc-core/src/audio/input/capture.rs crates/wc-core/src/audio/input/devices.rs crates/wc-core/src/audio/mod.rs
git commit -m "feat(audio): scaffold audio-input module with pinned AudioAnalysis and AudioCaptureRequest resources" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: AGC — asymmetric automatic gain control

**Files:**
- Modify: `crates/wc-core/src/audio/input/analysis.rs` (replace the Task 1 stub)
- Create: `crates/wc-core/src/audio/input/analysis_tests.rs`

**Interfaces:**
- Consumes: nothing beyond `std`.
- Produces: `pub struct Agc` with `new()`, `process(&mut self, raw_rms: f32, dt: f32) -> f32`, `gain(&self) -> f32`, `reset(&mut self)`; consts `AGC_TARGET_RMS`, `AGC_ATTACK_TAU_S`, `AGC_RELEASE_TAU_S`, `AGC_MIN_GAIN`, `AGC_MAX_GAIN`, `AGC_ENVELOPE_FLOOR`; helper `one_pole_coeff(dt, tau)`. Task 3's `AnalysisEngine` embeds `Agc`.

- [ ] **Step 1: Write the failing test**

Create `crates/wc-core/src/audio/input/analysis_tests.rs`:

```rust
//! Unit tests for [`super::Agc`] and [`super::AnalysisEngine`].
//!
//! Lives in a sibling file (linked from `analysis.rs` via
//! `#[path = ...] mod tests;`) so the production module stays under the
//! AGENTS.md ~300-line guideline — the same idiom as `audio/dsp_tests.rs`.
//! Everything here runs headlessly: the engine is a pure struct fed
//! synthesized samples; no audio device is ever opened.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "expect and panic are appropriate in test code"
)]
#![allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "tests synthesize waveforms from small integer sample indices, exact in f32"
)]

use super::*;

/// Simulated frame period: 60 Hz drain, matching the render loop.
const DT: f32 = 1.0 / 60.0;

/// Drive the AGC with a constant raw RMS for `seconds` of simulated time and
/// return the final gain.
fn run_agc(agc: &mut Agc, raw_rms: f32, seconds: f32) -> f32 {
    let steps = (seconds / DT) as usize;
    let mut gain = agc.gain();
    for _ in 0..steps {
        gain = agc.process(raw_rms, DT);
    }
    gain
}

#[test]
fn agc_converges_on_a_quiet_step_input() {
    // Room mic scenario: a steady signal 10x below target. After 30 s the
    // release-side AGC must have brought the post-gain level to target.
    let mut agc = Agc::new();
    // Settle at a loud level first so the quiet step exercises release.
    run_agc(&mut agc, AGC_TARGET_RMS, 5.0);
    let gain = run_agc(&mut agc, 0.025, 30.0);
    let post = 0.025 * gain;
    assert!(
        (post - AGC_TARGET_RMS).abs() < 0.02,
        "post-AGC level {post} should be within 0.02 of target {AGC_TARGET_RMS}"
    );
}

#[test]
fn agc_converges_on_a_loud_step_input() {
    // Loud step: attack side is fast — within 2 s the post-gain level is at
    // target.
    let mut agc = Agc::new();
    run_agc(&mut agc, 0.025, 30.0);
    let gain = run_agc(&mut agc, 0.5, 2.0);
    let post = 0.5 * gain;
    assert!(
        (post - AGC_TARGET_RMS).abs() < 0.02,
        "post-AGC level {post} should be within 0.02 of target {AGC_TARGET_RMS}"
    );
}

#[test]
fn agc_attack_is_faster_than_release() {
    // The same 2 s that converges the loud step must leave the quiet step
    // still far from target: gain rises slowly (release), falls fast (attack).
    let mut loud = Agc::new();
    run_agc(&mut loud, 0.025, 30.0);
    let post_loud = 0.5 * run_agc(&mut loud, 0.5, 2.0);

    let mut quiet = Agc::new();
    run_agc(&mut quiet, 0.5, 5.0);
    let post_quiet = 0.025 * run_agc(&mut quiet, 0.025, 2.0);

    assert!((post_loud - AGC_TARGET_RMS).abs() < 0.02);
    assert!(
        (post_quiet - AGC_TARGET_RMS).abs() > 0.05,
        "release must still be converging after 2 s (got {post_quiet})"
    );
}

#[test]
fn agc_gain_is_clamped_and_silence_does_not_blow_up() {
    let mut agc = Agc::new();
    let gain = run_agc(&mut agc, 0.0, 60.0);
    assert!(gain <= AGC_MAX_GAIN);
    let mut agc = Agc::new();
    let gain = run_agc(&mut agc, 10.0, 60.0);
    assert!(gain >= AGC_MIN_GAIN);
}

#[test]
fn agc_reset_restores_unity_gain() {
    let mut agc = Agc::new();
    run_agc(&mut agc, 0.01, 30.0);
    assert!(agc.gain() > 1.0);
    agc.reset();
    assert!((agc.gain() - 1.0).abs() < f32::EPSILON);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p wc-core audio::input::analysis`
Expected: FAIL to compile — `Agc` and the `AGC_*` consts do not exist, and `analysis.rs` does not link the test file.

- [ ] **Step 3: Write minimal implementation**

Replace `crates/wc-core/src/audio/input/analysis.rs` with:

```rust
//! Ring drain + DSP analysis for the audio-input path.
//!
//! Everything in this module runs on the **Bevy main thread**, once per
//! frame (`PreUpdate`), downstream of the lock-free ring the cpal input
//! callback fills (see `super::capture`). The core is [`AnalysisEngine`]
//! (Tasks 3–5): a pure, device-free struct — construct, push synthesized
//! samples, call `analyze`, assert — mirroring how `audio::dsp::DspHost` is
//! tested without hardware.
//!
//! ## Real-time / hot-path invariants
//!
//! Construction (`AnalysisEngine::new`, at stream build) allocates every
//! buffer once; `analyze` and `push` never allocate. This system runs every
//! frame for the life of the session, so per-iteration allocation here is a
//! thermal/jitter regression (AGENTS.md hot-path rule).

// ---------------------------------------------------------------------------
// AGC
// ---------------------------------------------------------------------------

/// Post-AGC level the gain controller steers the windowed RMS toward.
/// Chosen so typical program material sits mid-scale in the `~0..1` outputs.
pub const AGC_TARGET_RMS: f32 = 0.25;
/// Envelope time constant when the level is **rising** (gain falling). Fast,
/// so a sudden loud source cannot pin the bands at clip for long.
pub const AGC_ATTACK_TAU_S: f32 = 0.4;
/// Envelope time constant when the level is **falling** (gain rising). Slow,
/// so gaps between songs do not pump the room-noise floor up to full scale.
pub const AGC_RELEASE_TAU_S: f32 = 4.0;
/// Lower gain clamp (a very hot line-in is attenuated at most 2x).
pub const AGC_MIN_GAIN: f32 = 0.5;
/// Upper gain clamp (a quiet room mic is boosted at most 64x, bounding how
/// far silence-noise can be amplified).
pub const AGC_MAX_GAIN: f32 = 64.0;
/// Envelope floor: below this the input is treated as silent rather than
/// dividing by ~0 (which would slam the gain to the clamp instantly).
pub const AGC_ENVELOPE_FLOOR: f32 = 1.0e-4;

/// Slow, attack/release-asymmetric automatic gain control.
///
/// Tracks an envelope of the raw windowed RMS with asymmetric time constants
/// and derives `gain = target / envelope` (clamped). "Mic is the
/// analysis-quality bar" (spec): a room mic's absolute level is arbitrary,
/// so every downstream feature (bands, flux) consumes post-AGC signal.
#[derive(Debug, Clone)]
pub struct Agc {
    /// Smoothed raw-RMS envelope the gain is derived from.
    envelope: f32,
    /// Current gain multiplier, updated by [`Agc::process`].
    gain: f32,
}

impl Agc {
    /// A neutral controller: zero envelope, unity gain.
    pub fn new() -> Self {
        Self {
            envelope: 0.0,
            gain: 1.0,
        }
    }

    /// Advance the envelope by `dt` seconds toward `raw_rms` and return the
    /// updated gain. Asymmetric: rising levels use the fast attack constant,
    /// falling levels the slow release constant.
    pub fn process(&mut self, raw_rms: f32, dt: f32) -> f32 {
        let tau = if raw_rms > self.envelope {
            AGC_ATTACK_TAU_S
        } else {
            AGC_RELEASE_TAU_S
        };
        self.envelope += (raw_rms - self.envelope) * one_pole_coeff(dt, tau);
        self.gain = (AGC_TARGET_RMS / self.envelope.max(AGC_ENVELOPE_FLOOR))
            .clamp(AGC_MIN_GAIN, AGC_MAX_GAIN);
        self.gain
    }

    /// The gain computed by the most recent [`Agc::process`] call (`1.0`
    /// before the first).
    pub fn gain(&self) -> f32 {
        self.gain
    }

    /// Return to the neutral state (zero envelope, unity gain).
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

impl Default for Agc {
    fn default() -> Self {
        Self::new()
    }
}

/// One-pole smoothing coefficient for a step of `dt` seconds toward a target
/// with time constant `tau`: `1 - exp(-dt / tau)`. `dt == 0` yields `0`
/// (no movement), so a zero-delta first frame is harmless.
fn one_pole_coeff(dt: f32, tau: f32) -> f32 {
    1.0 - (-dt / tau).exp()
}

#[cfg(test)]
#[path = "analysis_tests.rs"]
mod tests;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p wc-core audio::input::analysis`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```
git add crates/wc-core/src/audio/input/analysis.rs crates/wc-core/src/audio/input/analysis_tests.rs
git commit -m "feat(audio): asymmetric AGC for room-mic input normalization" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: AnalysisEngine — circular history, RMS, peak, liveness

**Files:**
- Modify: `crates/wc-core/src/audio/input/analysis.rs` (append below the `Agc` section, above the `#[cfg(test)]` footer)
- Test: `crates/wc-core/src/audio/input/analysis_tests.rs` (append)

**Interfaces:**
- Consumes: `Agc`, `one_pole_coeff` (Task 2); `super::{AudioAnalysis, AUDIO_BAND_COUNT}` (Task 1).
- Produces: `pub struct AnalysisEngine` with `new(sample_rate: u32)`, `push(&mut self, sample: f32)`, `analyze(&mut self, dt: f32) -> AudioAnalysis`, `reset(&mut self)`, `samples_received() -> u64`, `last_raw_rms() -> f32`; consts `FFT_SIZE`, `HISTORY_LEN`, `ACTIVE_TIMEOUT_S`. Tasks 4–5 extend `analyze`; Task 6 wraps the engine in a resource.

- [ ] **Step 1: Write the failing test**

Append to `crates/wc-core/src/audio/input/analysis_tests.rs`:

```rust
// ---------------------------------------------------------------------------
// AnalysisEngine: time domain (Task 3)
// ---------------------------------------------------------------------------

/// Generate `len` samples of a sine at `freq` Hz, `amp` amplitude, 48 kHz,
/// phase-continuous from sample index 0.
fn sine(freq: f32, amp: f32, len: usize) -> Vec<f32> {
    (0..len)
        .map(|n| amp * (core::f32::consts::TAU * freq * (n as f32) / 48_000.0).sin())
        .collect()
}

/// Push `samples` into the engine in 800-sample chunks (one 60 Hz frame of
/// 48 kHz audio), calling `analyze` after each chunk. Returns the last
/// analysis output.
fn run_frames(engine: &mut AnalysisEngine, samples: &[f32]) -> crate::audio::input::AudioAnalysis {
    let mut out = crate::audio::input::AudioAnalysis::neutral();
    for chunk in samples.chunks(800) {
        for &s in chunk {
            engine.push(s);
        }
        out = engine.analyze(DT);
    }
    out
}

#[test]
fn engine_is_inactive_until_a_full_window_arrives() {
    let mut engine = AnalysisEngine::new(48_000);
    let out = engine.analyze(DT);
    assert!(!out.active, "no samples pushed yet");
    for &s in &sine(440.0, 0.5, FFT_SIZE - 1) {
        engine.push(s);
    }
    assert!(!engine.analyze(DT).active, "one short of a full window");
    engine.push(0.0);
    assert!(engine.analyze(DT).active, "a full window has arrived");
}

#[test]
fn engine_goes_inactive_after_samples_stop() {
    let mut engine = AnalysisEngine::new(48_000);
    run_frames(&mut engine, &sine(440.0, 0.5, 4_800));
    assert!(engine.analyze(DT).active);
    // One simulated second with no pushes: liveness times out.
    let mut out = engine.analyze(DT);
    for _ in 0..60 {
        out = engine.analyze(DT);
    }
    assert!(!out.active);
}

#[test]
fn post_agc_rms_converges_to_target_on_a_steady_sine() {
    let mut engine = AnalysisEngine::new(48_000);
    // 30 s of a steady 440 Hz sine at 0.5 amplitude (raw RMS ~0.354).
    let out = run_frames(&mut engine, &sine(440.0, 0.5, 48_000 * 30));
    assert!(
        (out.rms - AGC_TARGET_RMS).abs() < 0.03,
        "post-AGC rms {} should sit near target {}",
        out.rms,
        AGC_TARGET_RMS
    );
    assert!(out.peak > out.rms, "peak-hold rides above rms for a sine");
    assert!(out.peak <= 1.0);
    assert!(out.active);
}

#[test]
fn history_is_circular_and_the_window_reads_the_newest_samples() {
    let mut engine = AnalysisEngine::new(48_000);
    // Fill well past HISTORY_LEN with a loud DC value, then exactly one
    // window of a quiet DC value. The analysis window must see only the
    // quiet tail, proving the circular wrap points at the newest samples.
    for _ in 0..(HISTORY_LEN + 100) {
        engine.push(0.9);
    }
    for _ in 0..FFT_SIZE {
        engine.push(0.25);
    }
    engine.analyze(DT);
    assert!(
        (engine.last_raw_rms() - 0.25).abs() < 1.0e-3,
        "window raw RMS {} should reflect only the newest FFT_SIZE samples",
        engine.last_raw_rms()
    );
}

#[test]
fn engine_reset_returns_to_neutral_and_counts_from_zero() {
    let mut engine = AnalysisEngine::new(48_000);
    run_frames(&mut engine, &sine(440.0, 0.5, 9_600));
    assert!(engine.samples_received() > 0);
    engine.reset();
    assert_eq!(engine.samples_received(), 0);
    let out = engine.analyze(DT);
    assert!(!out.active);
    assert!(out.rms.abs() < f32::EPSILON);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p wc-core audio::input::analysis`
Expected: FAIL to compile — `AnalysisEngine`, `FFT_SIZE`, `HISTORY_LEN` do not exist.

- [ ] **Step 3: Write minimal implementation**

Append to `crates/wc-core/src/audio/input/analysis.rs`, below the `Agc` section and above the `#[cfg(test)]` footer:

```rust
// ---------------------------------------------------------------------------
// AnalysisEngine
// ---------------------------------------------------------------------------

use super::{AudioAnalysis, AUDIO_BAND_COUNT};

/// FFT / analysis window length in samples. A power of two supported by
/// `fundsp::fft::real_fft` (microfft-backed). At 48 kHz this is ~21 ms —
/// ~47 Hz bin resolution, comfortably recomputed once per 60 Hz frame.
pub const FFT_SIZE: usize = 1024;
/// `FFT_SIZE` as f32, kept as a literal so no runtime cast is needed.
/// Invariant: must equal `FFT_SIZE`.
const FFT_SIZE_F32: f32 = 1024.0;
/// `FFT_SIZE` as u64, kept as a literal so no runtime cast is needed.
/// Invariant: must equal `FFT_SIZE`.
const FFT_SIZE_U64: u64 = 1024;
/// Circular sample-history length. Holds ~85 ms at 48 kHz — several frames
/// of headroom over the per-frame drain (800 samples at 60 Hz) so the
/// analysis window is always fully populated with recent audio.
pub const HISTORY_LEN: usize = 4096;
/// Smoothing time constant for the published post-AGC RMS.
const RMS_SMOOTH_TAU_S: f32 = 0.1;
/// Decay time constant for the published peak-hold level.
const PEAK_DECAY_TAU_S: f32 = 0.5;
/// Seconds without a single new sample before `active` drops to false
/// (device stall / unplugged-but-not-yet-errored).
pub const ACTIVE_TIMEOUT_S: f32 = 0.5;

/// Pure, device-free analysis core for the audio-input path.
///
/// Owned by `AnalysisState` (a Bevy resource) and fed by
/// `drain_and_analyze` each `PreUpdate`; equally constructible in a unit
/// test with synthesized samples. All buffers are allocated in
/// [`AnalysisEngine::new`]; [`AnalysisEngine::push`] and
/// [`AnalysisEngine::analyze`] never allocate (hot-path rule).
pub struct AnalysisEngine {
    /// Capture sample rate in Hz (fixed per stream; a rebuild constructs a
    /// fresh engine).
    sample_rate: u32,
    /// Circular buffer of the most recent mono samples.
    history: Vec<f32>,
    /// Next write index into `history`.
    write_pos: usize,
    /// Total samples ever pushed (liveness: a full window must have arrived
    /// before the outputs mean anything).
    total_pushed: u64,
    /// Samples pushed since the last `analyze` call (liveness tracking).
    pending: usize,
    /// Seconds since the last frame that delivered at least one sample.
    seconds_since_sample: f32,
    /// Scratch the analysis window is copied into. Also the in-place FFT
    /// buffer from Task 4 onward.
    fft_scratch: Vec<f32>,
    /// Automatic gain control (post-AGC signal feeds every feature).
    agc: Agc,
    /// One-pole smoothed post-AGC RMS (the published `rms`).
    smoothed_rms: f32,
    /// Decaying peak-hold of the post-AGC window peak (the published `peak`).
    peak: f32,
    /// Raw (pre-AGC) RMS of the most recent analysis window. Diagnostic.
    last_raw_rms: f32,
}

impl AnalysisEngine {
    /// Allocate an engine for a stream at `sample_rate` Hz. This is the one
    /// place the analysis path allocates; called at stream build (event
    /// frequency), never per frame.
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            history: vec![0.0; HISTORY_LEN],
            write_pos: 0,
            total_pushed: 0,
            pending: 0,
            seconds_since_sample: ACTIVE_TIMEOUT_S,
            fft_scratch: vec![0.0; FFT_SIZE],
            agc: Agc::new(),
            smoothed_rms: 0.0,
            peak: 0.0,
            last_raw_rms: 0.0,
        }
    }

    /// Append one mono sample to the circular history. Allocation-free.
    pub fn push(&mut self, sample: f32) {
        self.history[self.write_pos] = sample;
        self.write_pos = (self.write_pos + 1) % HISTORY_LEN;
        self.total_pushed = self.total_pushed.saturating_add(1);
        self.pending = self.pending.saturating_add(1);
    }

    /// Analyze the newest window and advance all smoothers by `dt` seconds.
    /// Returns the full [`AudioAnalysis`] snapshot for this frame.
    /// Allocation-free.
    pub fn analyze(&mut self, dt: f32) -> AudioAnalysis {
        // Liveness: track how long since audio last flowed.
        if self.pending > 0 {
            self.seconds_since_sample = 0.0;
        } else {
            self.seconds_since_sample += dt;
        }
        self.pending = 0;

        // Copy the newest FFT_SIZE samples out of the circular history.
        self.fill_scratch_raw();

        // RMS + peak over the raw window. sum of squares / N, then sqrt.
        let mut sum_sq = 0.0_f32;
        let mut window_peak = 0.0_f32;
        for &s in &self.fft_scratch {
            sum_sq += s * s;
            window_peak = window_peak.max(s.abs());
        }
        let raw_rms = (sum_sq / FFT_SIZE_F32).sqrt();
        self.last_raw_rms = raw_rms;

        // AGC and the smoothed/held level outputs.
        let gain = self.agc.process(raw_rms, dt);
        self.smoothed_rms +=
            ((raw_rms * gain).min(1.0) - self.smoothed_rms) * one_pole_coeff(dt, RMS_SMOOTH_TAU_S);
        self.peak =
            ((window_peak * gain).min(1.0)).max(self.peak * (-dt / PEAK_DECAY_TAU_S).exp());

        AudioAnalysis {
            rms: self.smoothed_rms,
            gain,
            bands: [0.0; AUDIO_BAND_COUNT], // spectral bands land in Task 4
            onset: 0.0,                     // spectral flux lands in Task 5
            beat_confidence: 0.0,           // beat debounce lands in Task 5
            peak: self.peak,
            active: self.is_live(),
        }
    }

    /// Discard all state and start from silence, as if freshly constructed.
    /// Reconstructs via [`AnalysisEngine::new`] — this *does* allocate, which
    /// is fine at its event frequency (pause/resume transitions only; the
    /// caller in `drain_and_analyze` guards it to run once per transition).
    pub fn reset(&mut self) {
        *self = Self::new(self.sample_rate);
    }

    /// Total samples ever pushed (test/diagnostic surface).
    pub fn samples_received(&self) -> u64 {
        self.total_pushed
    }

    /// Raw (pre-AGC) RMS of the most recent analysis window
    /// (test/diagnostic surface).
    pub fn last_raw_rms(&self) -> f32 {
        self.last_raw_rms
    }

    /// Whether a full window has ever arrived and samples flowed recently.
    fn is_live(&self) -> bool {
        self.total_pushed >= FFT_SIZE_U64 && self.seconds_since_sample < ACTIVE_TIMEOUT_S
    }

    /// Copy the newest `FFT_SIZE` samples (ending at `write_pos`) from the
    /// circular history into `fft_scratch` — at most two `copy_from_slice`
    /// segments, no allocation.
    fn fill_scratch_raw(&mut self) {
        let start = (self.write_pos + HISTORY_LEN - FFT_SIZE) % HISTORY_LEN;
        let first_len = (HISTORY_LEN - start).min(FFT_SIZE);
        self.fft_scratch[..first_len].copy_from_slice(&self.history[start..start + first_len]);
        let rest = FFT_SIZE - first_len;
        if rest > 0 {
            self.fft_scratch[first_len..].copy_from_slice(&self.history[..rest]);
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p wc-core audio::input::analysis`
Expected: PASS (10 tests). Note: `sample_rate` is stored but only read by `reset` until Task 4 computes band bins from it — it has a real reader, so no dead-code allow is needed.

- [ ] **Step 5: Commit**

```
git add crates/wc-core/src/audio/input/analysis.rs crates/wc-core/src/audio/input/analysis_tests.rs
git commit -m "feat(audio): AnalysisEngine time-domain core with circular history, RMS, peak, liveness" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: Spectral bands — Hann window + fundsp FFT + 8 log-spaced bands

**Files:**
- Modify: `crates/wc-core/src/audio/input/analysis.rs`
- Test: `crates/wc-core/src/audio/input/analysis_tests.rs` (append)

**Interfaces:**
- Consumes: `AnalysisEngine` (Task 3); `fundsp::fft::real_fft` (in the graph; the module is NOT gated by fundsp's disabled `fft` cargo feature — that feature only gates `convolve`).
- Produces: `BAND_EDGES_HZ`, `SPECTRUM_LEN`; `AudioAnalysis.bands` populated. Task 5 reuses `self.magnitudes`.

- [ ] **Step 1: Write the failing test**

Append to `crates/wc-core/src/audio/input/analysis_tests.rs`:

```rust
// ---------------------------------------------------------------------------
// AnalysisEngine: spectral bands (Task 4)
// ---------------------------------------------------------------------------

/// Index of the strongest band in an analysis output.
fn dominant_band(bands: &[f32; AUDIO_BAND_COUNT]) -> usize {
    let mut best = 0;
    for (i, &b) in bands.iter().enumerate() {
        if b > bands[best] {
            best = i;
        }
    }
    best
}

/// Feed 4 s of a steady tone and assert the given band dominates decisively.
fn assert_tone_lands_in_band(freq: f32, expected_band: usize) {
    let mut engine = AnalysisEngine::new(48_000);
    let out = run_frames(&mut engine, &sine(freq, 0.25, 48_000 * 4));
    assert_eq!(
        dominant_band(&out.bands),
        expected_band,
        "tone at {freq} Hz should dominate band {expected_band}, bands: {:?}",
        out.bands
    );
    assert!(
        out.bands[expected_band] > 0.02,
        "dominant band should carry real energy, bands: {:?}",
        out.bands
    );
    for (i, &b) in out.bands.iter().enumerate() {
        if i != expected_band {
            assert!(
                out.bands[expected_band] > 5.0 * b,
                "band {expected_band} should dominate band {i} by 5x, bands: {:?}",
                out.bands
            );
        }
    }
}

#[test]
fn a_250_hz_tone_lands_in_band_2() {
    // BAND_EDGES_HZ: band 2 spans 200–400 Hz.
    assert_tone_lands_in_band(250.0, 2);
}

#[test]
fn a_3_khz_tone_lands_in_band_5() {
    // BAND_EDGES_HZ: band 5 spans 1600–3200 Hz.
    assert_tone_lands_in_band(3_000.0, 5);
}

#[test]
fn silence_produces_zero_bands() {
    let mut engine = AnalysisEngine::new(48_000);
    let out = run_frames(&mut engine, &vec![0.0; 48_000]);
    assert!(
        out.bands.iter().all(|b| b.abs() < 1.0e-3),
        "silent input must not excite bands: {:?}",
        out.bands
    );
}

#[test]
fn band_bins_are_monotonic_and_in_range_at_both_common_rates() {
    for rate in [44_100_u32, 48_000_u32] {
        let bins = band_bins(rate);
        let mut prev_hi = 1;
        for &(lo, hi, inv) in &bins {
            assert!(lo >= 1, "DC bin excluded");
            assert!(hi > lo, "every band has at least one bin");
            assert_eq!(lo, prev_hi, "bands tile the spectrum contiguously");
            assert!(hi <= SPECTRUM_LEN);
            assert!(inv > 0.0);
            prev_hi = hi;
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p wc-core audio::input::analysis`
Expected: FAIL to compile — `band_bins` and `SPECTRUM_LEN` do not exist; then (after a partial implementation) the band assertions would fail against the Task 3 all-zero bands.

- [ ] **Step 3: Write minimal implementation**

Three edits to `crates/wc-core/src/audio/input/analysis.rs`:

**(a)** Add consts after `ACTIVE_TIMEOUT_S`:

```rust
/// Number of usable spectrum bins from a real FFT of `FFT_SIZE` samples.
pub const SPECTRUM_LEN: usize = FFT_SIZE / 2;
/// Log-spaced (octave) band edges in Hz: 8 bands from 50 Hz to 12.8 kHz.
/// Chosen for the room-mic bar: bass emphasis at the bottom, and nothing
/// above 12.8 kHz where a party-room mic is mostly noise.
pub const BAND_EDGES_HZ: [f32; AUDIO_BAND_COUNT + 1] = [
    50.0, 100.0, 200.0, 400.0, 800.0, 1_600.0, 3_200.0, 6_400.0, 12_800.0,
];
/// Amplitude normalization for Hann-windowed magnitudes: `4 / FFT_SIZE`
/// (2x for the discarded negative frequencies, 2x for the Hann window's 0.5
/// coherent gain). Kept as a literal to avoid a runtime cast.
/// Invariant: must equal `4.0 / FFT_SIZE`.
const SPECTRUM_NORM: f32 = 0.003_906_25;
/// Band smoothing when a band is rising (fast, so hits read as hits).
const BAND_RISE_TAU_S: f32 = 0.04;
/// Band smoothing when a band is falling (slower, for visual stability).
const BAND_FALL_TAU_S: f32 = 0.3;
```

**(b)** Extend the `AnalysisEngine` struct with spectral fields (after `fft_scratch`) and initialize them in `new`:

```rust
    /// Precomputed periodic Hann window, length `FFT_SIZE`.
    hann: Vec<f32>,
    /// Normalized magnitude spectrum of the most recent window,
    /// length `SPECTRUM_LEN`.
    magnitudes: Vec<f32>,
    /// Per-band bin ranges: `(lo, hi, 1/(hi-lo))`, computed once from the
    /// sample rate.
    band_bins: [(usize, usize, f32); AUDIO_BAND_COUNT],
    /// One-pole smoothed band energies (the published `bands`).
    bands: [f32; AUDIO_BAND_COUNT],
```

In `new`, replace the struct literal's tail so it reads (only the new lines shown; keep all Task 3 fields):

```rust
            hann: hann_window(),
            magnitudes: vec![0.0; SPECTRUM_LEN],
            band_bins: band_bins(sample_rate),
            bands: [0.0; AUDIO_BAND_COUNT],
```

**(c)** In `analyze`, replace the block from the `AudioAnalysis { ... }` construction backward to just after the `self.peak = ...` line, with:

```rust
        // Window + gain, FFT in place, magnitudes. Applying the AGC gain to
        // the samples makes every spectral feature post-AGC (spec: bands are
        // post-AGC so a quiet room mic and a hot line-in drive the sketch
        // identically).
        for (s, &w) in self.fft_scratch.iter_mut().zip(self.hann.iter()) {
            *s *= w * gain;
        }
        // `real_fft` panics on a non-power-of-two length; `fft_scratch` is
        // always exactly FFT_SIZE (1024, supported), so this is an invariant,
        // not a reachable panic. It transforms in place and returns the
        // buffer transmuted to SPECTRUM_LEN complex bins (Nyquist packed
        // into bin 0's imaginary part — irrelevant here, we skip bin 0).
        let spectrum = fundsp::fft::real_fft(&mut self.fft_scratch);
        self.magnitudes[0] = 0.0;
        for i in 1..SPECTRUM_LEN {
            self.magnitudes[i] = spectrum[i].norm() * SPECTRUM_NORM;
        }

        // Log-spaced band energies: RMS of the magnitudes across each band's
        // bins, smoothed asymmetrically (fast rise, slower fall).
        for (band, &(lo, hi, inv_count)) in self.bands.iter_mut().zip(self.band_bins.iter()) {
            let mut energy = 0.0_f32;
            for &m in &self.magnitudes[lo..hi] {
                energy += m * m;
            }
            let raw = (energy * inv_count).sqrt().min(1.0);
            let tau = if raw > *band {
                BAND_RISE_TAU_S
            } else {
                BAND_FALL_TAU_S
            };
            *band += (raw - *band) * one_pole_coeff(dt, tau);
        }

        AudioAnalysis {
            rms: self.smoothed_rms,
            gain,
            bands: self.bands,
            onset: 0.0,           // spectral flux lands in Task 5
            beat_confidence: 0.0, // beat debounce lands in Task 5
            peak: self.peak,
            active: self.is_live(),
        }
```

**(d)** Add the two init helpers below the `impl AnalysisEngine` block (private, init-time only):

```rust
/// Precompute the periodic Hann window: `0.5 * (1 - cos(2*pi*n / N))`.
/// Init-time only; the index-to-f32 casts are exact for n < 2^24.
#[allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "init-time window build; indices < 1024 are exact in f32"
)]
fn hann_window() -> Vec<f32> {
    (0..FFT_SIZE)
        .map(|n| 0.5 * (1.0 - (core::f32::consts::TAU * (n as f32) / FFT_SIZE_F32).cos()))
        .collect()
}

/// Map `BAND_EDGES_HZ` onto FFT bin ranges for the given sample rate:
/// `(lo, hi, 1/(hi-lo))` per band, contiguous, each at least one bin wide,
/// DC (bin 0) excluded. The first band starts at bin 1 regardless of its
/// nominal low edge — a documented approximation at ~47 Hz resolution.
/// Init-time only.
#[allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "init-time bin mapping; edge/bin values are small, positive, and \
              value-safe in this domain (bins <= 512, rates <= 192 kHz)"
)]
fn band_bins(sample_rate: u32) -> [(usize, usize, f32); AUDIO_BAND_COUNT] {
    let bin_hz = f64::from(sample_rate) / f64::from(FFT_SIZE_F32);
    let mut out = [(1_usize, 2_usize, 1.0_f32); AUDIO_BAND_COUNT];
    let mut lo = 1_usize;
    for (band, slot) in out.iter_mut().enumerate() {
        let raw_hi = (f64::from(BAND_EDGES_HZ[band + 1]) / bin_hz).floor() as usize;
        // At least one bin per band; never past the spectrum end.
        let hi = raw_hi.clamp(lo + 1, SPECTRUM_LEN);
        *slot = (lo, hi, 1.0 / ((hi - lo) as f32));
        lo = hi;
    }
    out
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p wc-core audio::input::analysis`
Expected: PASS (14 tests).

- [ ] **Step 5: Commit**

```
git add crates/wc-core/src/audio/input/analysis.rs crates/wc-core/src/audio/input/analysis_tests.rs
git commit -m "feat(audio): eight log-spaced spectral bands via Hann-windowed fundsp FFT" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: Spectral-flux onset + debounced beat

**Files:**
- Modify: `crates/wc-core/src/audio/input/analysis.rs`
- Test: `crates/wc-core/src/audio/input/analysis_tests.rs` (append)

**Interfaces:**
- Consumes: `AnalysisEngine.magnitudes` (Task 4).
- Produces: `AudioAnalysis.onset` / `.beat_confidence` populated; `AnalysisEngine::beat_count()` (test/diagnostic surface).

- [ ] **Step 1: Write the failing test**

Append to `crates/wc-core/src/audio/input/analysis_tests.rs`:

```rust
// ---------------------------------------------------------------------------
// AnalysisEngine: onset + beat (Task 5)
// ---------------------------------------------------------------------------

/// One 60 Hz frame (800 samples at 48 kHz) that is silent except for a
/// broadband click: the first 100 samples at 0.8.
fn click_frame() -> Vec<f32> {
    let mut frame = vec![0.0_f32; 800];
    for s in frame.iter_mut().take(100) {
        *s = 0.8;
    }
    frame
}

/// One silent 60 Hz frame.
fn silent_frame() -> Vec<f32> {
    vec![0.0_f32; 800]
}

/// Push one frame of samples and analyze it.
fn step(engine: &mut AnalysisEngine, frame: &[f32]) -> crate::audio::input::AudioAnalysis {
    for &s in frame {
        engine.push(s);
    }
    engine.analyze(DT)
}

#[test]
fn silence_produces_zero_onset_and_no_beats() {
    let mut engine = AnalysisEngine::new(48_000);
    let mut out = engine.analyze(DT);
    for _ in 0..120 {
        out = step(&mut engine, &silent_frame());
    }
    assert!(out.onset.abs() < f32::EPSILON, "flux of silence is exactly 0");
    assert!(out.beat_confidence < 1.0e-3);
    assert_eq!(engine.beat_count(), 0);
}

#[test]
fn a_click_train_produces_debounced_beats_half_second_apart() {
    let mut engine = AnalysisEngine::new(48_000);
    // Settle on silence first so the click is a clean onset.
    for _ in 0..60 {
        step(&mut engine, &silent_frame());
    }
    // Two clicks 0.5 s apart (frames 0 and 30): both register as beats.
    let mut max_onset = 0.0_f32;
    for i in 0..60 {
        let out = if i == 0 || i == 30 {
            step(&mut engine, &click_frame())
        } else {
            step(&mut engine, &silent_frame())
        };
        max_onset = max_onset.max(out.onset);
        if i == 0 || i == 30 {
            assert!(
                (out.beat_confidence - 1.0).abs() < f32::EPSILON,
                "click frame {i} snaps beat confidence to 1.0 (got {})",
                out.beat_confidence
            );
        }
    }
    assert_eq!(engine.beat_count(), 2);
    assert!(
        max_onset > BEAT_ONSET_THRESHOLD,
        "click onsets must clear the beat threshold (max {max_onset})"
    );
}

#[test]
fn beats_within_the_minimum_interval_are_debounced() {
    let mut engine = AnalysisEngine::new(48_000);
    for _ in 0..60 {
        step(&mut engine, &silent_frame());
    }
    // Clicks at frames 0 and 3 — 0.05 s apart, inside MIN_BEAT_INTERVAL_S.
    for i in 0..10 {
        if i == 0 || i == 3 {
            step(&mut engine, &click_frame());
        } else {
            step(&mut engine, &silent_frame());
        }
    }
    assert_eq!(
        engine.beat_count(),
        1,
        "the second click is inside the debounce window"
    );
}

#[test]
fn beat_confidence_decays_between_beats() {
    let mut engine = AnalysisEngine::new(48_000);
    for _ in 0..60 {
        step(&mut engine, &silent_frame());
    }
    let at_beat = step(&mut engine, &click_frame());
    assert!((at_beat.beat_confidence - 1.0).abs() < f32::EPSILON);
    let mut later = at_beat;
    for _ in 0..30 {
        later = step(&mut engine, &silent_frame());
    }
    assert!(
        later.beat_confidence < 0.3,
        "confidence should have decayed well below 1 after 0.5 s (got {})",
        later.beat_confidence
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p wc-core audio::input::analysis`
Expected: FAIL — `beat_count` does not exist; after adding it, the onset/beat assertions fail against Task 4's hardcoded zeros.

- [ ] **Step 3: Write minimal implementation**

Three edits to `crates/wc-core/src/audio/input/analysis.rs`:

**(a)** Consts, after `BAND_FALL_TAU_S`:

```rust
/// Time constant of the running mean the spectral flux is normalized by.
const FLUX_MEAN_TAU_S: f32 = 2.0;
/// Floor for the flux running mean, so the first sound after silence cannot
/// register as an unbounded onset.
const FLUX_MEAN_FLOOR: f32 = 0.05;
/// Normalized-onset value that (subject to debounce) counts as a beat.
const BEAT_ONSET_THRESHOLD: f32 = 2.5;
/// Minimum spacing between beats — the debounce window (240 BPM ceiling).
const MIN_BEAT_INTERVAL_S: f32 = 0.25;
/// Decay time constant of the published beat confidence between beats.
const BEAT_CONFIDENCE_DECAY_TAU_S: f32 = 0.3;
```

Make `BEAT_ONSET_THRESHOLD` `pub(crate)` — the test asserts against it: `pub(crate) const BEAT_ONSET_THRESHOLD: f32 = 2.5;` (the sibling-test `#[path]` module is inside this module, so plain private also works; keep it private and the test reaches it via `super::*`. Use **private**, matching the other tuning consts.)

**(b)** Struct fields (after `bands`) and their `new` initializers:

```rust
    /// Magnitude spectrum of the previous window (spectral-flux reference).
    prev_magnitudes: Vec<f32>,
    /// Slow running mean of the spectral flux (onset normalizer).
    flux_mean: f32,
    /// Seconds since the last debounced beat.
    seconds_since_beat: f32,
    /// Published beat confidence: 1.0 at a beat, exponential decay between.
    beat_confidence: f32,
    /// Total debounced beats detected (test/diagnostic counter).
    beats: u64,
```

```rust
            prev_magnitudes: vec![0.0; SPECTRUM_LEN],
            flux_mean: 0.0,
            // Start "ready": the first onset may immediately be a beat.
            seconds_since_beat: MIN_BEAT_INTERVAL_S,
            beat_confidence: 0.0,
            beats: 0,
```

**(c)** In `analyze`, between the band loop and the final `AudioAnalysis { ... }` literal, insert:

```rust
        // Spectral flux: positive-only magnitude change since the previous
        // window, summed over the spectrum (bin 0 excluded — it is zeroed
        // above). Onset strength is flux relative to its own slow running
        // mean, floored so silence cannot make the next sound register as an
        // unbounded onset.
        let mut flux = 0.0_f32;
        for (m, p) in self.magnitudes[1..]
            .iter()
            .zip(self.prev_magnitudes[1..].iter())
        {
            flux += (m - p).max(0.0);
        }
        self.prev_magnitudes.copy_from_slice(&self.magnitudes);
        let onset = flux / self.flux_mean.max(FLUX_MEAN_FLOOR);
        self.flux_mean += (flux - self.flux_mean) * one_pole_coeff(dt, FLUX_MEAN_TAU_S);

        // Debounced beat: an onset spike no sooner than MIN_BEAT_INTERVAL_S
        // after the previous beat snaps confidence to 1.0; between beats the
        // confidence decays exponentially.
        self.seconds_since_beat += dt;
        if onset > BEAT_ONSET_THRESHOLD && self.seconds_since_beat >= MIN_BEAT_INTERVAL_S {
            self.seconds_since_beat = 0.0;
            self.beat_confidence = 1.0;
            self.beats = self.beats.saturating_add(1);
        } else {
            self.beat_confidence *= (-dt / BEAT_CONFIDENCE_DECAY_TAU_S).exp();
        }
```

…and change the output literal's placeholders to the real values:

```rust
        AudioAnalysis {
            rms: self.smoothed_rms,
            gain,
            bands: self.bands,
            onset,
            beat_confidence: self.beat_confidence,
            peak: self.peak,
            active: self.is_live(),
        }
```

**(d)** Add the counter accessor next to `samples_received`:

```rust
    /// Total debounced beats detected since construction/reset
    /// (test/diagnostic surface).
    pub fn beat_count(&self) -> u64 {
        self.beats
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p wc-core audio::input::analysis`
Expected: PASS (18 tests). This completes the pure-DSP surface: every spec-listed audio unit test (AGC step convergence, tone bands, click-train onsets) is green with no audio device.

- [ ] **Step 5: Commit**

```
git add crates/wc-core/src/audio/input/analysis.rs crates/wc-core/src/audio/input/analysis_tests.rs
git commit -m "feat(audio): spectral-flux onset strength and debounced beat detection" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: Ring handoff types + the drain system

**Files:**
- Modify: `crates/wc-core/src/audio/input/capture.rs` (replace the Task 1 stub)
- Modify: `crates/wc-core/src/audio/input/analysis.rs` (append `AnalysisState` + `drain_and_analyze`)
- Modify: `crates/wc-core/src/audio/input/mod.rs` (register the drain system)
- Test: `crates/wc-core/src/audio/input/analysis_tests.rs` (append)

**Interfaces:**
- Consumes: `AnalysisEngine` (Tasks 3–5), `AudioAnalysis`/`AudioCaptureRequest` (Task 1), `rtrb`.
- Produces: `capture::{AudioInputRing, AudioInputErrorFlag, AudioInputStatus, CaptureRuntime, RING_SAMPLE_CAPACITY, RETRY_COOLDOWN_S}`; `analysis::{AnalysisState, drain_and_analyze}`. Task 8's driver inserts/removes these around the real stream.

- [ ] **Step 1: Write the failing test**

Append to `crates/wc-core/src/audio/input/analysis_tests.rs`:

```rust
// ---------------------------------------------------------------------------
// drain_and_analyze system (Task 6)
// ---------------------------------------------------------------------------

use bevy::prelude::*;

use crate::audio::input::capture::{AudioInputRing, RING_SAMPLE_CAPACITY};
use crate::audio::input::AudioCaptureRequest;

/// Headless drain-test app: ring + engine + request wired by hand, only the
/// drain system registered. NO capture driver and NO real cpal stream — see
/// the plan's execution notes (a request + driver would open a live mic).
fn drain_test_app(request: AudioCaptureRequest) -> (App, rtrb::Producer<f32>) {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.init_resource::<crate::audio::input::AudioAnalysis>();
    app.insert_resource(request);
    app.insert_resource(AnalysisState(AnalysisEngine::new(48_000)));
    let (producer, consumer) = rtrb::RingBuffer::<f32>::new(RING_SAMPLE_CAPACITY);
    app.world_mut().insert_non_send(AudioInputRing::new(consumer));
    app.add_systems(PreUpdate, drain_and_analyze);
    (app, producer)
}

#[test]
fn drain_empties_a_completely_full_ring_in_one_frame() {
    let (mut app, mut producer) = drain_test_app(AudioCaptureRequest {
        device_name: None,
        paused: false,
    });
    // Buffer pressure: fill the ring to capacity, then overflow it — the
    // overflow push is refused (dropped by the callback in production),
    // never a panic or a block.
    for _ in 0..RING_SAMPLE_CAPACITY {
        producer.push(0.25).expect("fits within capacity");
    }
    assert!(producer.push(0.5).is_err(), "full ring refuses the push");
    app.update();
    let received = app
        .world()
        .resource::<AnalysisState>()
        .0
        .samples_received();
    assert_eq!(
        received,
        u64::try_from(RING_SAMPLE_CAPACITY).expect("capacity fits u64"),
        "one frame drains the entire backlog"
    );
    assert!(app.world().resource::<crate::audio::input::AudioAnalysis>().active);
    assert_eq!(
        producer.slots(),
        RING_SAMPLE_CAPACITY,
        "ring fully drained: every slot free again"
    );
}

#[test]
fn paused_request_discards_samples_and_holds_neutral() {
    let (mut app, mut producer) = drain_test_app(AudioCaptureRequest {
        device_name: None,
        paused: true,
    });
    for _ in 0..4_096 {
        producer.push(0.5).expect("fits within capacity");
    }
    app.update();
    assert_eq!(
        *app.world().resource::<crate::audio::input::AudioAnalysis>(),
        crate::audio::input::AudioAnalysis::neutral()
    );
    assert_eq!(
        producer.slots(),
        RING_SAMPLE_CAPACITY,
        "paused drain discards in-flight samples so resume starts fresh"
    );
    assert_eq!(
        app.world()
            .resource::<AnalysisState>()
            .0
            .samples_received(),
        0,
        "discarded samples are never analyzed"
    );
}

#[test]
fn missing_capture_resources_hold_neutral() {
    // The plugin's steady state outside Radiance: no request, no ring, no
    // engine. The system must no-op to neutral, never panic.
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.init_resource::<crate::audio::input::AudioAnalysis>();
    app.add_systems(PreUpdate, drain_and_analyze);
    app.update();
    app.update();
    assert_eq!(
        *app.world().resource::<crate::audio::input::AudioAnalysis>(),
        crate::audio::input::AudioAnalysis::neutral()
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p wc-core audio::input`
Expected: FAIL to compile — `AudioInputRing`, `RING_SAMPLE_CAPACITY`, `AnalysisState`, `drain_and_analyze` do not exist.

- [ ] **Step 3: Write minimal implementation**

**(a)** Replace `crates/wc-core/src/audio/input/capture.rs` with:

```rust
//! cpal input-stream lifecycle for the audio-input path.
//!
//! ## Thread model
//!
//! - The **cpal input thread** owns the producer end of a lock-free `rtrb`
//!   sample ring plus a clone of [`AudioInputErrorFlag`]'s atomic. Its data
//!   callback downmixes each frame to mono and pushes; its error callback
//!   stores one relaxed `true`. No allocation, no locks, no logging on
//!   either (the `audio::engine` discipline, in reverse).
//! - The **Bevy main thread** owns everything else: [`AudioInputRing`]
//!   (non-send consumer, drained by `analysis::drain_and_analyze`) and the
//!   capture driver (Task 8) that builds/pauses/tears down the stream in
//!   response to `super::AudioCaptureRequest`.
//!
//! This file lands in two steps: the data-handoff types here (Task 6), then
//! the stream build + `drive_capture` driver (Task 8).

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use bevy::prelude::*;

/// Capacity of the input sample ring (mono f32 samples). ~340 ms at 48 kHz.
/// The `PreUpdate` drain empties it every frame (800 samples/frame at
/// 60 Hz), so this covers multi-frame render stalls; past that the callback
/// drops samples — analysis is best-effort, and a dropped sample is strictly
/// better than a blocked OS audio thread.
pub const RING_SAMPLE_CAPACITY: usize = 16_384;

/// Seconds between capture (re)build attempts after a failure. Keeps a
/// missing device from being probed every frame while still reacquiring a
/// kiosk mic within a couple of seconds of it reappearing.
pub const RETRY_COOLDOWN_S: f32 = 2.0;

/// Consumer end of the audio-input sample ring.
///
/// Installed as a **non-send** resource: `rtrb::Consumer` is `Send` but not
/// `Sync` (the same reasoning as `audio::ring`), so systems take it as
/// `NonSendMut`, pinning access to the main thread by construction.
pub struct AudioInputRing {
    /// Consumer half of the ring; the producer half lives in the cpal
    /// input callback.
    consumer: rtrb::Consumer<f32>,
}

impl AudioInputRing {
    /// Wrap the consumer half of an input ring. Called by the capture
    /// driver at stream build; also available to tests that construct rings
    /// manually without a real cpal stream.
    pub fn new(consumer: rtrb::Consumer<f32>) -> Self {
        Self { consumer }
    }

    /// Pop one sample, oldest first. `None` when the ring is empty.
    pub fn pop(&mut self) -> Option<f32> {
        self.consumer.pop().ok()
    }
}

/// Lock-free flag shared with the cpal input error callback (mirrors
/// `audio::state::AudioErrorFlag`). The callback runs on an OS audio thread
/// and must not allocate, lock, or log — it only stores `true` with a
/// relaxed atomic write. The capture driver swaps the flag each frame and
/// responds by tearing down and rebuilding the stream.
#[derive(Resource, Clone)]
pub struct AudioInputErrorFlag(pub Arc<AtomicBool>);

/// Diagnostic status of the audio-input capture path.
///
/// Written by the capture driver at event frequency only (build, teardown,
/// failure), so the `String`s in these variants are never per-frame
/// allocations. Read by diagnostics/dev UI; sketches should read
/// `super::AudioAnalysis` instead.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub enum AudioInputStatus {
    /// No `super::AudioCaptureRequest` present; capture torn down.
    #[default]
    Inactive,
    /// Capture is running.
    Running {
        /// Resolved cpal device name.
        device: String,
        /// Capture sample rate in Hz.
        sample_rate: u32,
    },
    /// Capture failed to build or died mid-run; retrying on a cooldown.
    Errored {
        /// Human-readable failure description.
        message: String,
    },
}

/// Main-thread bookkeeping for the capture driver.
///
/// Present from plugin build (`Default` = nothing running). All fields are
/// written at event frequency and read each frame by `drive_capture`
/// (Task 8).
#[derive(Resource, Default)]
pub struct CaptureRuntime {
    /// The *requested* device name the live stream was built for (`None` =
    /// system default). Compared against the current request each frame to
    /// detect device changes; the resolved cpal name lives in
    /// [`AudioInputStatus::Running`].
    pub current_device: Option<String>,
    /// Whether the live stream is currently paused.
    pub paused: bool,
    /// Whether the last build attempt failed (gates the retry cooldown).
    pub failed: bool,
    /// Seconds remaining before another build attempt is allowed.
    pub retry_timer: f32,
}
```

**(b)** Append to `crates/wc-core/src/audio/input/analysis.rs` (above the `#[cfg(test)]` footer):

```rust
// ---------------------------------------------------------------------------
// Bevy surface: resource + drain system
// ---------------------------------------------------------------------------

use bevy::ecs::system::NonSendMut;
use bevy::prelude::*;

use super::capture::AudioInputRing;
use super::AudioCaptureRequest;

/// Bevy resource owning the [`AnalysisEngine`] for the live capture stream.
///
/// Inserted by the capture driver at stream build (constructed with the
/// stream's actual sample rate) and removed at teardown — its presence is
/// the analysis system's signal that capture is up.
#[derive(Resource)]
pub struct AnalysisState(pub AnalysisEngine);

/// `PreUpdate` system: drain the input ring into the engine and publish
/// `super::AudioAnalysis`.
///
/// Runs every frame in every state (sanctioned always-on core plumbing,
/// like the settings-reload listeners): with no request/ring/engine present
/// it holds the neutral value behind an equality guard and returns — a few
/// resource-existence checks per frame, no allocation ever. Scheduled after
/// `capture::drive_capture` (Task 8 chains them), so a teardown this frame
/// is observed this frame.
pub fn drain_and_analyze(
    time: Res<'_, Time>,
    request: Option<Res<'_, AudioCaptureRequest>>,
    ring: Option<NonSendMut<'_, AudioInputRing>>,
    state: Option<ResMut<'_, AnalysisState>>,
    mut analysis: ResMut<'_, super::AudioAnalysis>,
) {
    let (Some(request), Some(mut ring), Some(mut state)) = (request, ring, state) else {
        // Inactive or failed: hold neutral. The equality guard keeps Bevy
        // change detection quiet in the steady no-capture state.
        if *analysis != super::AudioAnalysis::neutral() {
            *analysis = super::AudioAnalysis::neutral();
        }
        return;
    };
    if request.paused {
        // Paused (Idle/Screensaver): discard anything in flight so resume
        // starts fresh. The stream itself is paused by the capture driver,
        // so this loop is empty in steady state.
        while ring.pop().is_some() {}
        if *analysis != super::AudioAnalysis::neutral() {
            // One-shot on the pause transition (guarded by the equality
            // check): clear the engine so AGC/smoothers do not carry stale
            // state across the pause. reset() reallocates, which is fine at
            // transition frequency.
            state.0.reset();
            *analysis = super::AudioAnalysis::neutral();
        }
        return;
    }
    while let Some(sample) = ring.pop() {
        state.0.push(sample);
    }
    *analysis = state.0.analyze(time.delta_secs());
}
```

Note: `use bevy::prelude::*;` mid-file is fine here because the file had no
bevy imports before this task; rustfmt will not reorder across the comment
banner. If clippy complains about duplicate imports, hoist both `use` lines
to the file top instead.

**(c)** In `crates/wc-core/src/audio/input/mod.rs`, extend `AudioInputPlugin::build`:

```rust
    fn build(&self, app: &mut App) {
        app.init_resource::<AudioAnalysis>()
            .init_resource::<capture::AudioInputStatus>()
            .init_resource::<capture::CaptureRuntime>()
            .add_systems(PreUpdate, analysis::drain_and_analyze);
        // Task 8 chains capture::drive_capture ahead of the drain.
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p wc-core audio::input`
Expected: PASS (23 tests: 18 analysis + 3 drain + 2 mod). Also `cargo check -p wc-core` to confirm the new capture.rs types compile under `missing_docs`.

- [ ] **Step 5: Commit**

```
git add crates/wc-core/src/audio/input/capture.rs crates/wc-core/src/audio/input/analysis.rs crates/wc-core/src/audio/input/analysis_tests.rs crates/wc-core/src/audio/input/mod.rs
git commit -m "feat(audio): input ring handoff types and the PreUpdate drain-and-analyze system" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: Device enumeration + runtime-enum registration

**Files:**
- Modify: `crates/wc-core/src/audio/input/devices.rs` (replace the Task 1 stub)
- Modify: `crates/wc-core/src/audio/input/mod.rs` (register resource, source, and systems)
- Test: `#[cfg(test)] mod tests` at the footer of `devices.rs`

**Interfaces:**
- Consumes: `crate::settings::{RegisterRuntimeEnumOptionsExt, RuntimeEnumOptionsSource}`; `crate::settings::runtime_enum::{snapshot, options_for}` (pub(crate), reachable from this in-crate test); cpal device enumeration.
- Produces: pinned `AvailableAudioInputDevices` with `OPTIONS_KEY = "audio_input_devices"`; systems `enumerate_input_devices` (Startup) and `refresh_devices_on_request_added` (Update). Plan C's `RadianceSettings` field declares `ty = RuntimeEnum, options_key = "audio_input_devices"` against this source.

- [ ] **Step 1: Write the failing test**

Replace `crates/wc-core/src/audio/input/devices.rs` with the stub doc plus the test module only (implementation comes in Step 3; the test module compiles against names that do not exist yet, which is the failure):

```rust
//! Input-device enumeration + runtime-enum registration. Populated in Task 7.

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions; expect_used is denied workspace-wide for non-test code"
)]
mod tests {
    use super::*;
    use crate::settings::runtime_enum::{options_for, snapshot};
    use crate::settings::{RegisterRuntimeEnumOptionsExt, RuntimeEnumOptionsSource};
    use bevy::prelude::*;

    #[test]
    fn options_key_matches_the_pinned_contract() {
        // The string is the whole cross-module contract (see the
        // runtime_enum module docs): Plan C's RadianceSettings field will
        // declare options_key = "audio_input_devices" — a mismatch here
        // degrades into an empty dropdown, so pin it with a test.
        assert_eq!(AvailableAudioInputDevices::OPTIONS_KEY, "audio_input_devices");
    }

    #[test]
    fn registered_source_resolves_through_the_registry() {
        let mut app = App::new();
        app.register_runtime_enum_options::<AvailableAudioInputDevices>();
        app.insert_resource(AvailableAudioInputDevices(vec![
            "USB Interface".to_owned(),
            "Built-in Microphone".to_owned(),
        ]));
        let snap = snapshot(app.world());
        assert_eq!(
            options_for(&snap, AvailableAudioInputDevices::OPTIONS_KEY).to_vec(),
            vec!["USB Interface".to_owned(), "Built-in Microphone".to_owned()]
        );
    }

    #[test]
    fn refresh_runs_only_on_the_frame_the_request_is_added() {
        // Marker check via change detection semantics: the refresh system's
        // run condition is is_added() on the request. We can't call cpal in
        // an assertion-friendly way here, so this exercises the guard path
        // only: with no request, the device list is untouched by Update.
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.insert_resource(AvailableAudioInputDevices(vec!["Sentinel".to_owned()]));
        app.add_systems(Update, refresh_devices_on_request_added);
        app.update();
        app.update();
        assert_eq!(
            app.world().resource::<AvailableAudioInputDevices>().0,
            vec!["Sentinel".to_owned()],
            "no request added, so the sentinel list must be untouched"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p wc-core audio::input::devices`
Expected: FAIL to compile — `AvailableAudioInputDevices` and `refresh_devices_on_request_added` do not exist.

- [ ] **Step 3: Write minimal implementation**

Insert above the test module in `devices.rs` (replacing the stub doc line):

```rust
//! Input-device enumeration for the audio-input device picker.
//!
//! Publishes [`AvailableAudioInputDevices`] and registers it as the
//! runtime-enum options source under the pinned key
//! `"audio_input_devices"` — the exact use case the
//! `crate::settings::runtime_enum` module docs anticipate. Plan C binds a
//! `SettingKind::RuntimeEnum { options_key: "audio_input_devices" }` field
//! on its sketch settings to this list; an empty string / `None` request
//! means the system default input device.
//!
//! Enumeration runs at **event frequency only** (app startup, plus a
//! refresh on the frame a `super::AudioCaptureRequest` is inserted), never
//! per frame — `cpal` enumeration can be slow and allocates the name
//! `String`s, which is fine at that cadence.

use bevy::prelude::*;
use cpal::traits::{DeviceTrait, HostTrait};

use crate::settings::RuntimeEnumOptionsSource;

use super::AudioCaptureRequest;

/// Names of every input device cpal can currently enumerate, in cpal
/// order. Pinned Radiance contract; the runtime-enum source for the
/// `"audio_input_devices"` dropdown.
#[derive(Resource, Default)]
pub struct AvailableAudioInputDevices(pub Vec<String>);

impl RuntimeEnumOptionsSource for AvailableAudioInputDevices {
    const OPTIONS_KEY: &'static str = "audio_input_devices";

    fn options(&self) -> &[String] {
        &self.0
    }
}

/// `Startup` system: seed [`AvailableAudioInputDevices`] with the devices
/// present at launch.
pub fn enumerate_input_devices(mut list: ResMut<'_, AvailableAudioInputDevices>) {
    list.0 = current_input_device_names();
    tracing::info!(count = list.0.len(), devices = ?list.0, "audio input devices enumerated");
}

/// `Update` system: re-enumerate on the frame a capture request is inserted,
/// so the settings dropdown reflects devices plugged in since startup by the
/// time Radiance's panel can show it. No-ops (one `Option` check) on every
/// other frame.
pub fn refresh_devices_on_request_added(
    request: Option<Res<'_, AudioCaptureRequest>>,
    mut list: ResMut<'_, AvailableAudioInputDevices>,
) {
    if request.is_some_and(|r| r.is_added()) {
        list.0 = current_input_device_names();
        tracing::debug!(count = list.0.len(), "audio input devices re-enumerated");
    }
}

/// Enumerate input-device names from the default cpal host. Event-frequency
/// only; allocates the returned names. Enumeration failure (headless CI, no
/// audio subsystem) degrades to an empty list — the dropdown renders empty
/// and the persisted value shows as unavailable, which the settings panel
/// already handles.
fn current_input_device_names() -> Vec<String> {
    let host = cpal::default_host();
    match host.input_devices() {
        Ok(devices) => devices.filter_map(|d| d.name().ok()).collect(),
        Err(err) => {
            tracing::warn!(?err, "audio input device enumeration failed");
            Vec::new()
        }
    }
}
```

And in `crates/wc-core/src/audio/input/mod.rs`, extend the plugin (final shape before Task 8):

```rust
use crate::settings::RegisterRuntimeEnumOptionsExt;
```

```rust
    fn build(&self, app: &mut App) {
        app.init_resource::<AudioAnalysis>()
            .init_resource::<devices::AvailableAudioInputDevices>()
            .init_resource::<capture::AudioInputStatus>()
            .init_resource::<capture::CaptureRuntime>()
            .register_runtime_enum_options::<devices::AvailableAudioInputDevices>()
            .add_systems(Startup, devices::enumerate_input_devices)
            .add_systems(Update, devices::refresh_devices_on_request_added)
            .add_systems(PreUpdate, analysis::drain_and_analyze);
        // Task 8 chains capture::drive_capture ahead of the drain.
    }
```

Note: the debug-build `warn_on_unresolved_options_keys` cross-check will stay quiet — it only warns for *declared fields* without a source, and registering a source without a field is legal (the field arrives with Plan C).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p wc-core audio::input`
Expected: PASS (26 tests). The `plugin_installs_neutral_analysis_resource` test now also exercises `enumerate_input_devices` on Startup — on a headless runner cpal returns an empty/erroring enumeration, which degrades to an empty list without panicking (that degradation is exactly what the implementation guarantees).

- [ ] **Step 5: Commit**

```
git add crates/wc-core/src/audio/input/devices.rs crates/wc-core/src/audio/input/mod.rs
git commit -m "feat(audio): enumerate input devices and register the audio_input_devices runtime enum" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 8: cpal input stream + capture driver + smoke harness

**Files:**
- Modify: `crates/wc-core/src/audio/input/capture.rs` (append the stream/driver half)
- Modify: `crates/wc-core/src/audio/input/mod.rs` (chain the driver ahead of the drain; add the smoke harness)
- Test: `#[cfg(test)] mod tests` at the footer of `capture.rs`

**Interfaces:**
- Consumes: Task 6's types; `analysis::{AnalysisState, AnalysisEngine}`; `super::AudioCaptureRequest`; cpal 0.16 input API (`default_input_device`, `input_devices`, `default_input_config`, `build_input_stream`, 4-arg form with `Option<Duration>` timeout — mirror `audio/engine.rs`).
- Produces: `AudioInputStream` (non-send), `drive_capture` (exclusive system), pure `decide`/`CaptureAction`/`CaptureInputs` (unit-tested), `push_mono` (unit-tested). This completes the runtime path Plan C activates.

- [ ] **Step 1: Write the failing test**

Append to `crates/wc-core/src/audio/input/capture.rs` footer:

```rust
#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions; expect_used is denied workspace-wide for non-test code"
)]
mod tests {
    use super::*;
    use crate::audio::input::AudioCaptureRequest;

    fn request(device: Option<&str>, paused: bool) -> AudioCaptureRequest {
        AudioCaptureRequest {
            device_name: device.map(String::from),
            paused,
        }
    }

    fn inputs<'a>(req: Option<&'a AudioCaptureRequest>) -> CaptureInputs<'a> {
        CaptureInputs {
            requested: req,
            stream_alive: false,
            current_device: None,
            stream_paused: false,
            error_fired: false,
            failed: false,
            retry_timer_elapsed: true,
        }
    }

    // --- decide(): the driver's whole policy, as a pure table ---

    #[test]
    fn no_request_and_nothing_running_is_a_no_op() {
        assert_eq!(decide(&inputs(None)), CaptureAction::None);
    }

    #[test]
    fn no_request_with_a_live_stream_tears_down() {
        let mut i = inputs(None);
        i.stream_alive = true;
        assert_eq!(decide(&i), CaptureAction::Teardown);
    }

    #[test]
    fn no_request_with_a_failed_build_clears_the_failure() {
        let mut i = inputs(None);
        i.failed = true;
        assert_eq!(decide(&i), CaptureAction::Teardown);
    }

    #[test]
    fn a_request_with_no_stream_builds() {
        let req = request(None, false);
        assert_eq!(decide(&inputs(Some(&req))), CaptureAction::Build);
    }

    #[test]
    fn a_failed_build_waits_for_the_retry_cooldown() {
        let req = request(None, false);
        let mut i = inputs(Some(&req));
        i.failed = true;
        i.retry_timer_elapsed = false;
        assert_eq!(decide(&i), CaptureAction::None);
        i.retry_timer_elapsed = true;
        assert_eq!(decide(&i), CaptureAction::Build);
    }

    #[test]
    fn a_device_change_rebuilds() {
        let req = request(Some("USB Interface"), false);
        let mut i = inputs(Some(&req));
        i.stream_alive = true;
        i.current_device = Some("Built-in Microphone");
        assert_eq!(decide(&i), CaptureAction::Rebuild);
    }

    #[test]
    fn a_stream_error_rebuilds() {
        let req = request(None, false);
        let mut i = inputs(Some(&req));
        i.stream_alive = true;
        i.error_fired = true;
        assert_eq!(decide(&i), CaptureAction::Rebuild);
    }

    #[test]
    fn pause_state_follows_the_request() {
        let paused_req = request(None, true);
        let mut i = inputs(Some(&paused_req));
        i.stream_alive = true;
        assert_eq!(decide(&i), CaptureAction::Pause);

        let live_req = request(None, false);
        let mut i = inputs(Some(&live_req));
        i.stream_alive = true;
        i.stream_paused = true;
        assert_eq!(decide(&i), CaptureAction::Resume);
    }

    #[test]
    fn a_healthy_matching_stream_is_a_no_op() {
        let req = request(Some("USB Interface"), false);
        let mut i = inputs(Some(&req));
        i.stream_alive = true;
        i.current_device = Some("USB Interface");
        assert_eq!(decide(&i), CaptureAction::None);
    }

    // --- push_mono(): the RT callback's only logic ---

    #[test]
    fn push_mono_downmixes_interleaved_stereo() {
        let (mut producer, mut consumer) = rtrb::RingBuffer::<f32>::new(8);
        // Two stereo frames: (1.0, 0.0) and (-0.5, -0.5).
        push_mono(&[1.0, 0.0, -0.5, -0.5], 2, 0.5, &mut producer, |s| s);
        assert!((consumer.pop().expect("frame 1") - 0.5).abs() < f32::EPSILON);
        assert!((consumer.pop().expect("frame 2") + 0.5).abs() < f32::EPSILON);
        assert!(consumer.pop().is_err(), "exactly two mono frames");
    }

    #[test]
    fn push_mono_converts_via_the_provided_closure() {
        let (mut producer, mut consumer) = rtrb::RingBuffer::<f32>::new(8);
        push_mono(
            &[i16::MAX, i16::MIN],
            1,
            1.0,
            &mut producer,
            |s| f32::from(s) / 32_768.0,
        );
        let a = consumer.pop().expect("first");
        let b = consumer.pop().expect("second");
        assert!(a > 0.999 && a <= 1.0);
        assert!((b + 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn push_mono_drops_samples_when_the_ring_is_full_without_panicking() {
        let (mut producer, mut consumer) = rtrb::RingBuffer::<f32>::new(4);
        let data = [0.1_f32, 0.2, 0.3, 0.4, 0.5, 0.6];
        push_mono(&data, 1, 1.0, &mut producer, |s| s);
        // First 4 kept, overflow dropped silently.
        for expected in [0.1_f32, 0.2, 0.3, 0.4] {
            assert!((consumer.pop().expect("kept") - expected).abs() < f32::EPSILON);
        }
        assert!(consumer.pop().is_err());
    }

    #[test]
    fn push_mono_with_zero_channels_is_a_no_op() {
        let (mut producer, mut consumer) = rtrb::RingBuffer::<f32>::new(4);
        push_mono(&[0.5_f32], 0, 1.0, &mut producer, |s| s);
        assert!(consumer.pop().is_err());
        drop(producer);
    }

    // --- driver no-op path (headless-safe: no request is ever inserted) ---

    #[test]
    fn drive_capture_without_a_request_is_inert() {
        use bevy::prelude::*;
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.init_resource::<AudioInputStatus>();
        app.init_resource::<CaptureRuntime>();
        app.add_systems(PreUpdate, drive_capture);
        app.update();
        app.update();
        assert_eq!(
            *app.world().resource::<AudioInputStatus>(),
            AudioInputStatus::Inactive
        );
        assert!(app
            .world()
            .get_non_send_resource::<AudioInputStream>()
            .is_none());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p wc-core audio::input::capture`
Expected: FAIL to compile — `decide`, `CaptureAction`, `CaptureInputs`, `push_mono`, `AudioInputStream`, `drive_capture` do not exist.

- [ ] **Step 3: Write minimal implementation**

Append to `crates/wc-core/src/audio/input/capture.rs` (below `CaptureRuntime`, above the test module), and add these to the file's import block: `use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};`, `use super::analysis::{AnalysisEngine, AnalysisState};`, `use super::AudioCaptureRequest;`. (`Ordering` is deliberately NOT imported — the code below writes `std::sync::atomic::Ordering::Relaxed` fully qualified, matching how rarely it appears; importing it too would be an unused-import warning if usage stays qualified.)

```rust
/// Wraps the live input `cpal::Stream` so Bevy keeps it alive. `cpal::Stream`
/// is `!Send` on macOS, hence a **non-send** resource — exactly like the
/// output engine's `audio::engine::AudioStream`.
pub struct AudioInputStream {
    /// Owned stream handle. Dropping it stops the OS input callback.
    stream: cpal::Stream,
}

impl AudioInputStream {
    /// Suspend the input callback. Errors are logged, never panicked — a
    /// failed pause leaves capture running, which is wasteful but harmless.
    pub fn pause(&self) {
        if let Err(err) = self.stream.pause() {
            tracing::warn!(?err, "cpal input stream pause failed");
        } else {
            tracing::debug!("cpal input stream paused");
        }
    }

    /// Resume the input callback after a pause. Errors are logged, never
    /// panicked — a failed play leaves analysis neutral, not broken.
    pub fn play(&self) {
        if let Err(err) = self.stream.play() {
            tracing::warn!(?err, "cpal input stream play failed");
        } else {
            tracing::debug!("cpal input stream resumed");
        }
    }
}

/// What `drive_capture` should do this frame. Derived by [`decide`] from
/// pure inputs so the policy is unit-testable without a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureAction {
    /// Nothing to do.
    None,
    /// Build a stream (none is running and one is wanted).
    Build,
    /// Tear the stream (and bookkeeping) down.
    Teardown,
    /// Tear down and immediately rebuild (device change or stream error).
    Rebuild,
    /// Pause the live stream.
    Pause,
    /// Resume the paused stream.
    Resume,
}

/// Pure inputs to [`decide`], gathered from the world each frame without
/// allocating (device names are compared as `&str`).
pub(crate) struct CaptureInputs<'a> {
    /// The current `AudioCaptureRequest`, if inserted.
    pub requested: Option<&'a AudioCaptureRequest>,
    /// Whether an `AudioInputStream` non-send resource is live.
    pub stream_alive: bool,
    /// The requested device name the live stream was built for.
    pub current_device: Option<&'a str>,
    /// Whether the live stream is paused.
    pub stream_paused: bool,
    /// Whether the cpal error callback fired since last frame.
    pub error_fired: bool,
    /// Whether the last build attempt failed.
    pub failed: bool,
    /// Whether the retry cooldown has elapsed (always true when not failed).
    pub retry_timer_elapsed: bool,
}

/// The capture driver's policy, as a pure function of [`CaptureInputs`].
pub(crate) fn decide(i: &CaptureInputs<'_>) -> CaptureAction {
    let Some(req) = i.requested else {
        // Nothing wanted: clear a live stream or a stale failure marker.
        return if i.stream_alive || i.failed {
            CaptureAction::Teardown
        } else {
            CaptureAction::None
        };
    };
    if !i.stream_alive {
        // Wanted but not running: build now, or wait out the failure cooldown.
        return if !i.failed || i.retry_timer_elapsed {
            CaptureAction::Build
        } else {
            CaptureAction::None
        };
    }
    if i.error_fired {
        // The stream died mid-run (device unplugged, backend error).
        return CaptureAction::Rebuild;
    }
    if req.device_name.as_deref() != i.current_device {
        // The operator picked a different device.
        return CaptureAction::Rebuild;
    }
    match (req.paused, i.stream_paused) {
        (true, false) => CaptureAction::Pause,
        (false, true) => CaptureAction::Resume,
        _ => CaptureAction::None,
    }
}

/// `PreUpdate` exclusive system: reconcile the live capture stream with
/// `super::AudioCaptureRequest` every frame.
///
/// Exclusive (`&mut World`) because building/tearing down inserts and
/// removes **non-send** resources, which `Commands` cannot do — the same
/// reason `audio::engine::start_audio_engine` is exclusive. The steady-state
/// cost with nothing to do is a handful of resource reads and one atomic
/// swap; all allocation (stream, rings, engine, name clones, status
/// strings) happens at event frequency inside the Build/Teardown arms.
///
/// Chained ahead of `analysis::drain_and_analyze` so a teardown or rebuild
/// is observed by the analysis system in the same frame.
pub fn drive_capture(world: &mut World) {
    // Tick the retry cooldown.
    let dt = world.resource::<Time>().delta_secs();
    {
        let mut runtime = world.resource_mut::<CaptureRuntime>();
        if runtime.retry_timer > 0.0 {
            runtime.retry_timer = (runtime.retry_timer - dt).max(0.0);
        }
    }
    // Consume the error flag (a swap, so one error yields one rebuild).
    let error_fired = world
        .get_resource::<AudioInputErrorFlag>()
        .is_some_and(|flag| flag.0.swap(false, std::sync::atomic::Ordering::Relaxed));

    let action = {
        let runtime = world.resource::<CaptureRuntime>();
        decide(&CaptureInputs {
            requested: world.get_resource::<AudioCaptureRequest>(),
            stream_alive: world.get_non_send_resource::<AudioInputStream>().is_some(),
            current_device: runtime.current_device.as_deref(),
            stream_paused: runtime.paused,
            error_fired,
            failed: runtime.failed,
            retry_timer_elapsed: runtime.retry_timer <= 0.0,
        })
    };

    match action {
        CaptureAction::None => {}
        CaptureAction::Pause => {
            if let Some(stream) = world.get_non_send_resource::<AudioInputStream>() {
                stream.pause();
            }
            world.resource_mut::<CaptureRuntime>().paused = true;
        }
        CaptureAction::Resume => {
            if let Some(stream) = world.get_non_send_resource::<AudioInputStream>() {
                stream.play();
            }
            world.resource_mut::<CaptureRuntime>().paused = false;
        }
        CaptureAction::Teardown => teardown_capture(world),
        CaptureAction::Build | CaptureAction::Rebuild => {
            teardown_capture(world);
            // Clone the requested name once, at build frequency. decide()
            // only returns Build/Rebuild when the request exists, and
            // nothing between there and here can remove it (exclusive
            // access), so this read is an invariant, not a race.
            let (device_name, start_paused) = {
                let Some(req) = world.get_resource::<AudioCaptureRequest>() else {
                    return;
                };
                (req.device_name.clone(), req.paused)
            };
            build_capture(world, device_name, start_paused);
        }
    }
}

/// Remove every capture-owned resource and reset the bookkeeping. Safe to
/// call when nothing is running (all removals are remove-if-present).
fn teardown_capture(world: &mut World) {
    let had_stream = world
        .remove_non_send_resource::<AudioInputStream>()
        .is_some();
    world.remove_non_send_resource::<AudioInputRing>();
    world.remove_resource::<AudioInputErrorFlag>();
    world.remove_resource::<AnalysisState>();
    {
        let mut runtime = world.resource_mut::<CaptureRuntime>();
        runtime.current_device = None;
        runtime.paused = false;
        runtime.failed = false;
        runtime.retry_timer = 0.0;
    }
    *world.resource_mut::<AudioInputStatus>() = AudioInputStatus::Inactive;
    if had_stream {
        tracing::info!("audio input capture torn down");
    }
}

/// Build the capture stream and install every capture-owned resource; on
/// failure, record the error and arm the retry cooldown. All allocation in
/// here is at build frequency.
fn build_capture(world: &mut World, device_name: Option<String>, start_paused: bool) {
    match try_build_capture(device_name.as_deref()) {
        Ok(built) => {
            if start_paused {
                built.stream.pause();
            }
            tracing::info!(
                device = %built.resolved_name,
                sample_rate = built.sample_rate,
                channels = built.channels,
                "audio input capture started",
            );
            *world.resource_mut::<AudioInputStatus>() = AudioInputStatus::Running {
                device: built.resolved_name,
                sample_rate: built.sample_rate,
            };
            world.insert_resource(AudioInputErrorFlag(built.error_flag));
            world.insert_resource(AnalysisState(AnalysisEngine::new(built.sample_rate)));
            world.insert_non_send(built.ring);
            world.insert_non_send(built.stream);
            let mut runtime = world.resource_mut::<CaptureRuntime>();
            runtime.current_device = device_name;
            runtime.paused = start_paused;
            runtime.failed = false;
            runtime.retry_timer = 0.0;
        }
        Err(err) => {
            // Spec failure posture: neutral analysis (the drain system sees
            // no ring/engine), diagnostics via status, retry on a cooldown.
            // Never panic, never block, never fall back to a device the
            // operator did not pick.
            tracing::warn!(?err, "audio input capture failed to start; analysis stays neutral");
            *world.resource_mut::<AudioInputStatus>() = AudioInputStatus::Errored {
                message: err.to_string(),
            };
            let mut runtime = world.resource_mut::<CaptureRuntime>();
            runtime.current_device = None;
            runtime.failed = true;
            runtime.retry_timer = RETRY_COOLDOWN_S;
        }
    }
}

/// Everything a successful build hands back to the world-installing side.
struct BuiltCapture {
    stream: AudioInputStream,
    ring: AudioInputRing,
    error_flag: Arc<AtomicBool>,
    sample_rate: u32,
    channels: u16,
    resolved_name: String,
}

/// Why a capture build failed. Event-frequency; formatting allocates, which
/// is fine off the audio thread.
#[derive(Debug, thiserror::Error)]
enum CaptureBuildError {
    #[error("no default input device available")]
    NoDefaultDevice,
    #[error("input device not found: {0}")]
    DeviceNotFound(String),
    #[error("cpal device enumeration error: {0}")]
    Devices(#[from] cpal::DevicesError),
    #[error("cpal default config error: {0}")]
    DefaultConfig(#[from] cpal::DefaultStreamConfigError),
    #[error("cpal stream build error: {0}")]
    BuildStream(#[from] cpal::BuildStreamError),
    #[error("cpal stream play error: {0}")]
    PlayStream(#[from] cpal::PlayStreamError),
    #[error("unsupported input sample format: {0}")]
    UnsupportedFormat(cpal::SampleFormat),
}

/// Resolve the device, size the ring, and build + start the cpal stream.
fn try_build_capture(device_name: Option<&str>) -> Result<BuiltCapture, CaptureBuildError> {
    let host = cpal::default_host();
    let device = match device_name {
        // None = system default input device (pinned contract).
        None => host
            .default_input_device()
            .ok_or(CaptureBuildError::NoDefaultDevice)?,
        // A named device must match exactly; absence is an error (retry
        // path), NOT a fallback to some other open mic.
        Some(name) => host
            .input_devices()?
            .find(|d| d.name().is_ok_and(|n| n == name))
            .ok_or_else(|| CaptureBuildError::DeviceNotFound(name.to_owned()))?,
    };
    let resolved_name = device
        .name()
        .unwrap_or_else(|_| String::from("<unnamed input device>"));
    let supported = device.default_input_config()?;
    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels();
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();

    let (producer, consumer) = rtrb::RingBuffer::<f32>::new(RING_SAMPLE_CAPACITY);
    let error_flag = Arc::new(AtomicBool::new(false));
    let stream = build_typed_stream(
        &device,
        &config,
        sample_format,
        producer,
        Arc::clone(&error_flag),
    )?;
    stream.play()?;

    Ok(BuiltCapture {
        stream: AudioInputStream { stream },
        ring: AudioInputRing::new(consumer),
        error_flag,
        sample_rate,
        channels,
        resolved_name,
    })
}

/// Build the stream for whichever sample format the device natively speaks,
/// converting to f32 in the callback. F32/I16/U16 cover every real backend
/// we target (CoreAudio is F32; WASAPI/ALSA commonly I16); anything exotic
/// errors cleanly into the retry path rather than guessing.
fn build_typed_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    mut producer: rtrb::Producer<f32>,
    error_flag: Arc<AtomicBool>,
) -> Result<cpal::Stream, CaptureBuildError> {
    let channels = usize::from(config.channels);
    // Downmix scale, computed once here so the callback never divides.
    // f32::from(u16) is lossless; max(1) guards a zero-channel config.
    let inv_channels = 1.0 / f32::from(config.channels.max(1));
    // The error callback runs on an OS audio thread: no alloc, no lock, no
    // log — a single relaxed store, observed by drive_capture (the same
    // discipline as the output engine's error callback).
    match sample_format {
        cpal::SampleFormat::F32 => Ok(device.build_input_stream(
            config,
            move |data: &[f32], _info: &cpal::InputCallbackInfo| {
                push_mono(data, channels, inv_channels, &mut producer, |s| s);
            },
            move |_err| {
                error_flag.store(true, std::sync::atomic::Ordering::Relaxed);
            },
            None,
        )?),
        cpal::SampleFormat::I16 => Ok(device.build_input_stream(
            config,
            move |data: &[i16], _info: &cpal::InputCallbackInfo| {
                // i16 -> f32 in [-1, 1): lossless From, scale by 1/32768.
                push_mono(data, channels, inv_channels, &mut producer, |s| {
                    f32::from(s) / 32_768.0
                });
            },
            move |_err| {
                error_flag.store(true, std::sync::atomic::Ordering::Relaxed);
            },
            None,
        )?),
        cpal::SampleFormat::U16 => Ok(device.build_input_stream(
            config,
            move |data: &[u16], _info: &cpal::InputCallbackInfo| {
                // u16 -> f32 in [-1, 1): recenter around 32768 then scale.
                push_mono(data, channels, inv_channels, &mut producer, |s| {
                    (f32::from(s) - 32_768.0) / 32_768.0
                });
            },
            move |_err| {
                error_flag.store(true, std::sync::atomic::Ordering::Relaxed);
            },
            None,
        )?),
        other => Err(CaptureBuildError::UnsupportedFormat(other)),
    }
}

/// The input data callback's entire logic: downmix interleaved frames to
/// mono and push into the ring. Runs on the OS audio thread — no
/// allocation, no locks, no logging. A full ring drops the sample (the
/// `let _ =`): analysis is best-effort and the main thread drains every
/// frame, so sustained fullness only means rendering has stalled longer
/// than the ring covers (~340 ms at 48 kHz).
fn push_mono<T: Copy>(
    data: &[T],
    channels: usize,
    inv_channels: f32,
    producer: &mut rtrb::Producer<f32>,
    convert: impl Fn(T) -> f32,
) {
    if channels == 0 {
        return;
    }
    for frame in data.chunks_exact(channels) {
        let mut sum = 0.0_f32;
        for &s in frame {
            sum += convert(s);
        }
        let _ = producer.push(sum * inv_channels);
    }
}
```

Then in `crates/wc-core/src/audio/input/mod.rs`:

**(a)** Replace the drain registration with the chained pair (single registration — remember the double-register schedule panic):

```rust
            .add_systems(
                PreUpdate,
                (capture::drive_capture, analysis::drain_and_analyze).chain(),
            )
```

and delete the `// Task 8 chains ...` breadcrumb comment.

**(b)** Append the smoke harness at the bottom of the plugin `build` body:

```rust
        // Debug-only manual smoke harness: WC_AUDIO_INPUT_SMOKE=1 requests
        // default-device capture at startup and logs the analysis at 1 Hz,
        // so the full mic -> ring -> analysis path can be exercised with
        // cargo rund before any sketch consumes it (Plan C). The env check
        // happens once, at plugin build — a normal debug run carries no
        // extra systems.
        #[cfg(debug_assertions)]
        if std::env::var_os("WC_AUDIO_INPUT_SMOKE").is_some() {
            app.add_systems(Startup, smoke::insert_smoke_request)
                .add_systems(Update, smoke::log_analysis);
        }
```

**(c)** Append the module at the file footer (above the tests):

```rust
/// Debug-only manual smoke harness (see the plugin body). Not compiled into
/// release; not registered unless WC_AUDIO_INPUT_SMOKE is set.
#[cfg(debug_assertions)]
mod smoke {
    use bevy::prelude::*;

    use super::{AudioAnalysis, AudioCaptureRequest};

    /// Startup system: request default-device capture, as Plan C will.
    pub(super) fn insert_smoke_request(mut commands: Commands<'_, '_>) {
        commands.insert_resource(AudioCaptureRequest {
            device_name: None,
            paused: false,
        });
        tracing::info!("WC_AUDIO_INPUT_SMOKE: default-device capture requested");
    }

    /// Update system: log the analysis snapshot once per second.
    pub(super) fn log_analysis(
        time: Res<'_, Time>,
        analysis: Res<'_, AudioAnalysis>,
        mut accumulated: Local<'_, f32>,
    ) {
        *accumulated += time.delta_secs();
        if *accumulated < 1.0 {
            return;
        }
        *accumulated = 0.0;
        tracing::info!(
            active = analysis.active,
            rms = analysis.rms,
            peak = analysis.peak,
            gain = analysis.gain,
            onset = analysis.onset,
            beat = analysis.beat_confidence,
            bands = ?analysis.bands,
            "audio input analysis",
        );
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p wc-core audio::input` — expected PASS (40 tests).
Then the wiring check: `cargo check -p wc-core` (full clippy/gates come in Task 9).
Reminder: none of these tests inserts `AudioCaptureRequest` into an app with `drive_capture` registered, so no OS input stream is ever opened.

- [ ] **Step 5: Commit**

```
git add crates/wc-core/src/audio/input/capture.rs crates/wc-core/src/audio/input/mod.rs
git commit -m "feat(audio): request-driven cpal input capture with RT-clean callback and retry policy" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 9: Full gate run + docs sweep

**Files:**
- Modify: only if a gate fails (fixups within `crates/wc-core/src/audio/input/` and `crates/wc-core/src/audio/mod.rs`).

**Interfaces:**
- Consumes: everything above.
- Produces: a green CI-equivalent state for the whole Plan A surface.

- [ ] **Step 1: Run the format gate**
Run: `cargo fmt --all` then `cargo fmt --all -- --check`
Expected: clean (the rustfmt.toml nightly-feature warnings are expected on stable and harmless).

- [ ] **Step 2: Run clippy over all targets**
Run: `cargo clippy --all-targets --all-features --workspace -- -D warnings`
Expected: clean. Likely fixup spots if not: pedantic lints in the new test modules (use the house `#[allow(..., reason = ...)]` blocks already specified), `clippy::float_cmp` in tests (add to the sibling-test allow block with a reason if it fires), doc-markdown backtick complaints on rustdoc identifiers.

- [ ] **Step 3: Run the test gates**
Run: `cargo nextest run --workspace --all-features` and `cargo test --doc --workspace`
Expected: all green, including the pre-existing suites (`audio.rs` integration tests, `lib.rs` CorePlugin tests — which now also build `AudioInputPlugin` through `AudioPlugin`).

- [ ] **Step 4: Run the doc, deny, and secrets gates**
Run: `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --document-private-items`, then `cargo deny check`, then `cargo xtask check-secrets`
Expected: clean. Doc-gate watchpoints in this plan's code: every intra-doc `[link]` targets a same-or-higher-visibility item in a default-features build; `AudioInputStatus`/`CaptureRuntime` rustdoc reference `super::AudioCaptureRequest` as plain code spans, not links, where visibility could bite.

- [ ] **Step 5: Self-review against the pinned contracts, then commit any fixups**
Confirm, by reading the final code: `AudioAnalysis` field names/types match the pinned contract (plus the additive `peak`); `AudioCaptureRequest { device_name: Option<String>, paused: bool }` exact; `AvailableAudioInputDevices(pub Vec<String>)` exact with `OPTIONS_KEY = "audio_input_devices"`; `AudioInputPlugin` is added by `AudioPlugin`, not by any sketch. If fixups were needed:

```
git add crates/wc-core/src/audio/input crates/wc-core/src/audio/mod.rs
git commit -m "chore(audio): gate fixups for the audio-input module" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 10: Manual smoke test (OPERATOR-ASSISTED)

**Files:** none (verification only).

**Interfaces:**
- Consumes: the `WC_AUDIO_INPUT_SMOKE` harness (Task 8).
- Produces: human confirmation that a real device drives non-neutral analysis.

This is the only device-dependent verification and it cannot be automated (CI has no input device; agents must not open a live mic silently). Prompt Madison to run it.

- [ ] **Step 1: Prompt the operator to run the smoke build**

Ask Madison to run:

```
WC_AUDIO_INPUT_SMOKE=1 cargo rund
```

(macOS will show the microphone-permission prompt on first run — approve it.)

- [ ] **Step 2: Operator checks, watching the 1 Hz "audio input analysis" log lines**
1. **Startup:** an "audio input devices enumerated" line lists the machine's input devices, and an "audio input capture started" line names the default device and its sample rate.
2. **Silence:** rms/bands sit near zero, active = true, gain climbs toward its clamp (slowly — release is 4 s).
3. **Speak or play music:** rms rises to roughly 0.2–0.3 (AGC target) within a few seconds regardless of how loud the source is; bands light up in the right registers (voice ≈ bands 2–4; bass music ≈ bands 0–1); onset spikes and beat snaps to 1.0 on percussive hits, roughly on the beat.
4. **Pause behavior is Plan C's to exercise** (nothing toggles `paused` yet) — skip.
5. **Failure posture (optional, kiosk bar):** unplug/replug a USB mic set as system default mid-run; expect a warn log, status errored, neutral analysis, and recovery within ~2 s of the device returning (the retry cooldown).
6. Quit; confirm no panic on shutdown (stream drop path).

- [ ] **Step 3: Record the outcome**
If anything reads wrong (AGC pumping, dead bands, onset too twitchy), the tuning consts at the top of `analysis.rs` are the knobs — file a follow-up rather than hand-tuning mid-plan unless the behavior is broken outright. Analysis tuning against real music is expected follow-up work for Plan C integration, same as the pending Line hand-audio ear-tune.

---

## Spec coverage self-review (Unit A checklist)

| Spec requirement | Where |
| --- | --- |
| cpal input stream as non-send resource, `!Send` on macOS | Task 8 `AudioInputStream` |
| RT-clean callback: pre-allocated only, rtrb push, `Arc<AtomicBool>` error flag | Task 8 `push_mono` + `build_typed_stream` (error closures) |
| PreUpdate drain into pre-allocated circular buffer | Task 6 `drain_and_analyze` + Task 3 `history` |
| RMS + peak | Task 3 |
| Slow asymmetric AGC (mic is the bar) | Task 2 `Agc` |
| ~8 log-spaced bands, windowed FFT via fundsp, post-AGC | Task 4 (gain applied to samples pre-FFT) |
| Spectral-flux onset + debounced beat flag | Task 5 |
| Window/hop keeps up with 60 Hz drain; buffers init-allocated | Tasks 3/6 (HISTORY_LEN 4096 vs 800/frame; RING 16384) |
| `Res<AudioAnalysis>` pinned shape | Task 1 (+ additive `peak`) |
| Neutral when inactive/failed; never panic/block | Tasks 6 (drain guards) + 8 (build failure path) |
| `AudioCaptureRequest` insert/remove/pause contract, per-frame reaction, rebuild on device change | Task 8 `decide`/`drive_capture` |
| `AvailableAudioInputDevices` + `register_runtime_enum_options` under `"audio_input_devices"` | Task 7 |
| Plugin added by core audio plumbing | Task 1 (inside `AudioPlugin::build`) |
| Rustdoc `//!` + `///` throughout | every task's code |
| Spec test list: AGC step convergence; tone bands; click-train onset; ring drain under pressure | Tasks 2 / 4 / 5 / 6 respectively, all device-free |
| Manual `cargo rund` smoke | Task 10 (operator-assisted) |

Deliberately deferred to Plan C (per the pinned contracts): inserting/removing the request on Radiance enter/exit, driving `paused` from `SketchActivity`, the `RadianceSettings` RuntimeEnum field + `requires_restart` wiring, and diagnostics-panel display of `AudioInputStatus`.
